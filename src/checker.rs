use std::collections::HashMap;

use anyhow::{Context, Result};
use futures_util::{stream::FuturesOrdered, TryFutureExt, TryStreamExt};
use http::{header::HeaderName, HeaderMap, HeaderValue, Method};
use interpolator::Formattable;
use kube::{
    api::ListParams,
    core::{DynamicObject, GroupVersionKind},
    discovery::ApiResource,
    Api,
};
use mlua::{Lua, Value};
use serde::Serialize;
use slack_blocks::{blocks::Section, text::ToSlackMarkdown, Block};
use tracing::Instrument;

use crate::{
    lua::lua_to_value,
    types::policy::{
        CronPolicyNotification, CronPolicyNotificationSlack, CronPolicyNotificationWebhook,
        CronPolicyNotificationWebhookMethod, CronPolicyResource,
    },
    util::find_group_version_pairs_by_kind,
};

async fn get_group_version_from_resource(
    resource: &CronPolicyResource,
    kube_client: kube::Client,
) -> Result<(String, String)> {
    if let Some(group) = &resource.group {
        if let Some(version) = &resource.version {
            return Ok((group.clone(), version.clone()));
        }
    }

    let gvs = find_group_version_pairs_by_kind(&resource.kind, true, kube_client)
        .await
        .context("failed to find API group and versions")?;

    if gvs.is_empty() {
        Err(anyhow::anyhow!(
            "specifed kind (`{}`) does not have matching group/versions",
            resource.kind
        ))
    } else if gvs.len() > 1 {
        Err(anyhow::anyhow!(
            "specifed kind (`{}`) has multiple matching group/versions",
            resource.kind
        ))
    } else {
        let mut gvs = gvs;
        let gv = gvs.pop().unwrap();
        Ok(gv)
    }
}

pub async fn resources_to_lua_values<'lua>(
    lua: &'lua Lua,
    kube_client: kube::Client,
    resources: &[CronPolicyResource],
) -> Result<Vec<Value<'lua>>> {
    resources
        .iter()
        .map(|resource| {
            let kube_client = kube_client.clone();
            let lua = &lua;
            async move {
                let (group, version) =
                    get_group_version_from_resource(resource, kube_client.clone()).await?;
                let gvk = GroupVersionKind::gvk(&group, &version, &resource.kind);
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
        let res = notify_slack(&policy_name, &interpolator_context, slack_notification)
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

#[derive(Serialize)]
struct SlackReq<'a> {
    text: String,
    blocks: Vec<Block<'a>>,
}

async fn notify_slack(
    policy_name: &str,
    context: &HashMap<String, Formattable<'_>>,
    config: CronPolicyNotificationSlack,
) -> Result<()> {
    let message = interpolator::format(&config.message, context)
        .context("failed to make Slack message from template")?;
    let blocks = vec![Section::builder().text(message.markdown()).build().into()];
    let body = SlackReq {
        text: format!("{} is firing", policy_name),
        blocks,
    };

    let client = reqwest::Client::new();
    client
        .post(config.webhook_url)
        .json(&body)
        .send()
        .await
        .context("failed to request to Slack webhook")?;

    Ok(())
}

async fn notify_webhook(
    context: &HashMap<String, Formattable<'_>>,
    config: CronPolicyNotificationWebhook,
) -> Result<()> {
    let method = match config.method {
        CronPolicyNotificationWebhookMethod::Get => Method::GET,
        CronPolicyNotificationWebhookMethod::Head => Method::HEAD,
        CronPolicyNotificationWebhookMethod::Post => Method::POST,
        CronPolicyNotificationWebhookMethod::Put => Method::PUT,
        CronPolicyNotificationWebhookMethod::Delete => Method::DELETE,
        CronPolicyNotificationWebhookMethod::Connect => Method::CONNECT,
        CronPolicyNotificationWebhookMethod::Options => Method::OPTIONS,
        CronPolicyNotificationWebhookMethod::Trace => Method::TRACE,
        CronPolicyNotificationWebhookMethod::Patch => Method::PATCH,
    };
    let mut headers = HeaderMap::<HeaderValue>::with_capacity(config.headers.len());
    for (name, value) in config.headers {
        headers.insert(
            HeaderName::from_lowercase(name.to_lowercase().as_bytes())
                .context("failed to parse header name")?,
            value.parse().context("failed to parse header value")?,
        );
    }
    let body =
        interpolator::format(&config.body, context).context("failed to make body from template")?;

    let client = reqwest::Client::new();
    client
        .request(method, config.url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .context("failed to request to webhook")?;

    Ok(())
}
