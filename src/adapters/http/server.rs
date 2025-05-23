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
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex; // For health_checker_handle
use tower_http::trace::TraceLayer;

// HealthChecker import might be unused if spawn_health_checker_task_from_server was its only user
// use crate::HealthChecker;
use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient;
use crate::adapters::http_handler::HyperHandler;
use crate::config::models::ServerConfig; // Ensure this is the correct path
use crate::core::ProxyService;
use crate::ports::http_server::{HandlerError, HttpHandler, HttpServer}; // Import HealthChecker
use crate::utils::health_checker_utils::spawn_health_checker_task; // Import shared helper

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
            // TODO: Secure this endpoint. Add authentication/authorization.
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

    // Validate the incoming configuration payload.
    if new_config_payload.listen_addr.is_empty() {
        tracing::warn!("Validation failed: listen_addr is empty.");
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid config payload: listen_addr is required".to_string(),
        )
            .into_response());
    }
    if new_config_payload.routes.is_empty() {
        tracing::warn!("Validation failed: routes cannot be empty.");
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid config payload: At least one route must be configured".to_string(),
        )
            .into_response());
    }
    // Add more validation based on ServerConfigBuilder or other rules as needed.
    // For example, check TLS paths if TLS is enabled.
    if let Some(tls_config) = &new_config_payload.tls {
        if tls_config.cert_path.is_empty() || tls_config.key_path.is_empty() {
            tracing::warn!("Validation failed: TLS cert_path or key_path is empty.");
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid config payload: TLS cert_path and key_path are required if TLS is configured".to_string(),
            )
                .into_response());
        }
    }

    // TODO: Consider more comprehensive validation, perhaps by trying to build
    // a new ServerConfig using a builder pattern if that enforces all constraints,
    // or by creating a dedicated validation function in the config module.

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
        *handle_guard = Some(spawn_health_checker_task(
            new_proxy_service.clone(),
            app_state.http_client.clone(),
            new_config_arc.clone(),
            "API Reload".to_string(), // Pass as String
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

            // The crypto provider should be installed once globally, typically in main.rs.

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
    // The HyperHandler passed to this fallback now holds an RwLock and reads the latest
    // ProxyService state. This ensures that requests to the fallback handler use the most
    // up-to-date ProxyService configuration. The API endpoint for config updates continues
    // to update the shared state, allowing new instances of handlers or systems querying
    // the holders directly to see changes in real-time.
    match handler.handle_request(req).await {
        Ok(response) => {
            let (parts, hyper_body) = response.into_parts();
            // Stream the body directly instead of collecting it in memory
            let axum_body = AxumBody::new(hyper_body.map_err(|e| {
                // This error mapping is crucial. AxumBody expects an error type that implements Into<BoxError>.
                // hyper::Error (which BodyExt::map_err might produce from the underlying body stream)
                // needs to be converted. A simple way is to stringify it and box it.
                tracing::error!("Error streaming response body to client: {}", e);
                axum::BoxError::from(e) // Convert the error from the body stream
            }));
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
