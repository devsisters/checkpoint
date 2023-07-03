use std::{fmt::Display, sync::Arc, time::Duration};

use k8s_openapi::ByteString;
use kube::runtime::controller::Action;
use tokio::sync::RwLock;

use crate::config::ControllerConfig;

pub mod policy;
pub mod rule;

pub struct ReconcilerContext {
    pub client: kube::Client,
    pub config: ControllerConfig,
    pub ca_bundle: Arc<RwLock<ByteString>>,
}

/// When error occurred, log it and requeue after three seconds
pub fn error_policy<T, E>(_rule: Arc<T>, error: &E, _ctx: Arc<ReconcilerContext>) -> Action
where
    E: Display,
{
    tracing::error!(%error);
    Action::requeue(Duration::from_secs(3))
}
