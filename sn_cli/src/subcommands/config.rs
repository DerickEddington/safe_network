// Copyright 2020 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use crate::operations::config::{Config, NetworkInfo};
use color_eyre::Result;
use structopt::StructOpt;
use tracing::debug;

#[derive(StructOpt, Debug)]
pub enum ConfigSubCommands {
    #[structopt(name = "add")]
    /// Add a config setting
    Add(SettingAddCmd),
    #[structopt(name = "remove")]
    /// Remove a config setting
    Remove(SettingRemoveCmd),
    #[structopt(name = "clear")]
    /// Remove all config settings
    Clear,
}

#[derive(StructOpt, Debug)]
pub enum SettingAddCmd {
    #[structopt(name = "network")]
    Network {
        /// Network name
        network_name: String,
        /// Location of the network connection information. If this argument is not passed, it takes current network connection information and caches it
        config_location: Option<String>,
    },
    // #[structopt(name = "contact")]
    // Contact {
    //    /// Contact friendly name
    //    name: String,
    //    /// SafeId of the contact
    //    safeid: String,
    // },
}

#[derive(StructOpt, Debug)]
pub enum SettingRemoveCmd {
    #[structopt(name = "network")]
    Network {
        /// Network to remove
        network_name: String,
    },
    // #[structopt(name = "contact")]
    // Contact {
    //    /// Name of the contact to remove
    //    name: String,
    // },
}

pub async fn config_commander(cmd: Option<ConfigSubCommands>, config: &mut Config) -> Result<()> {
    match cmd {
        Some(ConfigSubCommands::Add(SettingAddCmd::Network {
            network_name,
            config_location,
        })) => {
            config
                .add_network(
                    &network_name,
                    config_location.map(NetworkInfo::ConnInfoLocation),
                )
                .await?;
        }
        // Some(ConfigSubCommands::Add(SettingAddCmd::Contact { name, safeid })) => {}
        Some(ConfigSubCommands::Remove(SettingRemoveCmd::Network { network_name })) => {
            config.remove_network(&network_name).await?
        }
        // Some(ConfigSubCommands::Remove(SettingRemoveCmd::Contact { name })) => {}
        Some(ConfigSubCommands::Clear) => {
            config.clear().await?;
            debug!("Config settings cleared out");
        }
        None => config.print_networks().await,
    }

    Ok(())
}
