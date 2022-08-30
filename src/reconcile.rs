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

pub struct Data {
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

/// ValidatingRule reconciler
pub async fn reconcile_validatingrule(
    validating_rule: Arc<ValidatingRule>,
    ctx: Arc<Data>,
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
    let vwc = ValidatingWebhookConfiguration {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            owner_references: Some(vec![oref]),
            ..Default::default()
        },
        webhooks: Some(vec![ValidatingWebhook {
            name: format!("{}.validatingrule.checkpoint.devsisters.com", name),
            failure_policy: validating_rule
                .spec
                .0
                .failure_policy
                .map(|fp| fp.to_string()),
            namespace_selector: validating_rule.spec.0.namespace_selector,
            object_selector: validating_rule.spec.0.object_selector,
            rules: validating_rule.spec.0.object_rules,
            timeout_seconds: validating_rule.spec.0.timeout_seconds,
            client_config: WebhookClientConfig {
                ca_bundle: Some(k8s_openapi::ByteString(
                    ctx.config.ca_bundle.as_bytes().to_vec(),
                )),
                service: Some(ServiceReference {
                    namespace: ctx.config.service_namespace.clone(),
                    name: ctx.config.service_name.clone(),
                    path: Some(format!("/validate/{}", name)),
                    port: Some(443),
                }),
                url: None,
            },
            admission_review_versions: vec!["v1".to_string()],
            side_effects: "None".to_string(),
            ..Default::default()
        }]),
    };

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
    ctx: Arc<Data>,
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
    let mwc = MutatingWebhookConfiguration {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            owner_references: Some(vec![oref]),
            ..Default::default()
        },
        webhooks: Some(vec![MutatingWebhook {
            name: format!("{}.mutatingrule.checkpoint.devsisters.com", name),
            failure_policy: mutating_rule.spec.0.failure_policy.map(|fp| fp.to_string()),
            namespace_selector: mutating_rule.spec.0.namespace_selector,
            object_selector: mutating_rule.spec.0.object_selector,
            rules: mutating_rule.spec.0.object_rules,
            timeout_seconds: mutating_rule.spec.0.timeout_seconds,
            client_config: WebhookClientConfig {
                ca_bundle: Some(k8s_openapi::ByteString(
                    ctx.config.ca_bundle.as_bytes().to_vec(),
                )),
                service: Some(ServiceReference {
                    namespace: ctx.config.service_namespace.clone(),
                    name: ctx.config.service_name.clone(),
                    path: Some(format!("/mutate/{}", name)),
                    port: Some(443),
                }),
                url: None,
            },
            admission_review_versions: vec!["v1".to_string()],
            side_effects: "None".to_string(),
            ..Default::default()
        }]),
    };

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
pub fn error_policy(error: &Error, _ctx: Arc<Data>) -> Action {
    tracing::error!(%error);
    Action::requeue(Duration::from_secs(3))
}
