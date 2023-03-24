use std::sync::Arc;

use anyhow::Result;
use futures_util::{
    future::try_join,
    stream::{FuturesUnordered, StreamExt, TryStreamExt},
};
use k8s_openapi::{
    api::admissionregistration::v1::{
        MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
    },
    ByteString,
};
use kube::{
    api::{Api, ListParams, Patch, PatchParams},
    runtime::Controller,
    ResourceExt,
};
use stopper::Stopper;
use tokio::sync::{broadcast::Sender, RwLock};

use checkpoint::config::ControllerConfig;

/// Generate future that awaits shutdown signal
async fn shutdown_signal(shutdown_signal_broadcast_tx: Sender<()>, stopper: Stopper) {
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

    tracing::info!("terminate signal received");

    let _ = shutdown_signal_broadcast_tx.send(());
    stopper.stop();
}

async fn reload_ca_bundle(
    config: &ControllerConfig,
    vwc_api: &Api<ValidatingWebhookConfiguration>,
    mwc_api: &Api<MutatingWebhookConfiguration>,
    ca_bundle_lock: &RwLock<ByteString>,
) -> Result<()> {
    let ca_bundle = tokio::fs::read_to_string(&config.ca_bundle_path).await?;
    let ca_bundle = k8s_openapi::ByteString(ca_bundle.as_bytes().to_vec());

    {
        let current_ca_bundle = ca_bundle_lock.read().await;
        if ca_bundle == *current_ca_bundle {
            tracing::info!("TLS CA bundle is not changed. Skipping reload...");
            return Ok(());
        }
    }

    {
        let mut ca_bundle_lock_write = ca_bundle_lock.write().await;
        *ca_bundle_lock_write = ca_bundle.clone();
    }

    let vwcs = vwc_api
        .list(&ListParams::default().labels(checkpoint::reconcile::VALIDATINGRULE_OWNED_LABEL_KEY))
        .await?
        .items;
    let mwcs = mwc_api
        .list(&ListParams::default().labels(checkpoint::reconcile::MUTATINGRULE_OWNED_LABEL_KEY))
        .await?
        .items;

    macro_rules! patch {
        ($wcs:expr, $api:expr, $manager:literal) => {
            $wcs.into_iter()
                .map(|mut wc| {
                    // Mark WebhookConfiguration to be updated.
                    // Then the controller watching the WC will reconcile with new CA bundle
                    let annotations = wc.annotations_mut();
                    annotations.insert(
                        checkpoint::reconcile::SHOULD_UPDATE_ANNOTATION_KEY.to_string(),
                        "true".to_string(),
                    );
                    async move {
                        wc.metadata.managed_fields = None;
                        $api.patch(
                            &wc.name_any(),
                            &PatchParams::apply($manager),
                            &Patch::Apply(&wc),
                        )
                        .await
                        .map(|_| ())
                    }
                })
                .collect::<FuturesUnordered<_>>()
                .try_collect::<()>()
        };
    }

    let vwcs_patch = patch!(vwcs, vwc_api, "validatingrule.checkpoint.devsisters.com");
    let mwcs_patch = patch!(mwcs, mwc_api, "mutatingrule.checkpoint.devsisters.com");
    try_join(vwcs_patch, mwcs_patch).await?;

    tracing::info!("TLS CA bundle reloaded");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = ControllerConfig::try_from_env()?;
    let kube_config = kube::Config::infer().await?;
    let default_namespace = kube_config.default_namespace.clone();
    let client: kube::Client = kube_config.try_into()?;

    // Prepare shutdown signal futures
    let stopper = Stopper::new();
    let (shutdown_signal_broadcast_tx, mut shutdown_signal_broadcast_rx1) =
        tokio::sync::broadcast::channel::<()>(1);
    let mut shutdown_signal_broadcast_rx2 = shutdown_signal_broadcast_tx.subscribe();
    let mut shutdown_signal_broadcast_rx3 = shutdown_signal_broadcast_tx.subscribe();
    let shutdown_signal_fut = shutdown_signal(shutdown_signal_broadcast_tx, stopper.clone());
    tokio::spawn(async move {
        shutdown_signal_fut.await;
    });

    // Leader election
    // Acquire lease
    tracing::info!("attempting to acquire leader lease...");
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
    tracing::info!("acquired lease");

    tracing::info!("spawning controllers...");

    let ca_bundle = tokio::fs::read_to_string(&config.ca_bundle_path).await?;
    let ca_bundle = ByteString(ca_bundle.as_bytes().to_vec());
    let ca_bundle = Arc::new(RwLock::new(ca_bundle));

    // Prepare Kubernetes APIs
    let vr_api = Api::<checkpoint::types::rule::ValidatingRule>::all(client.clone());
    let vwc_api = Api::<ValidatingWebhookConfiguration>::all(client.clone());
    let mr_api = Api::<checkpoint::types::rule::MutatingRule>::all(client.clone());
    let mwc_api = Api::<MutatingWebhookConfiguration>::all(client.clone());

    // Prepare TLS CA bundle reloader
    let mut watcher = checkpoint::filewatcher::FileWatcher::new(
        {
            let config = config.clone();
            let ca_bundle = ca_bundle.clone();
            let vwc_api = vwc_api.clone();
            let mwc_api = mwc_api.clone();
            move |_| {
                let config = config.clone();
                let ca_bundle = ca_bundle.clone();
                let vwc_api = vwc_api.clone();
                let mwc_api = mwc_api.clone();
                async move {
                    tracing::info!("Reloading TLS CA bundle");
                    let res = reload_ca_bundle(&config, &vwc_api, &mwc_api, &ca_bundle).await;
                    if let Err(error) = res {
                        tracing::error!(%error, "Failed to reload CA bundle");
                    }
                }
            }
        },
        10,
        stopper,
    );
    watcher.watch(config.ca_bundle_path.clone());
    watcher.spawn()?;

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
                Arc::new(checkpoint::reconcile::ReconcilerContext {
                    client: client.clone(),
                    config: config.clone(),
                    ca_bundle: ca_bundle.clone(),
                }),
            )
            .for_each(|res| async move {
                match res {
                    Ok(object) => tracing::info!(?object, "reconciled"),
                    Err(error) => tracing::error!(%error, "reconcile failed"),
                }
            }),
    );
    tracing::info!("spawned validatingrule controller");

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
                Arc::new(checkpoint::reconcile::ReconcilerContext {
                    client,
                    config,
                    ca_bundle,
                }),
            )
            .for_each(|res| async move {
                match res {
                    Ok((object, _)) => tracing::info!(name = object.name, "reconciled"),
                    Err(error) => tracing::error!(%error, "reconcile failed"),
                }
            }),
    );
    tracing::info!("spawned mutatingrule controller");

    // Await all spawned futures
    let res = tokio::try_join!(vr_controller_handle, mr_controller_handle);
    tracing::info!("controllers terminated");

    tracing::info!("releasing lease...");
    // Release lease
    lease.join().await?;
    tracing::info!("lease released");

    // Unwrap result
    res?;

    Ok(())
}
