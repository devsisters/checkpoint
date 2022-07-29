use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::admissionregistration::v1::{
    ServiceReference, ValidatingWebhook, ValidatingWebhookConfiguration, WebhookClientConfig,
};
use kube::{
    api::{ObjectMeta, Patch, PatchParams},
    runtime::controller::Action,
    Api, Resource,
};
use thiserror::Error;

use crate::{config::CONFIG, types::ValidatingRule};

pub struct Data {
    pub client: kube::Client,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("MissingObjectKey: {0}")]
    MissingObjectKey(&'static str),
    #[error("Failed to create ValidatingWebhookConfiguration: {0}")]
    ValidatingWebhookConfigurationCreationFailed(#[source] kube::Error),
}

pub async fn reconcile(
    validating_rule: Arc<ValidatingRule>,
    ctx: Arc<Data>,
) -> Result<Action, Error> {
    let client = &ctx.client;
    let validating_rule = (*validating_rule).clone();
    let oref = validating_rule.controller_owner_ref(&()).unwrap();
    let name = validating_rule
        .metadata
        .name
        .ok_or(Error::MissingObjectKey(".metadata.name"))?;
    let vwc_name = format!("checkpoint-validatingrule-{}", name);
    let vwc_api = Api::<ValidatingWebhookConfiguration>::all(client.clone());

    let vwc = ValidatingWebhookConfiguration {
        metadata: ObjectMeta {
            name: Some(vwc_name.clone()),
            owner_references: Some(vec![oref]),
            ..Default::default()
        },
        webhooks: Some(vec![ValidatingWebhook {
            name: format!("{}.validatingrule.checkpoint.devsisters.com", name),
            failure_policy: validating_rule.spec.failure_policy.map(|fp| fp.to_string()),
            namespace_selector: validating_rule.spec.namespace_selector,
            object_selector: validating_rule.spec.object_selector,
            rules: validating_rule.spec.object_rules,
            timeout_seconds: validating_rule.spec.timeout_seconds,
            client_config: WebhookClientConfig {
                ca_bundle: Some(k8s_openapi::ByteString(
                    CONFIG.ca_bundle.as_bytes().to_vec(),
                )),
                service: Some(ServiceReference {
                    namespace: CONFIG.service_namespace.clone(),
                    name: CONFIG.service_name.clone(),
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

    vwc_api
        .patch(
            &vwc_name,
            &PatchParams::apply("validatingrule.checkpoint.devsisters.com"),
            &Patch::Apply(&vwc),
        )
        .await
        .map_err(Error::ValidatingWebhookConfigurationCreationFailed)?;

    Ok(Action::await_change())
}

pub fn error_policy(error: &Error, _ctx: Arc<Data>) -> Action {
    tracing::error!(%error);
    Action::requeue(Duration::from_secs(3))
}
