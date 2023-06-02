use anyhow::{Context, Result};
use futures_util::{stream::FuturesOrdered, TryFutureExt, TryStreamExt};
use kube::{
    api::ListParams,
    core::{DynamicObject, GroupVersionKind},
    discovery::ApiResource,
    Api,
};
use mlua::{Lua, Value};

use crate::{lua::lua_to_value, types::policy::CronPolicyResource};

pub async fn resources_to_lua_values<'lua>(
    lua: &'lua Lua,
    kube_client: kube::Client,
    resources: &[CronPolicyResource],
) -> Result<Vec<Value<'lua>>> {
    resources
        .iter()
        .map(|resource| {
            let gvk = GroupVersionKind::gvk(&resource.group, &resource.version, &resource.kind);
            let ar = if let Some(plural) = &resource.plural {
                ApiResource::from_gvk_with_plural(&gvk, plural)
            } else {
                ApiResource::from_gvk(&gvk)
            };
            let api = if let Some(namespace) = &resource.namespace {
                Api::<DynamicObject>::namespaced_with(kube_client.clone(), namespace, &ar)
            } else {
                Api::<DynamicObject>::all_with(kube_client.clone(), &ar)
            };

            let lua = &lua;
            async move {
                if let Some(name) = &resource.name {
                    let object = api
                        .get_opt(name)
                        .await
                        .context("failed to get Kubernetes object")?;
                    lua_to_value(lua, &object).context("failed to convert object to Lua value")
                } else {
                    let lp = if let Some(lp) = &resource.list_params {
                        ListParams {
                            label_selector: lp.label_selector.clone(),
                            field_selector: lp.field_selector.clone(),
                            ..Default::default()
                        }
                    } else {
                        Default::default()
                    };
                    let objects = api
                        .list(&lp)
                        .await
                        .context("failed to list Kubernetes objects")?
                        .items;
                    lua_to_value(lua, &objects).context("failed to convert objects into Lua value")
                }
            }
        })
        .collect::<FuturesOrdered<_>>()
        .try_collect()
        .err_into()
        .await
}
