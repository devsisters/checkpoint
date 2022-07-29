mod config;
mod handler;
mod reconcile;
mod types;

use std::sync::Arc;

use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use futures_util::stream::StreamExt;
use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use kube::{api::Api, runtime::Controller};

async fn shutdown_signal(axum_server_handle: axum_server::Handle) {
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

    axum_server_handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let client = kube::Client::try_default().await?;

    let http_app = crate::handler::create_app(client.clone());
    let tls_config = RustlsConfig::from_pem_file(
        &crate::config::CONFIG.cert_path,
        &crate::config::CONFIG.key_path,
    )
    .await?;

    let axum_server_handle = axum_server::Handle::new();
    let shutdown_signal_fut = shutdown_signal(axum_server_handle.clone());
    let (shutdown_signal_broadcast_tx, mut shutdown_signal_broadcast_rx1) =
        tokio::sync::broadcast::channel::<()>(1);
    let mut shutdown_signal_broadcast_rx2 = shutdown_signal_broadcast_tx.subscribe();
    tokio::spawn(async move {
        shutdown_signal_fut.await;
        let _ = shutdown_signal_broadcast_tx.send(());
    });

    let http_handle = tokio::spawn(
        axum_server::bind_rustls(crate::config::CONFIG.listen_addr.parse()?, tls_config)
            .handle(axum_server_handle)
            .serve(http_app.into_make_service()),
    );

    let vr_api = Api::<crate::types::ValidatingRule>::all(client.clone());
    let vwc_api = Api::<ValidatingWebhookConfiguration>::all(client.clone());
    let mr_api = Api::<crate::types::MutatingRule>::all(client.clone());
    let mwc_api = Api::<MutatingWebhookConfiguration>::all(client.clone());

    let vr_controller_handle = tokio::spawn(
        Controller::new(vr_api, Default::default())
            .owns(vwc_api, Default::default())
            .graceful_shutdown_on(async move {
                let _ = shutdown_signal_broadcast_rx1.recv().await;
            })
            .run(
                crate::reconcile::reconcile_validatingrule,
                crate::reconcile::error_policy,
                Arc::new(crate::reconcile::Data {
                    client: client.clone(),
                }),
            )
            .for_each(|res| async move {
                match res {
                    Ok(object) => tracing::info!(?object, "reconciled"),
                    Err(error) => tracing::error!(%error, "reconcile failed"),
                }
            }),
    );

    let mr_controller_handle = tokio::spawn(
        Controller::new(mr_api, Default::default())
            .owns(mwc_api, Default::default())
            .graceful_shutdown_on(async move {
                let _ = shutdown_signal_broadcast_rx2.recv().await;
            })
            .run(
                crate::reconcile::reconcile_mutatingrule,
                crate::reconcile::error_policy,
                Arc::new(crate::reconcile::Data { client }),
            )
            .for_each(|res| async move {
                match res {
                    Ok(object) => tracing::info!(?object, "reconciled"),
                    Err(error) => tracing::error!(%error, "reconcile failed"),
                }
            }),
    );

    let (res, (), ()) = tokio::try_join!(http_handle, vr_controller_handle, mr_controller_handle)?;
    res?;

    Ok(())
}
