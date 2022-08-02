use std::path::PathBuf;

use once_cell::sync::Lazy;
use serde::Deserialize;

pub static CONFIG: Lazy<Config> = Lazy::new(|| envy::prefixed("CONF_").from_env().unwrap());

fn default_listen_addr() -> String {
    "0.0.0.0:3000".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    /// Installed Kubernetes Service namespace of the controller
    pub service_namespace: String,
    /// Installed Kubernetes Service name of the controller
    pub service_name: String,
    /// Installed Kubernetes Service port of the controller
    pub service_port: i32,

    /// Certificate path for HTTPS
    pub cert_path: PathBuf,
    /// Certificate key path for HTTPS
    pub key_path: PathBuf,

    /// Base64 encoded PEM CA bundle for webhook config
    pub ca_bundle: String,

    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
}
