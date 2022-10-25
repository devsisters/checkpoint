use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhook, MutatingWebhookConfiguration, ServiceReference, ValidatingWebhook,
    ValidatingWebhookConfiguration, WebhookClientConfig,
};
use kube::{
    api::{ObjectMeta, Patch, PatchParams},
    runtime::controller::Action,
    Api, Resource,
};
use thiserror::Error;

use crate::{
    config::ControllerConfig,
    types::rule::{MutatingRule, ValidatingRule},
};

const VALIDATINGRULE_OWNED_LABEL_KEY: &str = "checkpoint.devsisters.com/validatingrule";
const MUTATINGRULE_OWNED_LABEL_KEY: &str = "checkpoint.devsisters.com/mutatingrule";

pub struct ReconcilerContext {
    pub client: kube::Client,
    pub config: ControllerConfig,
}

/// Errors can be raised within reconciler
#[derive(Debug, Error)]
pub enum Error {
    #[error("MissingObjectKey: {0}")]
    MissingObjectKey(&'static str),
    #[error("Failed to create ValidatingWebhookConfiguration: {0}")]
    ValidatingWebhookConfigurationCreationFailed(#[source] kube::Error),
    #[error("Failed to create MutatingWebhookConfiguration: {0}")]
    MutatingWebhookConfigurationCreationFailed(#[source] kube::Error),
}

fn webhook_client_config(
    config: &ControllerConfig,
    path: &str,
    rule_name: &str,
) -> WebhookClientConfig {
    WebhookClientConfig {
        ca_bundle: Some(k8s_openapi::ByteString(
            config.ca_bundle.as_bytes().to_vec(),
        )),
        service: Some(ServiceReference {
            namespace: config.service_namespace.clone(),
            name: config.service_name.clone(),
            path: Some(format!("/{}/{}", path, rule_name)),
            port: Some(config.service_port),
        }),
        url: None,
    }
}

macro_rules! webhook_configuration {
    (
        @internal
        $webhook_configuration_ty:ident,
        $webhook_ty:ident,
        $ty:expr,
        $path:expr,
        $owned_label_key:ident,
        $name:expr,
        $oref:expr,
        $spec:expr,
        $config:expr
    ) => {
        {
            let mut labels = ::std::collections::BTreeMap::default();
            labels.insert($owned_label_key.to_string(), $name.clone());

            $webhook_configuration_ty {
                metadata: ObjectMeta {
                    name: Some($name.clone()),
                    owner_references: Some(vec![$oref]),
                    labels: Some(labels),
                    ..Default::default()
                },
                webhooks: Some(vec![$webhook_ty {
                    name: format!("{}.{}.checkpoint.devsisters.com", $name, $ty),
                    failure_policy: $spec.failure_policy.map(|fp| fp.to_string()),
                    namespace_selector: $spec.namespace_selector,
                    object_selector: $spec.object_selector,
                    rules: $spec.object_rules,
                    timeout_seconds: $spec.timeout_seconds,
                    client_config: webhook_client_config(&$config, $path, &$name),
                    admission_review_versions: vec!["v1".to_string()],
                    side_effects: "None".to_string(),
                    ..Default::default()
                }]),
            }
        }
    };
    (
        validate,
        $name:expr,
        $oref:expr,
        $spec:expr,
        $config:expr
    ) => {
        webhook_configuration!(
            @internal
            ValidatingWebhookConfiguration,
            ValidatingWebhook,
            "validatingwebhook",
            "validate",
            VALIDATINGRULE_OWNED_LABEL_KEY,
            $name,
            $oref,
            $spec,
            $config
        )
    };
    (
        mutate,
        $name:expr,
        $oref:expr,
        $spec:expr,
        $config:expr
    ) => {
        webhook_configuration!(
            @internal
            MutatingWebhookConfiguration,
            MutatingWebhook,
            "mutatingwebhook",
            "mutate",
            MUTATINGRULE_OWNED_LABEL_KEY,
            $name,
            $oref,
            $spec,
            $config
        )
    };
}

/// ValidatingRule reconciler
pub async fn reconcile_validatingrule(
    validating_rule: Arc<ValidatingRule>,
    ctx: Arc<ReconcilerContext>,
) -> Result<Action, Error> {
    // Get Kubernetes client from context data
    let client = &ctx.client;

    let validating_rule = (*validating_rule).clone();

    // Prepare ownership reference
    let oref = validating_rule.controller_owner_ref(&()).unwrap();

    let name = validating_rule
        .metadata
        .name
        .ok_or(Error::MissingObjectKey(".metadata.name"))?;

    // Prepare Kubernetes API
    let vwc_api = Api::<ValidatingWebhookConfiguration>::all(client.clone());

    // Popluate ValidatingWebhookConfiguration
    let vwc: ValidatingWebhookConfiguration =
        webhook_configuration!(validate, name, oref, validating_rule.spec.0, ctx.config);

    // Create or update ValidatingWebhookConfiguration
    vwc_api
        .patch(
            &name,
            &PatchParams::apply("validatingrule.checkpoint.devsisters.com"),
            &Patch::Apply(&vwc),
        )
        .await
        .map_err(Error::ValidatingWebhookConfigurationCreationFailed)?;

    Ok(Action::await_change())
}

/// MutatingRule reconciler
pub async fn reconcile_mutatingrule(
    mutating_rule: Arc<MutatingRule>,
    ctx: Arc<ReconcilerContext>,
) -> Result<Action, Error> {
    // Get Kubernetes client from context data
    let client = &ctx.client;

    let mutating_rule = (*mutating_rule).clone();

    // Prepare ownership reference
    let oref = mutating_rule.controller_owner_ref(&()).unwrap();

    let name = mutating_rule
        .metadata
        .name
        .ok_or(Error::MissingObjectKey(".metadata.name"))?;

    // Prepare Kubernetes API
    let mwc_api = Api::<MutatingWebhookConfiguration>::all(client.clone());

    // Popluate MutatingWebhookConfiguration
    let mwc: MutatingWebhookConfiguration =
        webhook_configuration!(mutate, name, oref, mutating_rule.spec.0, ctx.config);

    // Create or update MutatingWebhookConfiguration
    mwc_api
        .patch(
            &name,
            &PatchParams::apply("mutatingrule.checkpoint.devsisters.com"),
            &Patch::Apply(&mwc),
        )
        .await
        .map_err(Error::MutatingWebhookConfigurationCreationFailed)?;

    Ok(Action::await_change())
}

/// When error occurred, log it and requeue after three seconds
pub fn error_policy<T>(_rule: Arc<T>, error: &Error, _ctx: Arc<ReconcilerContext>) -> Action {
    tracing::error!(%error);
    Action::requeue(Duration::from_secs(3))
}
