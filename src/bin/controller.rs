use std::sync::Arc;

use anyhow::Result;
use futures_util::stream::StreamExt;
use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use kube::{api::Api, runtime::Controller};
use tokio::sync::broadcast::Sender;

use checkpoint::config::ControllerConfig;

/// Generate future that awaits shutdown signal
async fn shutdown_signal(shutdown_signal_broadcast_tx: Sender<()>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    let _ = shutdown_signal_broadcast_tx.send(());
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config: ControllerConfig = envy::prefixed("CONF_").from_env()?;
    let kube_config = kube::Config::infer().await?;
    let default_namespace = kube_config.default_namespace.clone();
    let client: kube::Client = kube_config.try_into()?;

    // Prepare shutdown signal futures
    let (shutdown_signal_broadcast_tx, mut shutdown_signal_broadcast_rx1) =
        tokio::sync::broadcast::channel::<()>(1);
    let mut shutdown_signal_broadcast_rx2 = shutdown_signal_broadcast_tx.subscribe();
    let mut shutdown_signal_broadcast_rx3 = shutdown_signal_broadcast_tx.subscribe();
    let shutdown_signal_fut = shutdown_signal(shutdown_signal_broadcast_tx);
    tokio::spawn(async move {
        shutdown_signal_fut.await;
    });

    // Leader election
    // Acquire lease
    let hostname = hostname::get()?;
    let hostname = hostname.to_string_lossy();
    let lease_fut = checkpoint::leader_election::Lease::acquire_or_create(
        client.clone(),
        &default_namespace,
        "checkpoint.devsisters.com",
        &hostname,
    );
    let lease = tokio::select! {
        lease = lease_fut => {
            lease?
        }
        _ = shutdown_signal_broadcast_rx3.recv() => {
            // Early exit when shutdown signal is received
            return Ok(());
        }
    };

    // Prepare Kubernetes APIs
    let vr_api = Api::<checkpoint::types::rule::ValidatingRule>::all(client.clone());
    let vwc_api = Api::<ValidatingWebhookConfiguration>::all(client.clone());
    let mr_api = Api::<checkpoint::types::rule::MutatingRule>::all(client.clone());
    let mwc_api = Api::<MutatingWebhookConfiguration>::all(client.clone());

    // Spawn ValidatingRule controller
    let vr_controller_handle = tokio::spawn(
        Controller::new(vr_api, Default::default())
            .owns(vwc_api, Default::default())
            .graceful_shutdown_on(async move {
                let _ = shutdown_signal_broadcast_rx1.recv().await;
            })
            .run(
                checkpoint::reconcile::reconcile_validatingrule,
                checkpoint::reconcile::error_policy,
                Arc::new(checkpoint::reconcile::Data {
                    client: client.clone(),
                    config: config.clone(),
                }),
            )
            .for_each(|res| async move {
                match res {
                    Ok(object) => tracing::info!(?object, "reconciled"),
                    Err(error) => tracing::error!(%error, "reconcile failed"),
                }
            }),
    );

    // Spawn MutatingRule controller
    let mr_controller_handle = tokio::spawn(
        Controller::new(mr_api, Default::default())
            .owns(mwc_api, Default::default())
            .graceful_shutdown_on(async move {
                let _ = shutdown_signal_broadcast_rx2.recv().await;
            })
            .run(
                checkpoint::reconcile::reconcile_mutatingrule,
                checkpoint::reconcile::error_policy,
                Arc::new(checkpoint::reconcile::Data { client, config }),
            )
            .for_each(|res| async move {
                match res {
                    Ok(object) => tracing::info!(?object, "reconciled"),
                    Err(error) => tracing::error!(%error, "reconcile failed"),
                }
            }),
    );

    // Await all spawned futures
    let res = tokio::try_join!(vr_controller_handle, mr_controller_handle);

    // Release lease
    lease.join().await?;

    // Unwrap result
    res?;

    Ok(())
}
