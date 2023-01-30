use std::fmt;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

/// List param to select the resources.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CronPolicyResourceListParams {
    /// Optional selector to restrict the resources by their labels. List all if not specified.
    #[serde(default)]
    pub label_selector: Option<String>,
    /// Optional selector to restrict the resources by their fields. List all if not specified.
    #[serde(default)]
    pub field_selector: Option<String>,
}

/// Specifier for the resources to check.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CronPolicyResource {
    /// API group the resources belong to.
    pub group: String,
    /// API version the resources belong to.
    pub version: String,
    /// Kind of the resources.
    pub kind: String,
    /// Optional plural name. Use inferred from kind if not specified.
    #[serde(default)]
    pub plural: Option<String>,
    /// Optional Namespace name of the resources. List from all Namespaces if not specified.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Optional name of the resources. If name is not specified, the checker will list all resources. If name is specified, the checker will get the specific resource.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional list params to list the resources.
    #[serde(default)]
    pub list_params: Option<CronPolicyResourceListParams>,
}

fn default_cronpolicyspec_namespace() -> String {
    "default".to_string()
}

/// Restart policy for all containers within the pod. One of OnFailure, Never. More info: https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/#restart-policy
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub enum RestartPolicy {
    OnFailure,
    Never,
}

impl fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::OnFailure => write!(f, "OnFailure"),
            Self::Never => write!(f, "Never"),
        }
    }
}

/// Configuration of a webhook to notify when policy check failed.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct Webhook {
    /// Url of the webhook
    pub url: Url,
    /// Body template of the webhook
    pub template: String,
}

/// CronPolicies check the specified resources with the provided Lua code periodically.
#[derive(Serialize, Deserialize, JsonSchema, CustomResource, Clone, Debug)]
#[kube(
    group = "checkpoint.devsisters.com",
    version = "v1",
    kind = "CronPolicy",
    shortname = "cp",
    status = "CronPolicyStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct CronPolicySpec {
    /// This flag tells the controller to suspend subsequent executions, it does not apply to already started executions.  Defaults to false.
    #[serde(default)]
    pub suspend: bool,
    /// The schedule in Cron format, see https://en.wikipedia.org/wiki/Cron.
    pub schedule: String,

    /// Specifier for the resources to check.
    pub resources: Vec<CronPolicyResource>,
    /// Lua code to evaluate on the resources.
    pub code: String,
    /// Configuration of a webhook to notify when policy check failed.
    pub webhook: Webhook,

    /// Namespace name for the CronJob.  Defaults to "default".
    #[serde(default = "default_cronpolicyspec_namespace")]
    pub namespace: String,
    /// Restart policy for all containers within the pod. One of OnFailure, Never. More info: https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/#restart-policy
    pub restart_policy: RestartPolicy,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct CronPolicyStatus {}
