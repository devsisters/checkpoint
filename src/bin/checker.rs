use std::collections::HashMap;

use anyhow::{Context, Result};

use checkpoint::{
    checker::{notify, resources_to_lua_values},
    config::CheckerConfig,
    lua::prepare_lua_ctx,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = CheckerConfig::try_from_env().context("failed to parse config from env")?;
    let kube_config = kube::Config::infer()
        .await
        .context("failed to infer Kubernetes config")?;
    let kube_client: kube::Client = kube_config
        .try_into()
        .context("failed to make Kubernetes client")?;

    let lua = prepare_lua_ctx().context("failed to prepare Lua context")?;

    let resource_values = resources_to_lua_values(&lua, kube_client, &config.resources).await?;
    let resource_values = mlua::MultiValue::from_vec(resource_values);

    let lua_chunk = lua
        .load(&config.code)
        .set_name("checker code")
        .context("failed to load Lua code")?;

    let output: Option<HashMap<String, String>> = lua_chunk
        .call(resource_values)
        .context("failed to run Lua code")?;

    if let Some(output) = output {
        notify(config.policy_name, output, config.notifications).await;
    }

    Ok(())
}
