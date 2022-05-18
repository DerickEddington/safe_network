// Copyright 2022 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

//! Module providing keys, keypairs, and signatures.
//!
//! The easiest way to get a `PublicKey` is to create a random `Keypair` first through one of the
//! `new` functions. A `PublicKey` can't be generated by itself; it must always be derived from a
//! secret key.

use super::super::{Error, Result};
use bls::{self, serde_impl::SerdeSecret};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Debug, Display, Formatter};

// TODO: remove clones. We need to restructure to hold keypair ones and only require references for this.
/// Wrapper for different secret key types.
#[derive(Debug, Serialize, Deserialize)]
pub enum SecretKey {
    /// Ed25519 secretkey.
    Ed25519(ed25519_dalek::SecretKey),
    /// BLS secretkey share.
    BlsShare(SerdeSecret<bls::SecretKeyShare>),
}

impl SecretKey {
    /// Construct a secret key from a hex string
    ///
    /// Similar to public key, it is often useful in user
    /// facing apps to be able to set your own secret
    /// key without depending on both the ed25519_dalek
    /// and hex crates just to reimplement this function
    pub fn ed25519_from_hex(hex: &str) -> Result<Self> {
        let bytes = hex::decode(hex).map_err(|err| {
            Error::FailedToParse(format!(
                "Couldn't parse edd25519 secret key bytes from hex: {}",
                err
            ))
        })?;
        let ed25519_sk = ed25519_dalek::SecretKey::from_bytes(bytes.as_ref()).map_err(|err| {
            Error::FailedToParse(format!(
                "Couldn't parse ed25519 secret key from bytes: {}",
                err
            ))
        })?;
        Ok(Self::Ed25519(ed25519_sk))
    }
}

impl Display for SecretKey {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        Debug::fmt(self, formatter)
    }
}

#[cfg(feature = "test-utils")]
pub mod test_utils {
    use crate::messaging::system::KeyedSig;
    use crate::network_knowledge::{elder_count, supermajority};
    use std::ops::Deref;

    fn threshold() -> usize {
        supermajority(elder_count()) - 1
    }

    // Wrapper for `bls::SecretKeySet` that also allows to retrieve the corresponding `bls::SecretKey`.
    // Note: `bls::SecretKeySet` does have a `secret_key` method, but it's test-only and not available
    // for the consumers of the crate.
    pub struct SecretKeySet {
        set: bls::SecretKeySet,
        key: bls::SecretKey,
    }

    impl SecretKeySet {
        pub fn random() -> Self {
            let poly = bls::poly::Poly::random(threshold(), &mut rand::thread_rng());
            let key = bls::SecretKey::from_mut(&mut poly.evaluate(0));
            let set = bls::SecretKeySet::from(poly);

            Self { set, key }
        }

        pub fn secret_key(&self) -> &bls::SecretKey {
            &self.key
        }
    }

    impl Deref for SecretKeySet {
        type Target = bls::SecretKeySet;

        fn deref(&self) -> &Self::Target {
            &self.set
        }
    }

    /// Create signature for the given bytes using the given secret key.
    pub fn keyed_signed(secret_key: &bls::SecretKey, bytes: &[u8]) -> KeyedSig {
        KeyedSig {
            public_key: secret_key.public_key(),
            signature: secret_key.sign(bytes),
        }
    }
}