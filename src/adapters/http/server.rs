use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};
use axum::Json; // For JSON request/response
use axum::body::Body as AxumBody; // Keep AxumBody
use axum::extract::State; // To access shared state in handlers
use axum::routing::post; // For the new POST route
use axum::{
    Router,
    http::Request,
    response::{IntoResponse, Response as AxumResponse},
};
use axum_server::tls_rustls::RustlsConfig;
use http_body_util::BodyExt;
use hyper::StatusCode;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex; // For health_checker_handle
use tower_http::trace::TraceLayer;

use crate::HealthChecker;
use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient;
use crate::adapters::http_handler::HyperHandler;
use crate::config::models::ServerConfig; // Ensure this is the correct path
use crate::core::ProxyService;
use crate::ports::http_server::{HandlerError, HttpHandler, HttpServer}; // Import HealthChecker

// This helper function is similar to the one in main.rs
// It's duplicated here for simplicity, or could be moved to a shared module.
fn spawn_health_checker_task_from_server(
    proxy_service_to_use: Arc<ProxyService>,
    http_client_clone: Arc<HyperHttpClient>,
    config_for_health_check: Arc<ServerConfig>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if config_for_health_check.health_check.enabled {
            tracing::info!(
                "(API Reload) Health checker task started. Interval: {}s, Path: {}, Unhealthy Threshold: {}, Healthy Threshold: {}",
                config_for_health_check.health_check.interval_secs,
                config_for_health_check.health_check.path,
                config_for_health_check.health_check.unhealthy_threshold,
                config_for_health_check.health_check.healthy_threshold
            );
            let health_checker = HealthChecker::new(proxy_service_to_use, http_client_clone);
            if let Err(e) = health_checker.run().await {
                tracing::error!("(API Reload) Health checker run error: {}", e);
            }
        } else {
            tracing::info!(
                "(API Reload) Health checking is disabled. Health checker task not running."
            );
        }
    })
}

// Define a struct to hold all shared state for Axum handlers
#[derive(Clone)]
struct AppState {
    proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
    config_holder: Arc<RwLock<Arc<ServerConfig>>>,
    http_client: Arc<HyperHttpClient>,
    file_system: Arc<TowerFileSystem>,
    health_checker_handle: Arc<TokioMutex<Option<tokio::task::JoinHandle<()>>>>,
}

pub struct HyperServer {
    app_state: AppState,
}

impl HyperServer {
    pub fn with_dependencies(
        proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
        config_holder: Arc<RwLock<Arc<ServerConfig>>>,
        http_client: Arc<HyperHttpClient>,
        file_system: Arc<TowerFileSystem>,
        health_checker_handle: Arc<TokioMutex<Option<tokio::task::JoinHandle<()>>>>, // Added
    ) -> Self {
        Self {
            app_state: AppState {
                proxy_service_holder,
                config_holder,
                http_client,
                file_system,
                health_checker_handle,
            },
        }
    }

    async fn build_app(&self) -> Router {
        // Note: HyperHandler will be modified to take Arc<RwLock<Arc<ProxyService>>>
        // So it can access the latest proxy_service internally on each request.
        // The instance of HyperHandler itself can be cloned.
        let general_handler = HyperHandler::new(
            self.app_state.proxy_service_holder.clone(), // Pass the holder
            self.app_state.http_client.clone(),
            self.app_state.file_system.clone(),
        );

        Router::new()
            .route("/-/config", post(update_config_handler)) // New API endpoint
            .fallback(move |req: Request<AxumBody>| handle_request(general_handler.clone(), req))
            .with_state(self.app_state.clone()) // Provide AppState to all routes
            .layer(TraceLayer::new_for_http())
    }
}

async fn update_config_handler(
    State(app_state): State<AppState>, // Access AppState using Axum's State extractor
    Json(new_config_payload): Json<ServerConfig>, // Expect JSON body parsed into ServerConfig
) -> Result<AxumResponse, AxumResponse> {
    // Return AxumResponse for both success and error
    tracing::info!("Received API request to update configuration.");

    let new_config_arc = Arc::new(new_config_payload);

    // 1. Update Config Holder
    {
        let mut config_w = app_state.config_holder.write().unwrap();
        *config_w = new_config_arc.clone();
        tracing::info!("(API Reload) Global ServerConfig Arc updated.");
    }

    // 2. Update ProxyService Holder
    let new_proxy_service = Arc::new(ProxyService::new(new_config_arc.clone()));
    {
        let mut proxy_s_w = app_state.proxy_service_holder.write().unwrap();
        *proxy_s_w = new_proxy_service.clone();
        tracing::info!("(API Reload) Global ProxyService Arc updated.");
    }

    // 3. Restart HealthChecker
    let mut handle_guard = app_state.health_checker_handle.lock().await;
    if let Some(old_handle) = handle_guard.take() {
        tracing::info!("(API Reload) Aborting previous health checker task...");
        old_handle.abort();
    }

    if new_config_arc.health_check.enabled {
        tracing::info!(
            "(API Reload) Starting new health checker task with updated configuration..."
        );
        *handle_guard = Some(spawn_health_checker_task_from_server(
            new_proxy_service.clone(),
            app_state.http_client.clone(),
            new_config_arc.clone(),
        ));
    } else {
        tracing::info!(
            "(API Reload) Health checking is disabled in the new configuration. Not starting health checker task."
        );
    }

    tracing::info!("(API Reload) Configuration updated and health checker managed successfully.");
    Ok((StatusCode::OK, "Configuration updated successfully").into_response())
}

impl HttpServer for HyperServer {
    async fn run(&self) -> Result<()> {
        let app = self.build_app().await;
        let current_config = self.app_state.config_holder.read().unwrap().clone();

        let addr: SocketAddr = current_config
            .listen_addr
            .parse()
            .with_context(|| format!("Invalid listen address: {}", current_config.listen_addr))?;

        if let Some(tls_config) = &current_config.tls {
            tracing::info!("Starting server with TLS on {}", addr);
            let cert_path = &tls_config.cert_path;
            let key_path = &tls_config.key_path;
            use tokio::fs;
            let cert_data = fs::read(cert_path)
                .await
                .with_context(|| format!("Failed to read certificate file: {}", cert_path))?;
            let key_data = fs::read(key_path)
                .await
                .with_context(|| format!("Failed to read key file: {}", key_path))?;
            let cert_chain: Vec<CertificateDer<'static>> =
                rustls_pemfile::certs(&mut cert_data.as_slice())
                    .collect::<Result<_, _>>()
                    .context("Failed to parse certificate PEM")?;
            let key_der: PrivateKeyDer<'static> =
                rustls_pemfile::private_key(&mut key_data.as_slice())
                    .with_context(|| format!("Failed to parse private key file: {}", key_path))?
                    .ok_or_else(|| anyhow!("No private key found in {}", key_path))?;

            CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
                .map_err(|e| anyhow!("Failed to install default crypto provider: {:?}", e))?;

            let server_config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(cert_chain, key_der)
                .context("Failed to create TLS server config")?;

            let tls_acceptor = RustlsConfig::from_config(Arc::new(server_config));

            axum_server::bind_rustls(addr, tls_acceptor)
                .serve(app.into_make_service())
                .await
                .map_err(|e| anyhow!("TLS Server error: {}", e))?;
        } else {
            tracing::info!("Starting server without TLS on {}", addr);
            let listener = TcpListener::bind(addr)
                .await
                .with_context(|| format!("Failed to bind to address: {}", addr))?;
            axum::serve(listener, app.into_make_service())
                .await
                .map_err(|e| anyhow!("HTTP Server error: {}", e))?;
        }

        Ok(())
    }
}

async fn handle_request(
    handler: HyperHandler, // This handler is created with a snapshot of ProxyService
    req: Request<AxumBody>,
) -> Result<AxumResponse, Infallible> {
    // The HyperHandler passed to this fallback does not automatically get updates
    // if ProxyService changes. This is a limitation of the current approach
    // where HyperHandler takes Arc<ProxyService> directly, not Arc<RwLock<Arc<ProxyService>>>.
    // For the fallback handler to use the latest ProxyService, it would need to access
    // the AppState or the proxy_service_holder, or HyperHandler itself would need to be redesigned
    // to internally hold Arc<RwLock<Arc<ProxyService>>> and read from it per request.
    // For now, requests to the fallback will use the ProxyService state from when build_app was last called.
    // The API endpoint for config update *does* update the shared state, so new *instances* of handlers
    // or systems querying the holders directly would see changes.

    match handler.handle_request(req).await {
        Ok(response) => {
            let (parts, hyper_body) = response.into_parts();
            let bytes = match hyper_body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(err) => {
                    tracing::error!("Failed to collect response body in handler: {}", err);
                    return Ok((StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
                        .into_response());
                }
            };
            let axum_body = AxumBody::from(bytes);
            Ok(AxumResponse::from_parts(parts, axum_body))
        }
        Err(e) => {
            let response = match e {
                HandlerError::RequestError(err) => {
                    tracing::error!("Request error: {}", err);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Request error: {}", err),
                    )
                        .into_response()
                }
            };
            Ok(response)
        }
    }
}
