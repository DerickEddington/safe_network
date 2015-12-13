// Copyright 2015 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under (1) the MaidSafe.net Commercial License,
// version 1.0 or later, or (2) The General Public License (GPL), version 3, depending on which
// licence you accepted on initial access to the Software (the "Licences").
//
// By contributing code to the SAFE Network Software, or to this project generally, you agree to be
// bound by the terms of the MaidSafe Contributor Agreement, version 1.0.  This, along with the
// Licenses can be found in the root directory of this project at LICENSE, COPYING and CONTRIBUTOR.
//
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.
//
// Please review the Licences for the specific language governing permissions and limitations
// relating to use of the SAFE Network Software.

use itertools::Itertools;
use crust;
use std::fmt::{Debug, Formatter, Error};
use event::Event;
use action::Action;
use xor_name::XorName;
use sodiumoxide::crypto::{box_, sign, hash};
use id::{FullId, PublicId};
use lru_time_cache::LruCache;
use error::{RoutingError, ResponseError};
use authority::Authority;
use kademlia_routing_table::{RoutingTable, NodeInfo};
use maidsafe_utilities::serialisation::{serialise, deserialise};
use data::{Data, DataRequest};
use messages::{DirectMessage, HopMessage, SignedMessage, RoutingMessage, RequestMessage,
               ResponseMessage, RequestContent, ResponseContent, Message, GetResultType,
               APIResultType};
use utils;

const MAX_RELAYS: usize = 100;
const ROUTING_NODE_THREAD_NAME: &'static str = "RoutingNodeThread";
const CRUST_DEFAULT_BEACON_PORT: u16 = 5484;
const CRUST_DEFAULT_TCP_ACCEPTING_PORT: ::crust::Port = ::crust::Port::Tcp(5483);
const CRUST_DEFAULT_UTP_ACCEPTING_PORT: ::crust::Port = ::crust::Port::Utp(5483);

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
enum State {
    Disconnected,
    // Transition state while validating proxy node
    Bootstrapping,
    // We are Bootstrapped
    Client,
    // We have been Relocated and now a node
    Node,
}

/// Routing Node
pub struct RoutingNode {
    // for CRUST
    crust_service: ::crust::Service,
    accepting_on: Vec<::crust::Endpoint>,
    // for RoutingNode
    client_restriction: bool,
    crust_rx: ::std::sync::mpsc::Receiver<::crust::Event>,
    action_rx: ::std::sync::mpsc::Receiver<Action>,
    event_sender: ::std::sync::mpsc::Sender<Event>,
    signed_message_filter: ::message_filter::MessageFilter<::messages::SignedMessage>,
    connection_filter: ::message_filter::MessageFilter<::XorName>,
    node_id_cache: LruCache<XorName, PublicId>,
    message_accumulator: ::accumulator::Accumulator<RoutingMessage, sign::PublicKey>,
    refresh_accumulator: ::refresh_accumulator::RefreshAccumulator,
    refresh_causes: ::message_filter::MessageFilter<::XorName>,
    // Group messages which have been accumulated and then actioned
    grp_msg_filter: ::message_filter::MessageFilter<RoutingMessage>,
    // cache_options: ::data_cache_options::DataCacheOptions,
    full_id: FullId,
    state: State,
    routing_table: RoutingTable<::id::PublicId, ::crust::Connection>,
    // our bootstrap connections
    proxy_map: ::std::collections::HashMap<::crust::Connection, PublicId>,
    // any clients we have proxying through us
    client_map: ::std::collections::HashMap<sign::PublicKey, ::crust::Connection>,
    data_cache: LruCache<XorName, Data>,
}

impl RoutingNode {
    pub fn new(event_sender: ::std::sync::mpsc::Sender<Event>,
               client_restriction: bool,
               keys: Option<FullId>)
               -> Result<(::types::RoutingActionSender,
                          ::maidsafe_utilities::thread::RaiiThreadJoiner),
                         RoutingError> {
        let (crust_tx, crust_rx) = ::std::sync::mpsc::channel();
        let (action_tx, action_rx) = ::std::sync::mpsc::channel();
        let (category_tx, category_rx) = ::std::sync::mpsc::channel();

        let routing_event_category =
            ::maidsafe_utilities::event_sender::MaidSafeEventCategory::RoutingEvent;
        let action_sender = ::types::RoutingActionSender::new(action_tx,
                                                              routing_event_category,
                                                              category_tx.clone());

        let crust_event_category =
            ::maidsafe_utilities::event_sender::MaidSafeEventCategory::CrustEvent;
        let crust_sender = ::crust::CrustEventSender::new(crust_tx,
                                                          crust_event_category,
                                                          category_tx);

        let crust_service = match ::crust::Service::new(crust_sender) {
            Ok(service) => service,
            Err(what) => panic!(format!("Unable to start crust::Service {}", what)),
        };

        let full_id = match keys {
            Some(full_id) => full_id,
            None => FullId::new(),
        };
        let our_name = *full_id.public_id().name();

        let joiner = thread!(ROUTING_NODE_THREAD_NAME, move || {
            let mut routing_node = RoutingNode {
                crust_service: crust_service,
                accepting_on: vec![],
            // Counter starts at 1, 0 is reserved for bootstrapping.
                client_restriction: client_restriction,
                crust_rx: crust_rx,
                action_rx: action_rx,
                event_sender: event_sender,
                signed_message_filter: ::message_filter
                                       ::MessageFilter
                                       ::with_expiry_duration(::time::Duration::minutes(20)),
                connection_filter: ::message_filter::MessageFilter::with_expiry_duration(
                    ::time::Duration::seconds(20)),
                node_id_cache: LruCache::with_expiry_duration(::time::Duration::minutes(10)),
                message_accumulator: ::accumulator::Accumulator::with_duration(1,
                    ::time::Duration::minutes(5)),
                refresh_accumulator:
                    ::refresh_accumulator::RefreshAccumulator::with_expiry_duration(
                        ::time::Duration::minutes(5)),
                refresh_causes: ::message_filter::MessageFilter::with_expiry_duration(
                    ::time::Duration::minutes(5)),
                grp_msg_filter: ::message_filter::MessageFilter::with_expiry_duration(
                    ::time::Duration::minutes(20)),
            // cache_options: ::data_cache_options::DataCacheOptions::new(),
                full_id: full_id,
                state: State::Disconnected,
                routing_table: RoutingTable::new(&our_name),
                proxy_map: ::std::collections::HashMap::new(),
                client_map: ::std::collections::HashMap::new(),
                data_cache: LruCache::with_expiry_duration(::time::Duration::minutes(10)),
            };

            routing_node.run(category_rx);

            debug!("Exiting thread {:?}", ROUTING_NODE_THREAD_NAME);
        });

        Ok((action_sender,
            ::maidsafe_utilities::thread::RaiiThreadJoiner::new(joiner)))
    }

    pub fn run(&mut self,
               category_rx: ::std::sync::mpsc::Receiver<
                   ::maidsafe_utilities::event_sender::MaidSafeEventCategory>) {
        self.crust_service.bootstrap(0u32, Some(CRUST_DEFAULT_BEACON_PORT));
        for it in category_rx.iter() {
            if self.state == State::Node {
                trace!("Routing Table size: {}", self.routing_table.len());
            };
            match it {
                ::maidsafe_utilities::event_sender::MaidSafeEventCategory::RoutingEvent => {
                    if let Ok(action) = self.action_rx.try_recv() {
                        match action {
                            Action::SendContent(src, dst, content) => {
                                let _ = self.send_content(src, dst, content);
                            }
                            Action::ClientSendContent(dst, content) => {
                                self.client_send_content(dst, content);
                            }
                            Action::Terminate => {
                                let _ = self.event_sender.send(Event::Terminated);
                                self.crust_service.stop();
                                break;
                            }
                        }
                    }
                }
                ::maidsafe_utilities::event_sender::MaidSafeEventCategory::CrustEvent => {
                    if let Ok(crust_event) = self.crust_rx.try_recv() {
                        match crust_event {
                            ::crust::Event::BootstrapFinished => self.handle_bootstrap_finished(),
                            ::crust::Event::OnAccept(connection) => {
                                self.handle_on_accept(connection)
                            }

                            // TODO (Fraser) This needs to restart if we are left with 0 connections
                            ::crust::Event::LostConnection(connection) => {
                                self.handle_lost_connection(connection)
                            }

                            ::crust::Event::NewMessage(connection, bytes) => {
                                let _ = self.handle_new_message(connection, bytes);
                                // FIXME(dirvine) handle error  :12/12/2015
                            }
                            ::crust::Event::OnConnect(connection, connection_token) => {
                                self.handle_on_connect(connection, connection_token)
                            }
                            ::crust::Event::ExternalEndpoints(external_endpoints) => {
                                for external_endpoint in external_endpoints {
                                    debug!("Adding external endpoint {:?}", external_endpoint);
                                    self.accepting_on.push(external_endpoint);
                                }
                            }
                            ::crust::Event::OnHolePunched(_hole_punch_result) => unimplemented!(),
                            ::crust::Event::OnUdpSocketMapped(_mapped_udp_socket) => unimplemented!(),
                            ::crust::Event::OnRendezvousConnect(_connection, _signed_request) => unimplemented!(),
                        }
                    }
                }
            } // Category Match
        } // Category Rx
    }

    fn handle_new_message(&mut self,
                          connection: ::crust::Connection,
                          bytes: Vec<u8>)
                          -> Result<(), RoutingError> {
        match deserialise(&bytes) {
            Ok(Message::HopMessage(ref hop_msg)) => self.handle_hop_message(hop_msg, connection),
            Ok(Message::DirectMessage(direct_msg)) => {
                self.handle_direct_message(direct_msg, connection)
            },
            Err(error) => Err(RoutingError::SerialisationError(error))
        }
    }

    fn handle_hop_message(&mut self,
                          hop_msg: &HopMessage,
                          connection: ::crust::Connection)
                          -> Result<(), RoutingError> {
        if self.state == State::Node {
            if let Some(&NodeInfo { ref public_id, ..}) = self.routing_table.get(hop_msg.name()) {
                try!(hop_msg.verify(public_id.signing_public_key()));
            } else if let Some((ref pub_key, _)) = self.client_map.iter().find(|ref elt| &connection == elt.1) {
                try!(hop_msg.verify(pub_key));
            } else {
                // TODO drop connection ?
                return Err(RoutingError::UnknownConnection);
            }
        } else if self.state == State::Client {
            if let Some(pub_id) = self.proxy_map.get(&connection) {
                try!(hop_msg.verify(pub_id.signing_public_key()));
            }
        } else {
            return Err(RoutingError::InvalidStateForOperation);
        }

        let (content, name) = hop_msg.extract();
        self.handle_signed_message(content, name, connection)
    }

    fn handle_signed_message(&mut self,
                             signed_msg: SignedMessage,
                             hop_name: XorName,
                             connection: ::crust::Connection)
                             -> Result<(), RoutingError> {
        try!(signed_msg.check_integrity());

        // Prevents
        // 1) someone sending messages repeatedly to us
        // 2) swarm messages generated by us reaching us again
        if self.signed_message_filter.contains(&signed_msg) {
            return Err(RoutingError::FilterCheckFailed);
        }
        let _ = self.signed_message_filter.insert(signed_msg.clone());

        // Either swarm or Direction check
        if self.state == State::Node {
            if self.routing_table.is_close(signed_msg.content().dst().get_name()) {
                self.signed_msg_security_check(&signed_msg);

                if signed_msg.content().dst().is_group() {
                    self.send(signed_msg.clone()); // Swarm
                } else if self.full_id.public_id().name() != signed_msg.content().dst().get_name() {
                    // TODO See if this puts caching into disadvantage
                    // Incoming msg is in our range and not for a group and also not for us, thus
                    // sending on and bailing out
                    return self.send(signed_msg.clone());
                } else if let Authority::Client { ref client_key, .. } = *signed_msg.content().dst() {
                    // TODO Relay to client
                    return self.relay_to_client(signed_msg.clone());
                }
            } else if !::xor_name::closer_to_target(self.full_id.public_id().name(),
                                             &hop_name,
                                             signed_msg.content().dst().get_name()) {
                return Err(RoutingError::DirectionCheckFailed);
            }

            // Cache handling
            if let Some(data) = self.get_from_cache(signed_msg.content()) {
                let content = ResponseContent::Get { result: GetResultType::Success(data.clone()) };

                let response_msg = ResponseMessage {
                    src: Authority::ManagedNode(self.full_id.public_id().name().clone()),
                    dst: signed_msg.content().src().clone(),
                    content: content,
                };

                let routing_msg = RoutingMessage::Response(response_msg);
                let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

                return self.send(signed_msg.clone());
            }

            self.add_to_cache(signed_msg.content());

            // Forwarding the message not meant for us (transit)
            if !self.routing_table.is_close(signed_msg.content().dst().get_name()) {
                return self.send(signed_msg.clone());
            }
        } else if self.state == State::Client {
            match *signed_msg.content().dst() {
                Authority::Client { ref client_key, .. } => {
                    if self.full_id.public_id().signing_public_key() != client_key {
                        return Err(RoutingError::BadAuthority);
                    }
                }
                _ => return Err(RoutingError::BadAuthority),
            }
        } else {
            return Err(RoutingError::InvalidStateForOperation);
        }

        // Hereafter this msg is for us
        self.handle_routing_message(signed_msg.content().clone(), signed_msg.public_id().clone())
    }

    fn signed_msg_security_check(&self, signed_msg: &SignedMessage) -> Result<(), RoutingError> {
        if signed_msg.content().src().is_group() {
            // TODO validate unconfirmed node that belongs to the src group
            if !self.routing_table.try_confirm_safe_group_distance(signed_msg.content().src().get_name(),
                                                                   signed_msg.public_id().name()) {
                return Err(RoutingError::RoutingTableBucketIndexFailed);
            };
            Ok(())
        } else {
            match (signed_msg.content().dst(), signed_msg.content().src()) {
                (& Authority::NodeManager(manager_name),
                 & Authority::ManagedNode(node_name)) => {
                    // TODO confirm sender is in our routing table
                    unimplemented!();
                }
                // Security validation if came from a Client: This validation ensures that the
                // source authority matches the signed message's public_id. This prevents cases
                // where attacker can provide a fake SignedMessage wrapper over somebody else's
                // (Client's) RoutingMessage.
                (_, &Authority::Client { ref client_key, .. }) => {
                    if client_key != signed_msg.public_id().signing_public_key() {
                        return Err(RoutingError::FailedSignature);
                    };
                    return Ok(());
                },
                _ => Ok(()),
            }
        }
    }

    fn get_from_cache(&self, routing_msg: &RoutingMessage) -> Option<&Data> {
        match *routing_msg {
            RoutingMessage::Request(RequestMessage {
                    content: RequestContent::Get(DataRequest::ImmutableData(ref name, _)),
                    ..
                }) => {
                self.data_cache.get(name)
            }
            _ => None,
        }
    }

    fn add_to_cache(&self, routing_msg: &RoutingMessage) {
        match *routing_msg {
            RoutingMessage::Response(ResponseMessage {
                    content: ResponseContent::Get { result: GetResultType::Success(ref data @ Data::ImmutableData(_)), },
                    ..
                }) => {
                let _ = self.data_cache.insert(data.name().clone(), data.clone());
            }
            _ => (),
        }
    }

    // Needs to be commented
    fn handle_routing_message(&mut self,
                              mut routing_msg: RoutingMessage,
                              public_id: PublicId)
                              -> Result<(), RoutingError> {
        if self.grp_msg_filter.contains(&routing_msg) {
            return Err(RoutingError::FilterCheckFailed);
        }

        // TODO Node Harvest here ??
        if routing_msg.src().is_group() {
            if let Some(output_msg) =  self.accumulate(routing_msg, &public_id) {
                   let _ = self.grp_msg_filter.insert(output_msg.clone());
            } else {
                return Err(::error::RoutingError::NotEnoughSignatures);
            }
        }

        match routing_msg {
            RoutingMessage::Request(msg) => self.handle_request_message(msg),
            RoutingMessage::Response(msg) => self.handle_response_message(msg),
        }
    }

    fn accumulate(&mut self,
                  message: ::messages::RoutingMessage,
                  public_id: &PublicId)
                  -> Option<RoutingMessage> {
        // For clients we already have set it on reception of BootstrapIdentify message
        if self.state == State::Node {
            self.message_accumulator.set_quorum_size(self.routing_table.dynamic_quorum_size());
        }

        if self.message_accumulator.add(message.clone(), public_id.signing_public_key().clone()).is_some() {
            Some(message)
        } else {
            None
        }
    }

    fn handle_request_message(&mut self, request_msg: RequestMessage) -> Result<(), RoutingError> {
        match (request_msg.content, request_msg.src, request_msg.dst) {
            (RequestContent::GetNetworkName { current_id, },
             Authority::Client { client_key, proxy_node_name },
             Authority::NaeManager(dst_name)) => {
                self.handle_get_network_name_request(current_id,
                                                     client_key,
                                                     proxy_node_name,
                                                     dst_name)
            }
            (RequestContent::ExpectCloseNode { expect_id, },
             Authority::NaeManager(_),
             Authority::NodeManager(_)) => self.handle_expect_close_node_request(expect_id),
            (RequestContent::GetCloseGroup,
             Authority::Client { client_key, proxy_node_name, },
             Authority::NodeManager(dst_name)) => {
                self.handle_get_close_group_request(client_key, proxy_node_name, dst_name)
            }
            (RequestContent::Endpoints { encrypted_endpoints, nonce_bytes },
             Authority::Client { client_key, proxy_node_name, },
             Authority::ManagedNode(dst_name)) => {
                self.handle_endpoints_from_client(encrypted_endpoints,
                                                  nonce_bytes,
                                                  client_key,
                                                  proxy_node_name,
                                                  dst_name)
            }
            (RequestContent::Endpoints { encrypted_endpoints, nonce_bytes },
             Authority::ManagedNode(src_name),
             Authority::Client { .. }) |
            (RequestContent::Endpoints { encrypted_endpoints, nonce_bytes },
             Authority::ManagedNode(src_name),
             Authority::ManagedNode(_)) => {
                self.handle_endpoints_from_node(encrypted_endpoints,
                                                nonce_bytes,
                                                src_name,
                                                request_msg.dst)
            }
            (RequestContent::Connect,
             Authority::ManagedNode(src_name),
             Authority::ManagedNode(dst_name)) => self.handle_connect_request(src_name, dst_name),
            (RequestContent::GetPublicId,
             Authority::ManagedNode(src_name),
             Authority::NodeManager(dst_name)) => self.handle_get_public_id(src_name, dst_name),
            (RequestContent::GetPublicIdWithEndpoints { encrypted_endpoints, nonce_bytes, },
             Authority::ManagedNode(src_name),
             Authority::NodeManager(dst_name)) => {
                self.handle_get_public_id_with_endpoints(encrypted_endpoints,
                                                         nonce_bytes,
                                                         src_name,
                                                         dst_name)
            }
            (RequestContent::Get(_), _, _) |
            (RequestContent::Put(_), _, _) |
            (RequestContent::Post(_), _, _) |
            (RequestContent::Delete(_), _, _) => {
                let event = Event::Request {
                    content: request_msg.content,
                    src: request_msg.src,
                    dst: request_msg.dst,
                };

                let _ = self.event_sender.send(event);
                Ok(())
            }
            _ => {
                warn!("Unhandled request - Message {:?}", request_msg);
                Err(RoutingError::BadAuthority)
            }
            // RequestContent::Refresh { type_tag, message, cause, } => {
            //     if accumulated_message.source_authority.is_group() {
            //         self.handle_refresh(type_tag,
            //                             accumulated_message.source_authority
            //                                                .get_location()
            //                                                .clone(),
            //                             message,
            //                             accumulated_message.destination_authority,
            //                             cause)
            //     } else {
            //         return Err(RoutingError::BadAuthority);
            //     }
            // }
        }
    }

    fn handle_response_message(&mut self,
                               response_msg: ResponseMessage)
                               -> Result<(), RoutingError> {
        match (response_msg.content, response_msg.src, response_msg.dst) {
            (ResponseContent::GetNetworkName { relocated_id, },
             Authority::NaeManager(_),
             Authority::Client { client_key, proxy_node_name, }) => {
                self.handle_get_network_name_response(relocated_id, client_key, proxy_node_name)
            }
            (ResponseContent::GetPublicId { public_id, },
             Authority::NodeManager(_),
             Authority::ManagedNode(dst_name)) => {
                self.handle_get_public_id_response(public_id, dst_name)
            }
            (ResponseContent::GetPublicIdWithEndpoints { public_id, encrypted_endpoints, nonce_bytes },
             Authority::NodeManager(_),
             Authority::ManagedNode(dst_name)) => {
                self.handle_get_public_id_with_endpoints_response(public_id, encrypted_endpoints, nonce_bytes, dst_name)
            }
            (ResponseContent::GetCloseGroup { close_group_ids },
             Authority::NodeManager(_),
             Authority::Client { client_key, proxy_node_name, }) => {
                self.handle_get_close_group_response(close_group_ids, client_key, proxy_node_name)
            }
            (ResponseContent::Get{..}, _, _) |
            (ResponseContent::Put{..}, _, _) |
            (ResponseContent::Post{..}, _, _) |
            (ResponseContent::Delete{..}, _, _) => {
                let event = Event::Response {
                    content: response_msg.content,
                    src: response_msg.src,
                    dst: response_msg.dst,
                };

                let _ = self.event_sender.send(event);
                Ok(())
            }
            _ => {
                warn!("Unhandled response - Message {:?}", response_msg);
                Err(RoutingError::BadAuthority)
            }
        }
    }

    fn handle_bootstrap_finished(&mut self) {
        debug!("Finished bootstrapping.");
        // If we have no connections, we should start listening to allow incoming connections
        if self.state == State::Disconnected {
            debug!("Bootstrap finished with no connections. Start Listening to allow incoming \
                    connections.");
            self.start_listening();
        }
    }

    fn start_listening(&mut self) {
        match self.crust_service.start_beacon(CRUST_DEFAULT_BEACON_PORT) {
            Ok(port) => info!("Running Crust beacon listener on port {}", port),
            Err(error) => {
                warn!("Crust beacon failed to listen on port {}: {:?}",
                      CRUST_DEFAULT_BEACON_PORT,
                      error)
            }
        }
        match self.crust_service.start_accepting(CRUST_DEFAULT_TCP_ACCEPTING_PORT) {
            Ok(endpoint) => {
                info!("Running TCP listener on {:?}", endpoint);
                self.accepting_on.push(endpoint);
            }
            Err(error) => {
                warn!("Failed to listen on {:?}: {:?}",
                      CRUST_DEFAULT_TCP_ACCEPTING_PORT,
                      error)
            }
        }
        match self.crust_service.start_accepting(CRUST_DEFAULT_UTP_ACCEPTING_PORT) {
            Ok(endpoint) => {
                info!("Running uTP listener on {:?}", endpoint);
                self.accepting_on.push(endpoint);
            }
            Err(error) => {
                warn!("Failed to listen on {:?}: {:?}",
                      CRUST_DEFAULT_UTP_ACCEPTING_PORT,
                      error)
            }
        }

        // The above commands will give us only internal endpoints on which we're accepting. The
        // next command will try to find external endpoints. The result shall be returned async
        // through the Crust::ExternalEndpoints event.
        self.crust_service.get_external_endpoints();
    }

    fn handle_on_connect(&mut self,
                         connection: ::std::io::Result<::crust::Connection>,
                         connection_token: u32) {
        match connection {
            Ok(connection) => {
                debug!("New connection via OnConnect {:?} with token {}",
                       connection,
                       connection_token);
                if self.state == State::Disconnected {
                    // Established connection. Pending Validity checks
                    self.state = State::Bootstrapping;
                    let _ = self.client_identify(connection);
                    return;
                }

                let _ = self.node_identify(connection);
            }
            Err(error) => {
                warn!("Failed to make connection with token {} - {}",
                      connection_token,
                      error);
            }
        }
    }

    fn handle_on_accept(&mut self, connection: ::crust::Connection) {
        debug!("New connection via OnAccept {:?}", connection);
        if self.state == State::Disconnected {
            // I am the first node in the network, and I got an incoming connection so I'll
            // promote myself as a node.
            let new_name = XorName::new(hash::sha512::hash(&self.full_id
                                                                .public_id()
                                                                .name()
                                                                .0)
                                            .0);

            // This will give me a new RT and set state to Relocated
            self.set_self_node_name(new_name);
            self.state = State::Node;
        }
    }

    /// When CRUST reports a lost connection, ensure we remove the endpoint everywhere
    fn handle_lost_connection(&mut self, connection: ::crust::Connection) {
        debug!("Lost connection on {:?}", connection);
        self.dropped_routing_node_connection(&connection);
        self.dropped_client_connection(&connection);
        self.dropped_bootstrap_connection(&connection);
    }

    fn bootstrap_identify(&mut self, connection: ::crust::Connection) -> Result<(), RoutingError> {
        let direct_message = ::direct_messages::DirectMessage::BootstrapIdentify {
            public_id: self.full_id.public_id().clone(),
            // Current quorum size should also include ourselves when sending this message. Thus
            // the '+ 1'
            current_quorum_size: self.routing_table.dynamic_quorum_size() + 1,
        };
        // TODO impl convert trait for RoutingError
        let bytes = try!(::maidsafe_utilities::serialisation::serialise(&direct_message));

        Ok(self.crust_service.send(connection, bytes))
    }

    fn client_identify(&mut self, connection: ::crust::Connection) -> Result<(), RoutingError> {
        let serialised_public_id =
            try!(::maidsafe_utilities::serialisation::serialise(self.full_id.public_id()));
        let signature = sign::sign_detached(&serialised_public_id,
                                            self.full_id
                                                .signing_private_key());

        let direct_message = ::direct_messages::DirectMessage::ClientIdentify {
            serialised_public_id: serialised_public_id,
            signature: signature,
        };
        let bytes = try!(::maidsafe_utilities::serialisation::serialise(&direct_message));

        Ok(self.crust_service.send(connection, bytes))
    }

    fn node_identify(&mut self, connection: ::crust::Connection) -> Result<(), RoutingError> {
        let serialised_public_id =
            try!(::maidsafe_utilities::serialisation::serialise(self.full_id.public_id()));
        let signature = sign::sign_detached(&serialised_public_id,
                                            self.full_id
                                                .signing_private_key());

        let direct_message = ::direct_messages::DirectMessage::NodeIdentify {
            serialised_public_id: serialised_public_id,
            signature: signature,
        };
        let bytes = try!(::maidsafe_utilities::serialisation::serialise(&direct_message));

        Ok(self.crust_service.send(connection, bytes))
    }

    // ---- Direct Messages -----------------------------------------------------------------------
    fn verify_signed_public_id(serialised_public_id: &[u8],
                               signature: &sign::Signature)
                               -> Result<::id::PublicId, RoutingError> {
        let public_id: ::id::PublicId =
            try!(::maidsafe_utilities::serialisation::deserialise(serialised_public_id));
        if sign::verify_detached(signature,
                                 serialised_public_id,
                                 public_id.signing_public_key()) {
            Ok(public_id)
        } else {
            Err(RoutingError::FailedSignature)
        }
    }

    fn handle_direct_message(&mut self,
                             direct_message: DirectMessage,
                             connection: ::crust::Connection)
                             -> Result<(), RoutingError> {
        match direct_message {
            ::messages::DirectMessage::BootstrapIdentify { ref public_id, current_quorum_size } => {
                if *public_id.name() == ::XorName::new(::sodiumoxide
                                                        ::crypto
                                                        ::hash::sha512::hash(&public_id.signing_public_key().0).0) {
                    warn!("Incoming Connection not validated as a proper node - dropping");
                    self.crust_service.drop_node(connection);

                    // Probably look for other bootstrap connections
                    return Ok(())
                }

                if let Some(previous_name) = self.proxy_map.insert(connection, *public_id) {
                    warn!("Adding bootstrap node to proxy map caused a prior id to eject. \
                          Previous name: {:?}", previous_name);
                    warn!("Dropping this connection {:?}", connection);
                    self.crust_service.drop_node(connection);
                    let _ = self.proxy_map.remove(&connection);

                    // Probably look for other bootstrap connections
                    return Ok(());
                }

                self.state = State::Client;
                self.message_accumulator.set_quorum_size(current_quorum_size);

                // Only if we started as a client but eventually want to be a node
                if self.client_restriction {
                    let _ = self.event_sender.send(Event::Connected);
                } else {
                    self.relocate();
                };
                Ok(())
            }
            ::messages::DirectMessage::ClientIdentify { ref serialised_public_id, ref signature } => {
                let public_id = match RoutingNode::verify_signed_public_id(serialised_public_id, signature) {
                    Ok(public_id) => public_id,
                    Err(error) => {
                        warn!("Signature check failed in NodeIdentify - Dropping connection {:?}",
                              connection);
                        self.crust_service.drop_node(connection);

                        return Ok(());
                    },
                };

                if *public_id.name() != ::XorName::new(::sodiumoxide
                                                        ::crypto
                                                        ::hash::sha512::hash(&public_id.signing_public_key().0).0) {
                    warn!("Incoming Connection not validated as a proper client - dropping");
                    self.crust_service.drop_node(connection);
                    return Ok(());
                }

                if let Some(prev_conn) = self.client_map.insert(public_id.signing_public_key().clone(), connection) {
                    debug!("Found previous connection against client key - Dropping {:?}",
                           prev_conn);
                    self.crust_service.drop_node(prev_conn);
                }

                let _ = self.bootstrap_identify(connection);
                return Ok(());
            }
            ::messages::DirectMessage::NodeIdentify { ref serialised_public_id, ref signature } => {
                let public_id = match RoutingNode::verify_signed_public_id(serialised_public_id, signature) {
                    Ok(public_id) => public_id,
                    Err(error) => {
                        warn!("Signature check failed in NodeIdentify - Dropping connection {:?}",
                              connection);
                        self.crust_service.drop_node(connection);

                        return Ok(());
                    }
                };

                if let Some(their_public_id) = self.node_id_cache.get(public_id.name()).cloned() {
                    if their_public_id != public_id {
                        warn!("Given Public ID and Public ID in cache don't match - Given {:?} :: In cache {:?} \
                               Dropping connection {:?}", public_id, their_public_id, connection);

                        self.crust_service.drop_node(connection);
                        return Ok(());
                    }

                    let node_info = ::kademlia_routing_table::NodeInfo::new(public_id.clone(), vec![connection]);
                    if let Some(_) = self.routing_table.get(public_id.name()) {
                        if !self.routing_table.add_connection(public_id.name(), connection) {
                            // We already sent an identify down this connection
                            return Ok(());
                        }
                    } else {
                        let (is_added, node_removed) = self.routing_table.add_node(node_info);

                        if !is_added {
                            debug!("Node rejected by Routing table - Closing {:?}", connection);
                            self.crust_service.drop_node(connection);
                            let _ = self.node_id_cache.remove(public_id.name());

                            return Ok(());
                        }

                        if let Some(node_to_drop) = node_removed {
                            debug!("Node ejected by routing table on an add. Dropping node {:?}",
                                   node_to_drop);

                            for it in node_to_drop.connections.into_iter() {
                                self.crust_service.drop_node(it);
                            }
                        }
                    }

                    let _ = self.node_identify(connection);
                    return Ok(());
                } else {
                    debug!("PublicId not found in node_id_cache - Dropping Connection {:?}", connection);
                    self.crust_service.drop_node(connection);
                    return Ok(());
                }
            }
            ::messages::DirectMessage::Churn { ref close_group } => {
                // Message needs signature validation
                self.handle_churn(close_group);
                return Ok(());
            }
        }
    }

    fn handle_churn(&mut self, close_group: &[::XorName]) -> Result<(), RoutingError> {
        for close_node in close_group {
            if self.connection_filter.contains(close_node) {
                return Err(RoutingError::FilterCheckFailed);
            }
            let _ = self.connection_filter.insert(close_node.clone());

            if !self.routing_table.want_to_add(close_node) {
                return Ok(());
            }

            try!(self.send_connect_request(close_node))
        }
        Ok(())
    }

    // Constructed by A; From A -> X
    fn relocate(&mut self) -> Result<(), RoutingError> {
        let request_content = RequestContent::GetNetworkName {
            current_id: self.full_id.public_id().clone(),
        };

        let request_msg = RequestMessage {
            src: try!(self.get_client_authority()),
            dst: Authority::NaeManager(*self.full_id.public_id().name()),
            content: request_content,
        };

        let routing_msg = RoutingMessage::Request(request_msg);

        let signed_message = try!(SignedMessage::new(routing_msg, &self.full_id));

        self.send(signed_message)
    }

    // Received by X; From A -> X
    fn handle_get_network_name_request(&mut self,
                                       mut their_public_id: PublicId,
                                       client_key: sign::PublicKey,
                                       proxy_name: XorName,
                                       dst_name: XorName)
                                       -> Result<(), RoutingError> {
        let hashed_key = hash::sha512::hash(&client_key.0);
        let close_group_to_client = XorName::new(hashed_key.0);

        // Validate Client (relocating node) has contacted the correct Group-X
        if close_group_to_client != dst_name {
            return Err(RoutingError::InvalidDestination);
        }

        let mut close_group = self.routing_table
                                  .our_close_group()
                                  .iter()
                                  .map(|node_info| node_info.public_id.name().clone())
                                  .collect_vec();
        close_group.push(*self.full_id.public_id().name());

        let relocated_name = try!(utils::calculate_relocated_name(close_group,
                                                                  &their_public_id.name()));

        their_public_id.set_name(relocated_name.clone());

        // From X -> A (via B)
        {
            let response_content = ResponseContent::GetNetworkName {
                relocated_id: their_public_id,
            };

            let response_msg = ResponseMessage {
                src: Authority::NaeManager(dst_name.clone()),
                dst: Authority::Client {
                    client_key: client_key,
                    proxy_node_name: proxy_name,
                },
                content: response_content,
            };

            let routing_msg = RoutingMessage::Response(response_msg);

            let signed_message = try!(SignedMessage::new(routing_msg, &self.full_id));
            self.send(signed_message);
        }

        // From X -> Y; Send to close group of the relocated name
        {
            let request_content = RequestContent::ExpectCloseNode {
                expect_id: their_public_id.clone(),
            };

            let request_msg = RequestMessage {
                src: Authority::NaeManager(dst_name),
                dst: Authority::NodeManager(relocated_name),
                content: request_content,
            };

            let routing_msg = RoutingMessage::Request(request_msg);

            let signed_message = try!(SignedMessage::new(routing_msg, &self.full_id));

            self.send(signed_message)
        }
    }

    // Received by Y; From X -> Y
    fn handle_expect_close_node_request(&mut self, expect_id: PublicId) -> Result<(), RoutingError> {
        if let Some(prev_id) = self.node_id_cache.insert(*expect_id.name(), expect_id) {
            warn!("Previous id {:?} with same name found during \
                   handle_expect_close_node_request. Ignoring that",
                  prev_id);
             return Err(RoutingError::RejectedPublicId);
        }

        Ok(())
    }

    // Received by A; From X -> A
    fn handle_get_network_name_response(&mut self,
                                        relocated_id: PublicId,
                                        client_key: sign::PublicKey,
                                        proxy_name: XorName)
                                        -> Result<(), RoutingError> {
        self.set_self_node_name(*relocated_id.name());

        let request_content = RequestContent::GetCloseGroup;

        // From A -> Y
        let request_msg = RequestMessage {
            src: Authority::Client {
                client_key: client_key,
                proxy_node_name: proxy_name,
            },
            dst: Authority::NodeManager(*relocated_id.name()),
            content: request_content,
        };

        let routing_msg = RoutingMessage::Request(request_msg);

        let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

        self.send(signed_msg)
    }

    // Received by Y; From A -> Y
    fn handle_get_close_group_request(&mut self,
                                      client_key: sign::PublicKey,
                                      proxy_name: XorName,
                                      dst_name: XorName)
                                      -> Result<(), RoutingError> {
        let mut public_ids = self.routing_table
                                 .our_close_group()
                                 .into_iter()
                                 .map(|node_info| node_info.public_id)
                                 .collect_vec();

        // Also add our own full_id to the close_group list getting sent
        public_ids.push(self.full_id.public_id().clone());

        let response_content = ResponseContent::GetCloseGroup { close_group_ids: public_ids };

        let response_msg = ResponseMessage {
            src: Authority::NodeManager(dst_name),
            dst: Authority::Client {
                client_key: client_key,
                proxy_node_name: proxy_name,
            },
            content: response_content,
        };

        let routing_message = RoutingMessage::Response(response_msg);

        let signed_message = try!(::messages::SignedMessage::new(routing_message, &self.full_id));

        self.send(signed_message)
    }

    // Received by A; From Y -> A
    fn handle_get_close_group_response(&mut self,
                                       close_group_ids: Vec<::id::PublicId>,
                                       client_key: sign::PublicKey,
                                       proxy_name: XorName)
                                       -> Result<(), RoutingError> {
        self.start_listening();

        // From A -> Each in Y
        for peer_id in close_group_ids {
            try!(self.send_endpoints(&peer_id,
                                     Authority::Client {
                                         client_key: client_key,
                                         proxy_node_name: proxy_name,
                                     },
                                     ::authority::Authority::ManagedNode(*peer_id.name())));

            let _ = self.node_id_cache.insert(*peer_id.name(), peer_id);
        }

        Ok(())
    }

    fn send_endpoints(&mut self,
                      their_public_id: &::id::PublicId,
                      src_authority: Authority,
                      dst_authority: Authority)
                      -> Result<(), RoutingError> {
        let encoded_endpoints =
            try!(::maidsafe_utilities::serialisation::serialise(&self.accepting_on));
        let nonce = box_::gen_nonce();
        let encrypted_endpoints = box_::seal(&encoded_endpoints,
                                             &nonce,
                                             their_public_id.encrypting_public_key(),
                                             self.full_id.encrypting_private_key());

        let request_content = RequestContent::Endpoints {
            encrypted_endpoints: encrypted_endpoints,
            nonce_bytes: nonce.0,
        };

        let request_msg = RequestMessage {
            src: src_authority,
            dst: dst_authority,
            content: request_content,
        };

        let routing_msg = RoutingMessage::Request(request_msg);

        let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

        self.send(signed_msg)
    }

    fn handle_endpoints_from_client(&mut self,
                                    encrypted_endpoints: Vec<u8>,
                                    nonce_bytes: [u8; box_::NONCEBYTES],
                                    client_key: sign::PublicKey,
                                    proxy_name: XorName,
                                    dst_name: XorName)
                                    -> Result<(), RoutingError> {
        match self.node_id_cache
                  .retrieve_all()
                  .iter()
                  .find(|elt| *elt.1.signing_public_key() == client_key) {
            Some(&(ref name, ref their_public_id)) => {
                if self.routing_table.want_to_add(&name) {
                    try!(self.connect(encrypted_endpoints,
                                      nonce_bytes,
                                      their_public_id.encrypting_public_key()));
                    self.send_endpoints(their_public_id,
                                        Authority::ManagedNode(dst_name),
                                        Authority::Client {
                                            client_key: client_key,
                                            proxy_node_name: proxy_name,
                                        })
                } else {
                    Err(RoutingError::RefusedFromRoutingTable)
                }
            }
            None => Err(RoutingError::RejectedPublicId),
        }
    }

    fn handle_endpoints_from_node(&mut self,
                                  encrypted_endpoints: Vec<u8>,
                                  nonce_bytes: [u8; box_::NONCEBYTES],
                                  src_name: XorName,
                                  dst: Authority)
                                  -> Result<(), RoutingError> {
        if self.routing_table.want_to_add(&src_name) {
            if let Some(their_public_id) = self.node_id_cache.get(&src_name).map(|id| id.clone()) {
                self.connect(encrypted_endpoints,
                             nonce_bytes,
                             their_public_id.encrypting_public_key())
            } else {
                let request_content = RequestContent::GetPublicIdWithEndpoints {
                    encrypted_endpoints: encrypted_endpoints,
                    nonce_bytes: nonce_bytes,
                };

                let request_msg = RequestMessage {
                    src: dst,
                    dst: Authority::ManagedNode(src_name),
                    content: request_content,
                };

                let routing_msg = RoutingMessage::Request(request_msg);

                let signed_message = try!(SignedMessage::new(routing_msg, &self.full_id));

                self.send(signed_message)
            }
        } else {
            let _ = self.node_id_cache.remove(&src_name);
            Err(RoutingError::RefusedFromRoutingTable)
        }
    }

    // ---- Connect Requests and Responses --------------------------------------------------------

    fn send_connect_request(&mut self, dst_name: &XorName) -> Result<(), RoutingError> {
        let request_content = RequestContent::Connect;

        let request_msg = RequestMessage {
            src: Authority::ManagedNode(self.full_id.public_id().name().clone()),
            dst: Authority::ManagedNode(*dst_name),
            content: request_content,
        };

        let routing_msg = RoutingMessage::Request(request_msg);

        let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

        self.send(signed_msg)
    }

    fn handle_connect_request(&mut self,
                              src_name: XorName,
                              dst_name: XorName)
                              -> Result<(), RoutingError> {
        if !self.routing_table.want_to_add(&src_name) {
            return Err(RoutingError::RefusedFromRoutingTable);
        }

        if let Some(public_id) = self.node_id_cache.get(&src_name) {
            try!(self.send_endpoints(public_id,
                                     Authority::ManagedNode(self.full_id
                                                                .public_id()
                                                                .name()
                                                                .clone()),
                                     Authority::ManagedNode(src_name)));
            return Ok(());
        }

        let request_content = RequestContent::GetPublicId;

        let request_msg = RequestMessage {
            src: Authority::ManagedNode(dst_name),
            dst: Authority::NodeManager(src_name),
            content: request_content,
        };

        let routing_msg = RoutingMessage::Request(request_msg);

        let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

        self.send(signed_msg)
    }

    fn handle_get_public_id(&mut self,
                            src_name: XorName,
                            dst_name: XorName)
                            -> Result<(), RoutingError> {
        if let Some(node_info) = self.routing_table
                                     .our_close_group()
                                     .into_iter()
                                     .find(|elt| *elt.name() == dst_name) {
            let response_content = ResponseContent::GetPublicId { public_id: node_info.public_id };

            let response_msg = ResponseMessage {
                src: Authority::NodeManager(dst_name),
                dst: Authority::ManagedNode(src_name),
                content: response_content,
            };

            let routing_msg = RoutingMessage::Response(response_msg);

            let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

            self.send(signed_msg)
        } else {
            // TODO Invent error for this
            Err(::error::RoutingError::RejectedPublicId)
        }
    }

    fn handle_get_public_id_response(&mut self,
                                     public_id: PublicId,
                                     dst_name: XorName)
                                     -> Result<(), RoutingError> {
        if !self.routing_table.want_to_add(public_id.name()) {
            return Err(::error::RoutingError::RefusedFromRoutingTable);
        }

        try!(self.send_endpoints(&public_id,
                                 Authority::ManagedNode(dst_name),
                                 Authority::ManagedNode(public_id.name().clone())));
        let _ = self.node_id_cache.insert(public_id.name().clone(), public_id);

        Ok(())
    }

    fn handle_get_public_id_with_endpoints(&mut self,
                                           encrypted_endpoints: Vec<u8>,
                                           nonce_bytes: [u8; box_::NONCEBYTES],
                                           src_name: XorName,
                                           dst_name: XorName)
                                           -> Result<(), RoutingError> {
        if let Some(node_info) = self.routing_table
                                     .our_close_group()
                                     .into_iter()
                                     .find(|elt| *elt.name() == dst_name) {
            let response_content = ResponseContent::GetPublicIdWithEndpoints {
                public_id: node_info.public_id,
                encrypted_endpoints: encrypted_endpoints,
                nonce_bytes: nonce_bytes,
            };

            let response_msg = ResponseMessage {
                src: Authority::NodeManager(dst_name),
                dst: Authority::ManagedNode(src_name),
                content: response_content,
            };

            let routing_msg = RoutingMessage::Response(response_msg);

            let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

            self.send(signed_msg)
        } else {
            // TODO Invent error for this
            Err(::error::RoutingError::RejectedPublicId)
        }
    }

    fn handle_get_public_id_with_endpoints_response(&mut self,
                                                    public_id: PublicId,
                                                    encrypted_endpoints: Vec<u8>,
                                                    nonce_bytes: [u8; box_::NONCEBYTES],
                                                    dst_name: XorName)
                                                    -> Result<(), RoutingError> {
        if !self.routing_table.want_to_add(public_id.name()) {
            return Err(::error::RoutingError::RefusedFromRoutingTable);
        }

        try!(self.send_endpoints(&public_id,
                                 Authority::ManagedNode(dst_name),
                                 Authority::ManagedNode(public_id.name().clone())));
        let _ = self.node_id_cache.insert(public_id.name().clone(), public_id.clone());

        self.connect(encrypted_endpoints,
                     nonce_bytes,
                     public_id.encrypting_public_key())
    }

    fn connect(&mut self,
               encrypted_endpoints: Vec<u8>,
               nonce_bytes: [u8; box_::NONCEBYTES],
               their_public_key: &box_::PublicKey)
               -> Result<(), RoutingError> {
        let decipher_result = box_::open(&encrypted_endpoints,
                                         &box_::Nonce(nonce_bytes),
                                         their_public_key,
                                         self.full_id.encrypting_private_key());

        let serialised_endpoints = try!(decipher_result.map_err(|()| {
            ::error::RoutingError::AsymmetricDecryptionFailure
        }));
        let endpoints =
            try!(::maidsafe_utilities::serialisation::deserialise(&serialised_endpoints));

        self.crust_service.connect(0u32, endpoints);

        Ok(())
    }

    // ----- Send Functions -----------------------------------------------------------------------

    fn send_to_user(&self, event: Event) {
        debug!("Send to user event");
        if self.event_sender.send(event).is_err() {
            error!("Channel to user is broken;");
        }
    }

    fn send_content(&mut self,
                    _src: Authority,
                    _dst: Authority,
                    _content: RequestContent)
                    -> Result<(), RoutingError> {
        // TODO Does this need split up for Requests / Responses
        Ok(())

        //
        // let routing_message = RoutingMessage {
        // source_authority: source_authority,
        // destination_authority: destination_authority,
        // content: content,
        // group_keys: None,
        // };
        //
        // let signed_message = try!(SignedMessage::new(&routing_message, &self.full_id));
        //
        // Ok(self.send(signed_message))
    }

    fn client_send_content(&mut self,
                           dst: Authority,
                           content: RequestContent)
                           -> Result<(), RoutingError> {
        let request_msg = RequestMessage {
            src: try!(self.get_client_authority()),
            dst: dst,
            content: content,
        };

        let routing_msg = RoutingMessage::Request(request_msg);

        let signed_msg = try!(SignedMessage::new(routing_msg, &self.full_id));

        // TODO Check and send local failures to client
        self.send(signed_msg)
    }

    fn send_failed_message_to_user(&self, _dst: Authority, _content: RequestContent) {
        // match content {
        // Content::ExternalRequest(external_request) => {
        // self.send_to_user(Event::FailedRequest {
        // request: external_request,
        // our_authority: None,
        // location: destination_authority,
        // interface_error: InterfaceError::NotConnected,
        // });
        // }
        // Content::ExternalResponse(external_response) => {
        // self.send_to_user(Event::FailedResponse {
        // response: external_response,
        // our_authority: None,
        // location: destination_authority,
        // interface_error: InterfaceError::NotConnected,
        // });
        // }
        // _ => {
        // error!("{}InternalRequest/Response was sent back to user {:?}",
        // self,
        // content)
        // }
        // }
    }

    fn relay_to_client(&mut self, _signed_message: SignedMessage)-> Result<(), RoutingError> {
        Ok(())
    }
    /// Send a SignedMessage out to the destination
    /// 1. if it can be directly sent to a Client, then it will
    /// 2. if we can forward it to nodes closer to the destination, it will be sent in parallel
    /// 3. if the destination is in range for us, then send it to all our close group nodes
    /// 4. if all the above failed, try sending it over all available bootstrap connections
    /// 5. finally, if we are a node and the message concerns us, queue it for processing later.
    fn send(&mut self, _signed_message: SignedMessage)-> Result<(), RoutingError> {
        Ok(())
    // let message = match signed_message.get_routing_message() {
    //     Ok(routing_message) => routing_message,
    //     Err(error) => {
    //         debug!("{}Signature failed. {:?}", self, error);
    //         return;
    //     }
    // };

    // let destination_authority = message.destination_authority;
    // debug!("{}Send request to {:?}", self, destination_authority);
    // let bytes = match encode(&signed_message) {
    //     Ok(bytes) => bytes,
    //     Err(error) => {
    //         error!("{}Failed to serialise {:?} - {:?}",
    //                self,
    //                signed_message,
    //                error);
    //         return;
    //     }
    // };

    // // If we're a client going to be a node, send via our bootstrap connection
    // if self.state == State::Client {
    //     let bootstrap_connections: Vec<&::crust::Connection> = self.proxy_map.keys().collect();
    //     if bootstrap_connections.is_empty() {
    //         unreachable!("{}Target connections for send is empty", self);
    //     }
    //     for connection in bootstrap_connections {
    //         self.crust_service.send(connection.clone(), bytes.clone());
    //         debug!("{}Sent {:?} to bootstrap connection {:?}",
    //                self,
    //                signed_message,
    //                connection);
    //     }
    //     return;
    // }

    // // Handle if we have a client connection as the destination_authority
    // if let Authority::Client(_, ref client_public_key) = destination_authority {
    //     debug!("{}Looking for client target {:?}", self,
    //            ::XorName::new(
    //                hash::sha512::hash(&client_public_key[..]).0));
    //     if let Some(client_connection) = self.client_map.get(client_public_key) {
    //         self.crust_service.send(client_connection.clone(), bytes);
    //     } else {
    //         warn!("{}Failed to find client contact for {:?}", self,
    //               ::XorName::new(
    //                   hash::sha512::hash(&client_public_key[..]).0));
    //     }
    //     return;
    // }

    // // Query routing table to send it out parallel or to our close group (ourselves excluded)
    // let targets = self.routing_table.target_nodes(destination_authority.get_location());
    // targets.iter().all(|node_info| {
    //     node_info.connections.iter().all(|connection| {
    //         self.crust_service.send(connection.clone(), bytes.clone());
    //         true
    //     })
    // });

    // // If we need to handle this message, handle it.
    // if self.name_in_range(destination_authority.get_location()) {
    //     if let Err(error) = self.handle_routing_message(signed_message) {
    //         error!("{}Failed to handle message ourself: {:?}", self, error)
    //     }
    // }
    }

    // ----- Message Handlers that return to the event channel ------------------------------------

    fn handle_refresh(&mut self,
                      _type_tag: u64,
                      _sender: XorName,
                      _payload: Vec<u8>,
                      _our_authority: Authority,
                      _cause: ::XorName)
                      -> Result<(), RoutingError> {
        Ok(())
        // debug_assert!(our_authority.is_group());
        // let threshold = self.routing_table.dynamic_quorum_size();
        // let unknown_cause = !self.refresh_causes.check(&cause);
        // let (is_new_request, payloads) = self.refresh_accumulator
        // .add_message(threshold,
        // type_tag.clone(),
        // sender,
        // our_authority.clone(),
        // payload,
        // cause);
        // If this is a new refresh instance, notify user to perform refresh.
        // if unknown_cause && is_new_request {
        // let _ = self.event_sender.send(::event::Event::DoRefresh(type_tag,
        // our_authority.clone(),
        // cause.clone()));
        // }
        // match payloads {
        // Some(payloads) => {
        // let _ = self.event_sender.send(Event::Refresh(type_tag, our_authority, payloads));
        // Ok(())
        // }
        // None => Err(::error::RoutingError::NotEnoughSignatures),
        // }
    }

    fn get_client_authority(&self) -> Result<Authority, RoutingError> {
        match self.proxy_map.iter().next() {
            Some((ref connection, ref bootstrap_pub_id)) => {
                Ok(Authority::Client {
                    client_key: *self.full_id.public_id().signing_public_key(),
                    proxy_node_name: bootstrap_pub_id.name().clone(),
                })
            }
            None => Err(RoutingError::NotBootstrapped),
        }
    }

    // set our network name while transitioning to a node
    // If called more than once with a unique name, this function will assert
    fn set_self_node_name(&mut self, new_name: XorName) {
        // Validating this function doesn't run more that once
        let hash_of_name = XorName(hash::sha512::hash(&self.full_id
                                                           .public_id()
                                                           .signing_public_key()
                                                           .0).0);

        self.routing_table = RoutingTable::new(&new_name);
        self.full_id.public_id_mut().set_name(new_name);
    }

    /// check client_map for a client and remove from map
    fn dropped_client_connection(&mut self, connection: &::crust::Connection) {
        let public_key = self.client_map
                             .iter()
                             .find(|&(_, client)| client == connection)
                             .map(|entry| entry.0.clone());
        if let Some(public_key) = public_key {
            let _ = self.client_map.remove(&public_key);
        }
    }

    fn dropped_bootstrap_connection(&mut self, connection: &::crust::Connection) {
        let _ = self.proxy_map.remove(connection);
    }

    fn dropped_routing_node_connection(&mut self, connection: &::crust::Connection) {
        if let Some(node_name) = self.routing_table.drop_connection(connection) {
            for _node in &self.routing_table.our_close_group() {
                // trigger churn
                // if close node
            }
            self.routing_table.drop_node(&node_name);
        }
    }
}

impl Debug for RoutingNode {
    fn fmt(&self, f: &mut Formatter) -> ::std::fmt::Result {
        write!(f,
               "{:?}({:?}) - ",
               self.state,
               self.full_id.public_id().name())
    }
}
