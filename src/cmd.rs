// Copyright 2019 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use crate::msg::Message;
use safe_nd::{MessageId, PublicId, XorName};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Node internal cmds, about what requests to make.

/// Any network node
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum NodeCmd {
    /// Send to a client.
    SendToClient(MsgEnvelope),
    /// Send to a single node.
    SendToNode(MsgEnvelope),
    /// Send to a section.
    SendToSection(MsgEnvelope),
    /// Send the same request to each individual Adult.
    SendToAdults { msgs: BTreeSet<MsgEnvelope> },
    /// Vote for a cmd so we can process the deferred action on consensus.
    /// (Currently immediately.)
    VoteFor(ConsensusAction),
}

// Need to Serialize/Deserialize to go through the consensus process.
/// A ConsensusAction is something only
/// taking place at the network Gateways.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum ConsensusAction {
    /// When Gateway nodes consider a request
    /// valid, they will vote for it to be forwarded.
    /// As they reach consensus, this is then carried out.
    Forward(MsgEnvelope),
}

// /// The Gateway consists of
// /// the Elders in a section.
// #[derive(Debug)]
// #[allow(clippy::large_enum_variant)]
// pub(crate) enum GatewayCmd {
//     /// Vote for a cmd so we can process the deferred action on consensus.
//     /// (Currently immediately.)
//     VoteFor(ConsensusAction),
//     /// Send a validated client request from Gateway to the appropriate destination nodes.
//     ForwardClientMsg(MsgEnvelope),
//     /// Send a msg to client.
//     PushToClient(MsgEnvelope),
// }

// #[derive(Debug)]
// #[allow(clippy::large_enum_variant)]
// pub(crate) enum MetadataCmd {
//     /// Send the same request to each individual Adult.
//     SendToAdults {
//         targets: BTreeSet<XorName>,
//         msg: MsgEnvelope,
//     },
//     /// Accumulate rewards after Adults have
//     /// stored the data.
//     AccumulateReward { data_hash: Vec<u8>, num_bytes: u64 },
//     /// Send to sectioon (used for errors).
//     SendToSection(MsgEnvelope),
// }

//     /// Send a response to
//     /// our section's Elders, i.e. our peers.
//     RespondToElderPeers(Message),
