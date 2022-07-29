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

#[derive(Serialize, Deserialize, JsonSchema, CustomResource, Clone, Debug)]
#[kube(
    group = "checkpoint.devsisters.com",
    version = "v1",
    kind = "ValidatingRule",
    shortname = "vr",
    status = "ValidatingRuleStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct ValidatingRuleSpec {
    /// FailurePolicy for ValidatingWebhookConfiguration.
    ///
    /// FailurePolicy defines how unrecognized errors from the admission endpoint are handled - allowed values are Ignore or Fail.
    /// Defaults to Fail.
    pub failure_policy: Option<FailurePolicy>,
    /// NamespaceSelector for ValidatingWebhookConfiguration.
    ///
    /// NamespaceSelector decides wheter to run the Rule on an object based on whether the namespace for that object matches the selector.
    pub namespace_selector: Option<LabelSelector>,
    /// ObjectSelector for ValidatingWebhookConfiguration
    ///
    /// ObjectSelector decides whether to run the Rule based on if the object has matching labels.
    /// Default to the empty LabelSelector, which matches everything.
    pub object_selector: Option<LabelSelector>,
    /// ObjectRules for Rules field in ValidatingWebhookConfiguration
    ///
    /// ObjectRules describes what operations on what resources/subresources the Rule cares about.
    /// Default to the empty LabelSelector, which matches everything.
    pub object_rules: Option<Vec<RuleWithOperations>>,
    /// TimeoutSeconds for ValidatingWebhookConfiguration.
    ///
    /// TimeoutSeconds specifies the timeout for this Rule.
    /// Default to 10 seconds.
    pub timeout_seconds: Option<i32>,

    /// Lua code to evaluate when validating request.
    pub code: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ValidatingRuleStatus {}

#[derive(Serialize, Deserialize, JsonSchema, CustomResource, Clone, Debug)]
#[kube(
    group = "checkpoint.devsisters.com",
    version = "v1",
    kind = "MutatingRule",
    shortname = "mr",
    status = "MutatingRuleStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct MutatingRuleSpec {
    /// FailurePolicy for MutatingWebhookConfiguration.
    ///
    /// FailurePolicy defines how unrecognized errors from the admission endpoint are handled - allowed values are Ignore or Fail.
    /// Defaults to Fail.
    pub failure_policy: Option<FailurePolicy>,
    /// NamespaceSelector for MutatingWebhookConfiguration.
    ///
    /// NamespaceSelector decides wheter to run the Rule on an object based on whether the namespace for that object matches the selector.
    pub namespace_selector: Option<LabelSelector>,
    /// ObjectSelector for MutatingWebhookConfiguration
    ///
    /// ObjectSelector decides whether to run the Rule based on if the object has matching labels.
    /// Default to the empty LabelSelector, which matches everything.
    pub object_selector: Option<LabelSelector>,
    /// ObjectRules for Rules field in MutatingWebhookConfiguration
    ///
    /// ObjectRules describes what operations on what resources/subresources the Rule cares about.
    /// Default to the empty LabelSelector, which matches everything.
    pub object_rules: Option<Vec<RuleWithOperations>>,
    /// TimeoutSeconds for MutatingWebhookConfiguration.
    ///
    /// TimeoutSeconds specifies the timeout for this Rule.
    /// Default to 10 seconds.
    pub timeout_seconds: Option<i32>,

    /// Lua code to evaluate when validating request.
    pub code: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MutatingRuleStatus {}
