// Copyright 2020 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

mod accumulation;
pub mod messaging;
mod msg_analysis;
mod network_events;

use crate::node::{
    adult_duties::AdultDuties,
    elder_duties::ElderDuties,
    node_duties::messaging::Messaging,
    node_ops::{NodeDuty, NodeOperation},
    state_db::{dump_state, AgeGroup, NodeInfo},
};
use msg_analysis::NetworkMsgAnalysis;
use network_events::NetworkEvents;
use rand::{CryptoRng, Rng};
use routing::Node as Routing;
use safe_nd::{NodeFullId, NodePublicId};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[allow(clippy::large_enum_variant)]
pub enum DutyLevel<R: CryptoRng + Rng> {
    Infant,
    Adult(AdultDuties),
    Elder(ElderDuties<R>),
}

pub struct NodeDuties<R: CryptoRng + Rng> {
    id: NodeFullId,
    node_info: NodeInfo,
    duty_level: DutyLevel<R>,
    network_events: NetworkEvents,
    messaging: Messaging,
    routing: Rc<RefCell<Routing>>,
    rng: Option<R>,
}

impl<R: CryptoRng + Rng> NodeDuties<R> {
    pub fn new(id: NodeFullId, node_info: NodeInfo, routing: Rc<RefCell<Routing>>, rng: R) -> Self {
        let network_events = NetworkEvents::new(NetworkMsgAnalysis::new(routing.clone()));
        let messaging = Messaging::new(routing.clone());
        Self {
            id,
            node_info,
            duty_level: DutyLevel::Infant,
            network_events,
            messaging,
            routing,
            rng: Some(rng),
        }
    }

    pub fn id(&self) -> &NodePublicId {
        self.id.public_id()
    }

    pub fn adult_duties(&mut self) -> Option<&mut AdultDuties> {
        use DutyLevel::*;
        match &mut self.duty_level {
            Adult(ref mut duties) => Some(duties),
            _ => None,
        }
    }

    pub fn elder_duties(&mut self) -> Option<&mut ElderDuties<R>> {
        use DutyLevel::*;
        match &mut self.duty_level {
            Elder(ref mut duties) => Some(duties),
            _ => None,
        }
    }

    pub fn process(&mut self, duty: NodeDuty) -> Option<NodeOperation> {
        use NodeDuty::*;
        match duty {
            BecomeAdult => self.become_adult(),
            BecomeElder => self.become_elder(),
            ProcessMessaging(duty) => self.messaging.process(duty),
            ProcessNetworkEvent(event) => self.network_events.process(event),
        }
    }

    fn become_adult(&mut self) -> Option<NodeOperation> {
        use DutyLevel::*;
        let total_used_space = Rc::new(Cell::new(0));
        if let Ok(duties) = AdultDuties::new(self.node_info.clone(), &total_used_space) {
            self.duty_level = Adult(duties);
            // NB: This is wrong, shouldn't write to disk here,
            // let it be upper layer resp.
            // Also, "Error-to-Unit" is not a good conversion..
            dump_state(AgeGroup::Adult, self.node_info.path(), &self.id).unwrap_or(());
        }
        None
    }

    fn become_elder(&mut self) -> Option<NodeOperation> {
        use DutyLevel::*;
        let total_used_space = Rc::new(Cell::new(0));

        if matches!(self.duty_level, Elder(_)) {
            return None;
        }
        if let Ok(duties) = ElderDuties::new(
            self.node_info.clone(),
            &total_used_space,
            self.routing.clone(),
            self.rng.take()?,
        ) {
            self.duty_level = Elder(duties);
            // NB: This is wrong, shouldn't write to disk here,
            // let it be upper layer resp.
            // Also, "Error-to-Unit" is not a good conversion..
            dump_state(AgeGroup::Elder, self.node_info.path(), &self.id).unwrap_or(())
        }
        None
    }
}
