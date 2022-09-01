use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use checkpoint::config::WebhokConfig;

/// Generate future that awaits shutdown signal
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

    tracing::info!("terminate signal received");

    axum_server_handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config: WebhokConfig = envy::prefixed("CONF_").from_env()?;
    let kube_config = kube::Config::infer().await?;
    let client: kube::Client = kube_config.try_into()?;

    // Prepare HTTP app
    let http_app = checkpoint::handler::create_app(client);

    // Prepare TLS config for HTTPS serving
    let tls_config = RustlsConfig::from_pem_file(&config.cert_path, &config.key_path).await?;

    // Prepare shutdown signal futures
    let axum_server_handle = axum_server::Handle::new();
    let shutdown_signal_fut = shutdown_signal(axum_server_handle.clone());
    tokio::spawn(async move {
        shutdown_signal_fut.await;
    });

    // Spawn HTTP server
    tracing::info!("starting web server...");
    axum_server::bind_rustls(config.listen_addr.parse()?, tls_config)
        .handle(axum_server_handle)
        .serve(http_app.into_make_service())
        .await?;
    tracing::info!("web server terminated");

    Ok(())
}
