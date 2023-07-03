//! JS helper functions for rules

use anyhow::Context;
use deno_core::op;
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use kube::{
    api::ListParams,
    config::AuthInfo,
    core::{DynamicObject, GroupVersionKind, ObjectList},
    discovery::ApiResource,
    Api,
};
use serde::Deserialize;

use crate::types::rule::ServiceAccountInfo;

deno_core::extension!(checkpoint_rule, ops = [ops_kube_get, ops_kube_list]);

/// Prepare Kubernetes client with specified ServiceAccount info in Rule spec
async fn prepare_kube_client(
    serviceaccount_info: Option<ServiceAccountInfo>,
    timeout_seconds: Option<i32>,
) -> anyhow::Result<kube::Client> {
    // Fail if ServiceAccountInfo is not provided
    let serviceaccount_info = serviceaccount_info.context(
        "serviceAccount field is not provided. You should provide serviceAccount field in Rule spec if you want to use `kubeGet` or `kubeList` function in JS code.",
    )?;

    let client = kube::Client::try_default()
        .await
        .context("failed to prepare Kubernetes client")?;

    let sa_api = Api::namespaced(client, &serviceaccount_info.namespace);

    // Retrieve token from ServiceAccount
    let tr = sa_api
        .create_token_request(
            &serviceaccount_info.name,
            &Default::default(),
            &TokenRequest {
                metadata: Default::default(),
                spec: TokenRequestSpec {
                    audiences: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
                    // expirationSeconds should greater than 10 minutes
                    expiration_seconds: Some(std::cmp::max(
                        timeout_seconds.unwrap_or(10 * 60).into(),
                        10 * 60,
                    )),
                    ..Default::default()
                },
                status: None,
            },
        )
        .await
        .map_err(|error| {
            if let kube::Error::Api(api_error) = &error {
                if api_error.code == 404 {
                    return anyhow::Error::new(error).context("ServiceAccount not found");
                }
            }
            anyhow::Error::new(error).context("failed to request to Kubernetes")
        })?;
    let token = tr.status.context("failed to request ServiceAccount")?.token;

    let mut kube_config =
        kube::Config::incluster().context("failed to get Kubernetes in-cluster config")?;

    // Set auth info with token
    kube_config.auth_info = AuthInfo {
        token: Some(secrecy::SecretString::new(token)),
        ..Default::default()
    };

    let new_client = kube::Client::try_from(kube_config)
        .context("failed to create restricted Kubernetes client")?;

    Ok(new_client)
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeGetArgument {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: Option<String>,
    pub namespace: Option<String>,
    pub name: String,
}

/// JS helper function to get a Kubernetes resource
#[op]
async fn ops_kube_get(
    serviceaccount_info: Option<ServiceAccountInfo>,
    timeout_seconds: Option<i32>,
    KubeGetArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        name,
    }: KubeGetArgument,
) -> anyhow::Result<Option<DynamicObject>> {
    // Prepare GroupVersionKind and ApiResource from argument
    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = prepare_kube_client(serviceaccount_info, timeout_seconds).await?;

    // Prepare Kubernetes API with or without namespace
    let api = if let Some(namespace) = namespace {
        Api::<DynamicObject>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<DynamicObject>::all_with(client, &ar)
    };

    // Get object
    let object = api
        .get_opt(&name)
        .await
        .context("failed to get from Kubernetes cluster")?;

    Ok(object)
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeListArgument {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: Option<String>,
    pub namespace: Option<String>,
    pub list_params: Option<KubeListArgumentListParams>,
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
pub enum KubeListArgumentListParamsVersionMatch {
    NotOlderThan,
    Exact,
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeListArgumentListParams {
    pub label_selector: Option<String>,
    pub field_selector: Option<String>,
    pub timeout: Option<u32>,
    pub limit: Option<u32>,
    pub continue_token: Option<String>,
    pub version_match: Option<KubeListArgumentListParamsVersionMatch>,
    pub resource_version: Option<String>,
}

/// JS helper function to list Kubernetes resources
#[op]
async fn ops_kube_list(
    serviceaccount_info: Option<ServiceAccountInfo>,
    timeout_seconds: Option<i32>,
    KubeListArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        list_params,
    }: KubeListArgument,
) -> anyhow::Result<ObjectList<DynamicObject>> {
    // Re-pack list params
    let list_params = list_params
        .map(
            |KubeListArgumentListParams {
                 label_selector,
                 field_selector,
                 timeout,
                 limit,
                 continue_token,
                 version_match,
                 resource_version,
             }| ListParams {
                label_selector,
                field_selector,
                timeout,
                limit,
                continue_token,
                version_match: version_match.map(|vm| match vm {
                    KubeListArgumentListParamsVersionMatch::NotOlderThan => {
                        kube::api::VersionMatch::NotOlderThan
                    }
                    KubeListArgumentListParamsVersionMatch::Exact => kube::api::VersionMatch::Exact,
                }),
                resource_version,
            },
        )
        .unwrap_or_default();

    // Prepare GroupVersionKind and ApiResource from argument
    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = prepare_kube_client(serviceaccount_info, timeout_seconds).await?;

    // Prepare Kubernetes API with or without namespace
    let api = if let Some(namespace) = namespace {
        Api::<DynamicObject>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<DynamicObject>::all_with(client, &ar)
    };

    // List objects
    let object_list = api
        .list(&list_params)
        .await
        .context("failed to list from Kubernetes cluster")?;

    Ok(object_list)
}
