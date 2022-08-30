use std::path::PathBuf;

use serde::Deserialize;

fn default_listen_addr() -> String {
    "0.0.0.0:3000".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct ControllerConfig {
    /// Installed Kubernetes Service namespace of the controller
    pub service_namespace: String,
    /// Installed Kubernetes Service name of the controller
    pub service_name: String,

    /// Base64 encoded PEM CA bundle for controller
    pub ca_bundle: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct WebhokConfig {
    /// Certificate path for HTTPS
    pub cert_path: PathBuf,
    /// Certificate key path for HTTPS
    pub key_path: PathBuf,

    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
}
