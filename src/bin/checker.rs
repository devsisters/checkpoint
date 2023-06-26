use std::collections::HashMap;

use anyhow::{Context, Result};

use checkpoint::{
    checker::{fetch_resources, notify, prepare_js_runtime},
    config::CheckerConfig,
    js::eval,
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

    // Fetch resources
    let resources = fetch_resources(kube_client, &config.resources).await?;

    // Set up runtime
    let mut js_runtime =
        prepare_js_runtime(resources).context("failed to prepare JavaScript runtime")?;

    js_runtime
        .execute_script("<checkpoint>", config.code.into())
        .context("failed to execute JavaScript code")?;

    let output: Option<HashMap<String, String>> =
        eval(&mut js_runtime, "__checkpoint_get_context(\"output\")")
            .context("failed to evaluate JavaScript code")?;

    if let Some(output) = output {
        notify(config.policy_name, output, config.notifications).await;
    }

    Ok(())
}
