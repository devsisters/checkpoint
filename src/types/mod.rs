pub mod rule;
pub mod testcase;

use std::borrow::Cow;

use kube::{
    core::{ObjectMeta, TypeMeta},
    discovery::ApiResource,
    Resource,
};
use serde::{Deserialize, Serialize};

/// Some resource (e.g. PodExecOptions) does not have `metadata`. But `kube` crate expects all resources have `metadata`.
/// So we create a custom `DynamicObject` that can use default `ObjectMeta` when deserializing.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct DynamicObjectWithOptionalMetadata {
    /// The type fields, not always present
    #[serde(flatten, default)]
    pub types: Option<TypeMeta>,
    /// Object metadata
    #[serde(default)]
    pub metadata: ObjectMeta,

    /// All other keys
    #[serde(flatten)]
    pub data: serde_json::Value,
}

impl Resource for DynamicObjectWithOptionalMetadata {
    type DynamicType = ApiResource;

    fn group(dt: &ApiResource) -> Cow<'_, str> {
        dt.group.as_str().into()
    }

    fn version(dt: &ApiResource) -> Cow<'_, str> {
        dt.version.as_str().into()
    }

    fn kind(dt: &ApiResource) -> Cow<'_, str> {
        dt.kind.as_str().into()
    }

    fn api_version(dt: &ApiResource) -> Cow<'_, str> {
        dt.api_version.as_str().into()
    }

    fn plural(dt: &ApiResource) -> Cow<'_, str> {
        dt.plural.as_str().into()
    }

    fn meta(&self) -> &ObjectMeta {
        &self.metadata
    }

    fn meta_mut(&mut self) -> &mut ObjectMeta {
        &mut self.metadata
    }
}
