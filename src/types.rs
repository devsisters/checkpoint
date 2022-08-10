use std::fmt;

use k8s_openapi::{
    api::admissionregistration::v1::RuleWithOperations,
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase")]
pub enum FailurePolicy {
    #[default]
    Fail,
    Ignore,
}

impl fmt::Display for FailurePolicy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Fail => write!(f, "Fail"),
            Self::Ignore => write!(f, "Ignore"),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceAccountInfo {
    pub namespace: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RuleSpec {
    /// FailurePolicy for webhook configuration.
    ///
    /// FailurePolicy defines how unrecognized errors from the admission endpoint are handled - allowed values are Ignore or Fail.
    /// Defaults to Fail.
    pub failure_policy: Option<FailurePolicy>,
    /// NamespaceSelector for webhook configuration.
    ///
    /// NamespaceSelector decides wheter to run the Rule on an object based on whether the namespace for that object matches the selector.
    pub namespace_selector: Option<LabelSelector>,
    /// ObjectSelector for webhook configuration.
    ///
    /// ObjectSelector decides whether to run the Rule based on if the object has matching labels.
    /// Default to the empty LabelSelector, which matches everything.
    pub object_selector: Option<LabelSelector>,
    /// ObjectRules for Rules field in webhook configuration.
    ///
    /// ObjectRules describes what operations on what resources/subresources the Rule cares about.
    /// Default to the empty LabelSelector, which matches everything.
    pub object_rules: Option<Vec<RuleWithOperations>>,
    /// TimeoutSeconds for webhook configuration..
    ///
    /// TimeoutSeconds specifies the timeout for this Rule.
    /// Default to 10 seconds.
    pub timeout_seconds: Option<i32>,

    /// The name of ServiceAccount to use to run Lua code.
    pub service_account: Option<ServiceAccountInfo>,

    /// Lua code to evaluate when validating request.
    pub code: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RuleStatus {}

#[derive(Serialize, Deserialize, JsonSchema, CustomResource, Clone, Debug)]
#[kube(
    group = "checkpoint.devsisters.com",
    version = "v1",
    kind = "ValidatingRule",
    shortname = "vr",
    status = "ValidatingRuleStatus"
)]
#[serde(transparent)]
pub struct ValidatingRuleSpec(pub RuleSpec);

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(transparent)]
pub struct ValidatingRuleStatus(pub RuleStatus);

#[derive(Serialize, Deserialize, JsonSchema, CustomResource, Clone, Debug)]
#[kube(
    group = "checkpoint.devsisters.com",
    version = "v1",
    kind = "MutatingRule",
    shortname = "mr",
    status = "MutatingRuleStatus"
)]
pub struct MutatingRuleSpec(pub RuleSpec);

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(transparent)]
pub struct MutatingRuleStatus(pub RuleStatus);
