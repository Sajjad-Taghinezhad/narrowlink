use narrowlink_types::ServiceType;
use serde::{Deserialize, Serialize};
use std::{env, fs::File, io::Read, path::PathBuf};

use crate::error::AgentError;

#[derive(Deserialize, Serialize, Default, PartialEq, Clone, Copy)]
pub enum KeyPolicy {
    #[default]
    Lax,
    Strict,
}
#[derive(Deserialize, Serialize)]
pub struct SelfHosted {
    pub gateway: String,
    pub token: String,
    pub publish: Option<Vec<String>>,
    #[serde(default = "ServiceType::default")]
    pub protocol: ServiceType,
}

#[derive(Deserialize, Serialize)]
pub enum Endpoint {
    // Platform(Platform),
    // Cloud(Cloud),
    SelfHosted(SelfHosted),
}

#[derive(Deserialize, Serialize, Clone)]
pub struct PassPhrase {
    pub phrase: String,
    #[serde(default = "KeyPolicy::default")]
    pub policy: KeyPolicy,
}

#[derive(Deserialize, Serialize, Clone)]
pub enum E2EE {
    PassPhrase(PassPhrase),
}

#[derive(Deserialize, Serialize)]
pub struct Config {
    pub endpoints: Vec<Endpoint>,
    #[serde(default = "Vec::new")]
    pub e2ee: Vec<E2EE>,
}

impl Config {
    pub fn load(path: Option<String>) -> Result<Self, AgentError> {
        let custom_path = if let Some(path) = path {
            let path = PathBuf::from(path);
            Some(
                if let Some(stripped_path) = path.strip_prefix("~/").ok().filter(|_| !cfg!(windows))
                {
                    let home_dir = dirs::home_dir().ok_or(AgentError::InvalidConfigPath)?;
                    home_dir.join(stripped_path)
                } else {
                    path
                },
            )
        } else {
            None
        };

        let current_dir = env::current_dir()
            .map(|mut d| {
                d.push("agent");
                d.set_extension("yaml");
                d
            })
            .ok()
            .filter(|f| f.is_file());
        let config_dir = dirs::config_dir()
            .map(|mut d| {
                d.push("narrowlink");
                d.push("agent");
                d.set_extension("yaml");
                d
            })
            .filter(|f| f.is_file());

        let home_dir = dirs::home_dir()
            .map(|mut d| {
                d.push(".narrowlink");
                d.push("agent");
                d.set_extension("yaml");
                d
            })
            .filter(|f| f.is_file());

        let etc = if cfg!(target_os = "linux") {
            Some(PathBuf::from("/etc/narrowlink/agent.yaml"))
        } else {
            None
        }
        .filter(|f| f.is_file());

        let path = custom_path
            .or(current_dir)
            .or(config_dir)
            .or(home_dir)
            .or(etc)
            .ok_or(AgentError::ConfigNotFound)?;

        let mut file = File::open(path)?;
        let mut configuration_data = String::new();
        file.read_to_string(&mut configuration_data)?;
        serde_yaml::from_str(&configuration_data).or(Err(AgentError::InvalidConfig))
    }
}

// impl SelfHosted {
//     pub fn get_agent_name(&self) -> Result<String, AgentError> {
//         self.token
//             .split('.')
//             .nth(1)
//             .and_then(|c| {
//                 base64::engine::general_purpose::STANDARD_NO_PAD
//                     .decode(c)
//                     .ok()
//             })
//             .and_then(|v| serde_json::from_slice::<AgentToken>(&v).ok())
//             .ok_or(AgentError::InvalidToken)
//             .map(|t| t.name)
//     }
// }
