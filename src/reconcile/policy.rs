use std::{collections::BTreeMap, sync::Arc};

use futures_util::{stream::FuturesUnordered, TryStreamExt};
use k8s_openapi::{
    api::{
        batch::v1::{CronJob, CronJobSpec, JobSpec, JobTemplateSpec},
        core::v1::{Container, EnvVar, PodSpec, PodTemplateSpec, ServiceAccount},
        rbac::v1::{
            ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef, Subject,
        },
    },
    apimachinery::pkg::apis::meta::v1::OwnerReference,
};
use kube::{
    api::{Patch, PatchParams},
    core::ObjectMeta,
    runtime::controller::Action,
    Api, Resource, ResourceExt,
};

use crate::{
    config::ControllerConfig,
    types::policy::{CronPolicy, CronPolicyResource, CronPolicySpec},
    util::find_group_version_pairs_by_kind,
};

use super::ReconcilerContext;

const CRONPOLICY_OWNED_LABEL_KEY: &str = "checkpoint.devsisters.com/cronpolicy";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to patch ServiceAccount: {0}")]
    PatchServiceAccount(#[source] kube::Error),
    #[error("Failed to patch Role: {0}")]
    PatchRole(#[source] kube::Error),
    #[error("Failed to patch RoleBinding: {0}")]
    PatchRoleBinding(#[source] kube::Error),
    #[error("Failed to patch ClusterRole: {0}")]
    PatchClusterRole(#[source] kube::Error),
    #[error("Failed to patch ClusterRoleBinding: {0}")]
    PatchClusterRoleBinding(#[source] kube::Error),
    #[error("Failed to patch CronJob: {0}")]
    PatchCronJob(#[source] kube::Error),
    #[error("Failed to serialize resources (This is a bug): {0}")]
    SerializeResources(#[source] serde_json::Error),
    #[error("Failed to serialize notifications (This is a bug): {0}")]
    SerializeNotifications(#[source] serde_json::Error),
    #[error("Kubernetes error: {0}")]
    Kubernetes(#[source] kube::Error),
    #[error("Specifed kind (`{0}`) does not have matching group/versions")]
    GroupVersionNotExists(String),
    #[error("Specifed kind (`{0}`) has multiple matching group/versions")]
    MultipleGroupVersion(String),
}

/// Set a label that indicates the object is owned by a CronPolicy
fn make_labels(name: String) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert(CRONPOLICY_OWNED_LABEL_KEY.to_string(), name);
    labels
}

fn make_cronjob(
    cp_name: String,
    namespace: String,
    oref: OwnerReference,
    spec: &CronPolicySpec,
    controller_config: &ControllerConfig,
) -> Result<CronJob, Error> {
    Ok(CronJob {
        metadata: ObjectMeta {
            name: Some(cp_name.clone()),
            namespace: Some(namespace),
            owner_references: Some(vec![oref]),
            labels: Some(make_labels(cp_name.clone())),
            ..Default::default()
        },
        spec: Some(CronJobSpec {
            suspend: Some(spec.suspend),
            schedule: spec.schedule.clone(),
            job_template: JobTemplateSpec {
                metadata: None,
                spec: Some(JobSpec {
                    template: PodTemplateSpec {
                        metadata: None,
                        spec: Some(PodSpec {
                            service_account_name: Some(cp_name.clone()),
                            containers: vec![Container {
                                command: Some(vec!["checkpoint-checker".to_string()]),
                                env: Some(vec![
                                    EnvVar {
                                        name: "RUST_LOG".to_string(),
                                        value: Some("info".to_string()),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "CONF_POLICY_NAME".to_string(),
                                        value: Some(cp_name),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "CONF_RESOURCES".to_string(),
                                        value: Some(
                                            serde_json::to_string(&spec.resources)
                                                .map_err(Error::SerializeResources)?,
                                        ),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "CONF_CODE".to_string(),
                                        value: Some(spec.code.clone()),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "CONF_NOTIFICATIONS".to_string(),
                                        value: Some(
                                            serde_json::to_string(&spec.notifications)
                                                .map_err(Error::SerializeNotifications)?,
                                        ),
                                        value_from: None,
                                    },
                                ]),
                                image: Some(controller_config.checker_image.clone()),
                                name: "checkpoint-checker".to_string(),
                                ..Default::default()
                            }],
                            restart_policy: Some(spec.restart_policy.to_string()),
                            ..Default::default()
                        }),
                    },
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        status: Default::default(),
    })
}

fn make_serviceaccount(name: String, namespace: String, oref: OwnerReference) -> ServiceAccount {
    ServiceAccount {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace),
            owner_references: Some(vec![oref]),
            labels: Some(make_labels(name)),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Simple pluralizer.
/// Duplicating the code from kube (without special casing) because it's simple enough.
/// Irregular plurals must be explicitly specified.
///
/// Source: https://github.com/kube-rs/kube/blob/da6b5e7b963bd6f72190a23b428abdf5e321141d/kube-derive/src/custom_resource.rs#L563-L592
fn to_plural(word: &str) -> String {
    // Words ending in s, x, z, ch, sh will be pluralized with -es (eg. foxes).
    if word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with('z')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        return format!("{}es", word);
    }

    // Words ending in y that are preceded by a consonant will be pluralized by
    // replacing y with -ies (eg. puppies).
    if word.ends_with('y') {
        if let Some(c) = word.chars().nth(word.len() - 2) {
            if !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') {
                // Remove 'y' and add `ies`
                let mut chars = word.chars();
                chars.next_back();
                return format!("{}ies", chars.as_str());
            }
        }
    }

    // All other words will have "s" added to the end (eg. days).
    format!("{}s", word)
}

async fn make_role_rules(
    resources: &[CronPolicyResource],
    kube_client: kube::Client,
) -> Result<Vec<PolicyRule>, Error> {
    resources
        .iter()
        .map(|resource| {
            let kube_client = kube_client.clone();
            async move {
                let group = if let Some(group) = &resource.group {
                    group.clone()
                } else {
                    let gvs = find_group_version_pairs_by_kind(&resource.kind, true, kube_client)
                        .await
                        .map_err(Error::Kubernetes)?;
                    if gvs.is_empty() {
                        return Err(Error::GroupVersionNotExists(resource.kind.clone()));
                    } else if gvs.len() > 1 {
                        return Err(Error::MultipleGroupVersion(resource.kind.clone()));
                    } else {
                        let mut gvs = gvs;
                        let gv = gvs.pop().unwrap();
                        gv.0
                    }
                };
                Ok(PolicyRule {
                    api_groups: Some(vec![group]),
                    resources: Some(vec![resource
                        .plural
                        .clone()
                        .unwrap_or_else(|| to_plural(&resource.kind.to_ascii_lowercase()))]),
                    verbs: vec![if resource.name.is_some() {
                        "get".to_string()
                    } else {
                        "list".to_string()
                    }],
                    resource_names: resource.name.clone().map(|name| vec![name]),
                    ..Default::default()
                })
            }
        })
        .collect::<FuturesUnordered<_>>()
        .try_collect()
        .await
}

async fn make_clusterrole(
    name: String,
    oref: OwnerReference,
    resources: &[CronPolicyResource],
    kube_client: kube::Client,
) -> Result<ClusterRole, Error> {
    Ok(ClusterRole {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            owner_references: Some(vec![oref]),
            labels: Some(make_labels(name)),
            ..Default::default()
        },
        rules: Some(make_role_rules(resources, kube_client).await?),
        aggregation_rule: None,
    })
}

fn make_clusterrolebinding(
    name: String,
    oref: OwnerReference,
    serviceaccount_namespace: String,
) -> ClusterRoleBinding {
    ClusterRoleBinding {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            owner_references: Some(vec![oref]),
            labels: Some(make_labels(name.clone())),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: ClusterRole::group(&()).into_owned(),
            kind: ClusterRole::kind(&()).into_owned(),
            name: name.clone(),
        },
        subjects: Some(vec![Subject {
            api_group: Some(ServiceAccount::group(&()).into_owned()),
            kind: ServiceAccount::kind(&()).into_owned(),
            name,
            namespace: Some(serviceaccount_namespace),
        }]),
    }
}

async fn make_role(
    name: String,
    oref: OwnerReference,
    target_namespace: String,
    resources: &[CronPolicyResource],
    kube_client: kube::Client,
) -> Result<Role, Error> {
    Ok(Role {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(target_namespace),
            owner_references: Some(vec![oref]),
            labels: Some(make_labels(name)),
            ..Default::default()
        },
        rules: Some(make_role_rules(resources, kube_client).await?),
    })
}

fn make_rolebinding(
    name: String,
    oref: OwnerReference,
    target_namespace: String,
    serviceaccount_namespace: String,
) -> RoleBinding {
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(target_namespace),
            owner_references: Some(vec![oref]),
            labels: Some(make_labels(name.clone())),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: Role::group(&()).into_owned(),
            kind: Role::kind(&()).into_owned(),
            name: name.clone(),
        },
        subjects: Some(vec![Subject {
            api_group: Some(ServiceAccount::group(&()).into_owned()),
            kind: ServiceAccount::kind(&()).into_owned(),
            name,
            namespace: Some(serviceaccount_namespace),
        }]),
    }
}

type RolesAndClusterRoles = (
    Vec<(Role, RoleBinding)>,
    Option<(ClusterRole, ClusterRoleBinding)>,
);

async fn make_roles_and_clusterroles(
    cp_name: String,
    cronjob_namespace: String,
    oref: OwnerReference,
    resources: &[CronPolicyResource],
    kube_client: kube::Client,
) -> Result<RolesAndClusterRoles, Error> {
    let mut namespaced_resources = BTreeMap::<String, Vec<CronPolicyResource>>::new(); // namespace -> [resource] map
    let mut global_resources = Vec::<CronPolicyResource>::new();
    for resource in resources {
        if let Some(namespace) = &resource.namespace {
            // Aggregate namespaced resources with same namespace
            namespaced_resources
                .entry(namespace.clone())
                .or_default()
                .push(resource.clone());
        } else {
            global_resources.push(resource.clone());
        }
    }

    let roles = namespaced_resources
        .into_iter()
        .map(|(namespace, resources)| {
            let cp_name = cp_name.clone();
            let oref = oref.clone();
            let cronjob_namespace = cronjob_namespace.clone();
            let kube_client = kube_client.clone();
            async move {
                let r = make_role(
                    cp_name.clone(),
                    oref.clone(),
                    namespace.clone(),
                    &resources,
                    kube_client,
                )
                .await?;
                let rb = make_rolebinding(cp_name, oref, namespace, cronjob_namespace);
                Ok((r, rb))
            }
        })
        .collect::<FuturesUnordered<_>>()
        .try_collect()
        .await?;
    let clusterrole = if !global_resources.is_empty() {
        let cr = make_clusterrole(
            cp_name.clone(),
            oref.clone(),
            &global_resources,
            kube_client,
        )
        .await?;
        let crb = make_clusterrolebinding(cp_name, oref, cronjob_namespace);
        Some((cr, crb))
    } else {
        None
    };

    Ok((roles, clusterrole))
}

pub async fn reconcile_cronpolicy(
    cp: Arc<CronPolicy>,
    ctx: Arc<ReconcilerContext>,
) -> Result<Action, Error> {
    let client = &ctx.client;
    let config = &ctx.config;

    // Prepare Kubernetes object ownership reference
    let oref = cp.controller_owner_ref(&()).unwrap();

    let cp_name = cp.name_any();
    let cronjob_namespace = cp.spec.namespace.clone();

    // Prepare Kubernetes APIs
    let sa_api = Api::<ServiceAccount>::namespaced(client.clone(), &cronjob_namespace);
    let cr_api = Api::<ClusterRole>::all(client.clone());
    let crb_api = Api::<ClusterRoleBinding>::all(client.clone());
    let cj_api = Api::<CronJob>::namespaced(client.clone(), &cronjob_namespace);
    let patch_params = PatchParams::apply("cronpolicy.checkpoint.devsisters.com");

    // Create ServiceAccount for checker
    let sa = make_serviceaccount(cp_name.clone(), cronjob_namespace.clone(), oref.clone());
    sa_api
        .patch(&sa.name_any(), &patch_params, &Patch::Apply(&sa))
        .await
        .map_err(Error::PatchServiceAccount)?;

    // Create Role or ClusterRole for the checker ServiceAccount that allows chechker to list the target resources
    let (roles, clusterrole) = make_roles_and_clusterroles(
        cp_name.clone(),
        cronjob_namespace.clone(),
        oref.clone(),
        &cp.spec.resources,
        client.clone(),
    )
    .await?;
    for (r, rb) in roles {
        let r_api = Api::<Role>::namespaced(client.clone(), &r.namespace().unwrap());
        let rb_api = Api::<RoleBinding>::namespaced(client.clone(), &rb.namespace().unwrap());

        r_api
            .patch(&r.name_any(), &patch_params, &Patch::Apply(&r))
            .await
            .map_err(Error::PatchRole)?;
        rb_api
            .patch(&rb.name_any(), &patch_params, &Patch::Apply(&rb))
            .await
            .map_err(Error::PatchRoleBinding)?;
    }
    if let Some((cr, crb)) = clusterrole {
        cr_api
            .patch(&cr.name_any(), &patch_params, &Patch::Apply(&cr))
            .await
            .map_err(Error::PatchClusterRole)?;
        crb_api
            .patch(&crb.name_any(), &patch_params, &Patch::Apply(&crb))
            .await
            .map_err(Error::PatchClusterRoleBinding)?;
    }

    // Create CronJob of checker
    let cj = make_cronjob(cp_name.clone(), cronjob_namespace, oref, &cp.spec, config)?;
    cj_api
        .patch(&cj.name_any(), &patch_params, &Patch::Apply(&cj))
        .await
        .map_err(Error::PatchCronJob)?;

    Ok(Action::await_change())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_roles_and_clusterroles() {
        let cp_name = "cron-policy-name".to_string();
        let cronjob_namespace = "cron-policy-namespace".to_string();
        let oref = OwnerReference::default();
        let resources = Vec::new();

        let (roles, clusterrole) = make_roles_and_clusterroles(
            cp_name.clone(),
            cronjob_namespace.clone(),
            oref.clone(),
            &resources,
        );
        assert_eq!(roles, Vec::new());
        assert_eq!(clusterrole, None);

        let some_namespace = "some-namespace".to_string();
        let other_namespace = "other-namespace".to_string();
        let resources = vec![
            CronPolicyResource {
                group: "".to_string(),
                version: "v1".to_string(),
                kind: "Namespace".to_string(),
                plural: None,
                namespace: None,
                name: None,
                list_params: None,
            },
            CronPolicyResource {
                group: "".to_string(),
                version: "v1".to_string(),
                kind: "Pod".to_string(),
                plural: None,
                namespace: Some(some_namespace.clone()),
                name: None,
                list_params: None,
            },
            CronPolicyResource {
                group: "apps".to_string(),
                version: "v1".to_string(),
                kind: "Deployment".to_string(),
                plural: None,
                namespace: None,
                name: None,
                list_params: None,
            },
            CronPolicyResource {
                group: "apps".to_string(),
                version: "v1".to_string(),
                kind: "StatefulSet".to_string(),
                plural: None,
                namespace: Some(some_namespace.clone()),
                name: None,
                list_params: None,
            },
            CronPolicyResource {
                group: "apps".to_string(),
                version: "v1".to_string(),
                kind: "DaemonSet".to_string(),
                plural: None,
                namespace: Some(other_namespace.clone()),
                name: None,
                list_params: None,
            },
        ];

        let (roles, clusterrole) =
            make_roles_and_clusterroles(cp_name.clone(), cronjob_namespace, oref, &resources);
        assert_eq!(roles.len(), 2);
        let role = &roles[0].0;
        assert_eq!(role.name_any(), cp_name);
        assert_eq!(role.namespace(), Some(other_namespace));
        let rules = role.rules.clone().unwrap();
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert_eq!(rule.api_groups, Some(vec!["apps".to_string()]));
        assert_eq!(rule.resources, Some(vec!["daemonsets".to_string()]));
        let role = &roles[1];
        assert_eq!(role.0.name_any(), cp_name);
        assert_eq!(role.0.namespace(), Some(some_namespace));
        let rules = role.0.rules.clone().unwrap();
        assert_eq!(rules.len(), 2);
        let rule = &rules[0];
        assert_eq!(rule.api_groups, Some(vec!["".to_string()]));
        assert_eq!(rule.resources, Some(vec!["pods".to_string()]));
        let rule = &rules[1];
        assert_eq!(rule.api_groups, Some(vec!["apps".to_string()]));
        assert_eq!(rule.resources, Some(vec!["statefulsets".to_string()]));
        let clusterrole = clusterrole.unwrap().0;
        assert_eq!(clusterrole.name_any(), cp_name);
        assert_eq!(clusterrole.namespace(), None);
        let rules = clusterrole.rules.unwrap();
        assert_eq!(rules.len(), 2);
        let rule = &rules[0];
        assert_eq!(rule.api_groups, Some(vec!["".to_string()]));
        assert_eq!(rule.resources, Some(vec!["namespaces".to_string()]));
        let rule = &rules[1];
        assert_eq!(rule.api_groups, Some(vec!["apps".to_string()]));
        assert_eq!(rule.resources, Some(vec!["deployments".to_string()]));
    }
}
