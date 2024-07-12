use std::{borrow::Cow, path::PathBuf};

use serde::{
    de::{self, DeserializeOwned},
    Deserialize, Deserializer,
};

use crate::types::policy::{CronPolicyNotification, CronPolicyResource};

fn default_listen_addr() -> String {
    "[::]:3000".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct ControllerConfig {
    /// Installed Kubernetes Service namespace of the checkpoint webhook
    pub service_namespace: String,
    /// Installed Kubernetes Service name of the checkpoint webhook
    pub service_name: String,
    /// Installed Kubernetes Service port of the checkpoint webhook
    pub service_port: i32,

    /// Base64 encoded PEM CA bundle file path for the checkpoint webhook
    pub ca_bundle_path: PathBuf,

    /// Container image URL for checker
    pub checker_image: String,
}

impl ControllerConfig {
    pub fn try_from_env() -> Result<Self, envy::Error> {
        envy::prefixed("CONF_").from_env()
    }
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

impl WebhookConfig {
    pub fn try_from_env() -> Result<Self, envy::Error> {
        envy::prefixed("CONF_").from_env()
    }
}

fn deserialize_json_string<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let s = Cow::<'_, str>::deserialize(d)?;
    serde_json::from_str(&s).map_err(de::Error::custom)
}

#[derive(Deserialize, Clone, Debug)]
pub struct CheckerConfig {
    /// Name of the policy
    pub policy_name: String,
    /// Specifier for the resources to check in JSON string,
    #[serde(deserialize_with = "deserialize_json_string")]
    pub resources: Vec<CronPolicyResource>,
    /// JS code to evaluate on the resources.
    pub code: String,
    /// Notification configurations
    #[serde(deserialize_with = "deserialize_json_string")]
    pub notifications: CronPolicyNotification,
}

impl CheckerConfig {
    pub fn try_from_env() -> Result<Self, envy::Error> {
        envy::prefixed("CONF_").from_env()
    }
}
