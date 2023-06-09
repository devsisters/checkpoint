use std::collections::HashMap;

use anyhow::{Context, Result};
use futures_util::{stream::FuturesOrdered, TryFutureExt, TryStreamExt};
use interpolator::Formattable;
use kube::{
    api::ListParams,
    core::{DynamicObject, GroupVersionKind},
    discovery::ApiResource,
    Api,
};
use mlua::{Lua, Value};
use tracing::Instrument;

use crate::{
    lua::lua_to_value,
    types::policy::{
        CronPolicyNotification, CronPolicyNotificationSlack, CronPolicyNotificationWebhook,
        CronPolicyResource,
    },
};

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

pub async fn notify(
    policy_name: String,
    output: HashMap<String, String>,
    notifications: CronPolicyNotification,
) {
    let mut interpolator_context = output
        .iter()
        .map(|(key, value)| (format!("output.{}", key), Formattable::display(value)))
        .collect::<HashMap<_, _>>();
    interpolator_context.insert(
        "policy.name".to_string(),
        Formattable::display(&policy_name),
    );
    let interpolator_context = interpolator_context;

    if let Some(slack_notification) = notifications.slack {
        let slack_span = tracing::info_span!("notify-slack", %policy_name);
        let res = notify_slack(&interpolator_context, slack_notification)
            .instrument(slack_span)
            .await;
        if let Err(error) = res {
            tracing::error!(%policy_name, %error, "Failed to notify slack");
        }
    }
    if let Some(webhook_notification) = notifications.webhook {
        let slack_span = tracing::info_span!("notify-webhook", %policy_name);
        let res = notify_webhook(&interpolator_context, webhook_notification)
            .instrument(slack_span)
            .await;
        if let Err(error) = res {
            tracing::error!(%policy_name, %error, "Failed to notify webhook");
        }
    }
}

async fn notify_slack(
    context: &HashMap<String, Formattable<'_>>,
    config: CronPolicyNotificationSlack,
) -> Result<()> {
    // TODO:
    Ok(())
}

async fn notify_webhook(
    context: &HashMap<String, Formattable<'_>>,
    config: CronPolicyNotificationWebhook,
) -> Result<()> {
    // TODO:
    Ok(())
}
