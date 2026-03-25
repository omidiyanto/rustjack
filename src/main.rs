use axum::{
    Router,
    routing::{get, post},
};
use std::net::SocketAddr;
use tracing::info;

mod tls;
mod webhook;

async fn healthz() -> &'static str {
    "OK"
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received SIGINT (Ctrl+C), initiating graceful shutdown...");
        },
        _ = terminate => {
            info!("Received SIGTERM, initiating graceful shutdown...");
        },
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let svc_name = std::env::var("SERVICE_NAME").unwrap_or_else(|_| "rustjack".to_string());
    let namespace = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "cert-manager".to_string());
    let webhook_name =
        std::env::var("WEBHOOK_NAME").unwrap_or_else(|_| "rustjack-webhook".to_string());
    let secret_name =
        std::env::var("TLS_SECRET_NAME").unwrap_or_else(|_| format!("{}-tls", svc_name));

    let client = kube::Client::try_default()
        .await
        .expect("Failed to create K8s client");

    let initial_tls =
        tls::initialize_tls(&client, &svc_name, &namespace, &webhook_name, &secret_name).await;

    let config = axum_server::tls_rustls::RustlsConfig::from_pem(
        initial_tls.0.clone(),
        initial_tls.1.clone(),
    )
    .await
    .expect("Failed to load Rustls configuration");

    let tls_config_handle = config.clone();
    let client_clone = client.clone();

    tokio::spawn(async move {
        tls::start_ha_tls_manager(
            client_clone,
            tls_config_handle,
            namespace,
            svc_name,
            webhook_name,
            secret_name,
            initial_tls,
        )
        .await;
    });

    let app = Router::new()
        .route("/mutate", post(webhook::mutate_handler))
        .route("/healthz", get(healthz));
    let addr = SocketAddr::from(([0, 0, 0, 0], 8443));

    info!("RustJack v1.1.0 is ready and listening on {}", addr);

    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();

    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
    });

    axum_server::bind_rustls(addr, config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .expect("Server crashed");
}