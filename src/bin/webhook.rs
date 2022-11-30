use std::path::Path;
use std::{io, net::SocketAddr};

use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use notify::{RecursiveMode, Watcher};
use stopper::Stopper;
use tokio::sync::mpsc;

use checkpoint::config::WebhookConfig;

/// Generate future that awaits shutdown signal
async fn shutdown_signal(axum_server_handle: axum_server::Handle, stopper: Stopper) {
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

    axum_server_handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
    stopper.stop();
}

async fn reload_config(config: &WebhookConfig, tls_config: &RustlsConfig) -> Result<(), io::Error> {
    tls_config
        .reload_from_pem_file(&config.cert_path, &config.key_path)
        .await
}

async fn reload_config_loop(
    mut receiver: mpsc::Receiver<()>,
    stopper: Stopper,
    config: WebhookConfig,
    tls_config: RustlsConfig,
) {
    while let Some(Some(())) = stopper.stop_future(receiver.recv()).await {
        let res = reload_config(&config, &tls_config).await;
        match res {
            Ok(_) => {
                tracing::info!("TLS certificate reloaded");
            }
            Err(error) => {
                tracing::error!(%error, "Failed to reload cert");
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = WebhookConfig::try_from_env()?;
    let kube_config = kube::Config::infer().await?;
    let client: kube::Client = kube_config.try_into()?;

    // Prepare HTTP app
    let http_app = checkpoint::handler::create_app(client);

    // Prepare TLS config for HTTPS serving
    let tls_config = RustlsConfig::from_pem_file(&config.cert_path, &config.key_path).await?;

    let stopper = Stopper::new();

    // Prepare TLS cert reloader
    let (watcher_sender, watcher_receiver) = mpsc::channel::<()>(10);
    tokio::spawn(reload_config_loop(
        watcher_receiver,
        stopper.clone(),
        config.clone(),
        tls_config.clone(),
    ));
    let mut watcher = notify::recommended_watcher(move |res| {
        tracing::info!("Reloading TLS certificate");
        match res {
            Ok(_) => {
                if watcher_sender.blocking_send(()).is_err() {
                    tracing::error!("Failed to send cert reload message");
                }
            }
            Err(error) => {
                tracing::error!(%error, "Failed to watch cert");
            }
        }
    })?;
    watcher.watch(Path::new(&config.cert_path), RecursiveMode::NonRecursive)?;
    watcher.watch(Path::new(&config.key_path), RecursiveMode::NonRecursive)?;

    // Prepare shutdown signal futures
    let axum_server_handle = axum_server::Handle::new();
    let shutdown_signal_fut = shutdown_signal(axum_server_handle.clone(), stopper);
    tokio::spawn(async move {
        shutdown_signal_fut.await;
    });

    // Spawn HTTP server
    tracing::info!("starting web server...");
    let listen_addr: SocketAddr = config.listen_addr.parse()?;
    tracing::info!("listening at {}...", listen_addr);
    axum_server::bind_rustls(listen_addr, tls_config)
        .handle(axum_server_handle)
        .serve(http_app.into_make_service())
        .await?;
    tracing::info!("web server terminated");

    Ok(())
}
