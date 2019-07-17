// Copyright 2019 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use crate::{chunk_store::ImmutableChunkStore, vault::Init, Result};
use safe_nd::NodePublicId;
use std::{
    cell::RefCell,
    fmt::{self, Display, Formatter},
    path::Path,
    rc::Rc,
};

pub(crate) struct Adult {
    _id: NodePublicId,
    _immutable_chunks: ImmutableChunkStore,
}

impl Adult {
    pub fn new<P: AsRef<Path>>(
        id: NodePublicId,
        root_dir: P,
        max_capacity: u64,
        init_mode: Init,
    ) -> Result<Self> {
        let _immutable_chunks =
            ImmutableChunkStore::new(root_dir, max_capacity, Rc::new(RefCell::new(0)), init_mode)?;
        Ok(Self {
            _id: id,
            _immutable_chunks,
        })
    }
}

impl Display for Adult {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        write!(formatter, "{}", self._id)
    }
}
