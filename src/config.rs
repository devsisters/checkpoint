use std::path::PathBuf;

use serde::Deserialize;

fn default_listen_addr() -> String {
    "0.0.0.0:3000".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct ControllerConfig {
    /// Installed Kubernetes Service namespace of the checkpoint webhook
    pub service_namespace: String,
    /// Installed Kubernetes Service name of the checkpoint webhook
    pub service_name: String,
    /// Installed Kubernetes Service port of the checkpoint webhook
    pub service_port: i32,

    /// Base64 encoded PEM CA bundle for the checkpoint webhook
    pub ca_bundle: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct WebhookConfig {
    /// Certificate path for HTTPS
    pub cert_path: PathBuf,
    /// Certificate key path for HTTPS
    pub key_path: PathBuf,

    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
}
