use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use axum::Json;
use axum::body::Body as AxumBody;
use axum::extract::State;
use axum::routing::post;
use axum::{
    Router,
    http::Request,
    response::{IntoResponse, Response as AxumResponse},
};
use axum_server::tls_rustls::RustlsConfig;
use http_body_util::BodyExt;
use hyper::StatusCode;
use tokio::sync::Mutex as TokioMutex;
use tower_http::trace::TraceLayer;

use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient;
use crate::adapters::http_handler::HyperHandler;
use crate::config::models::ServerConfig;
use crate::core::ProxyService;
use crate::ports::http_server::{HandlerError, HttpHandler, HttpServer};
use crate::utils::health_checker_utils::spawn_health_checker_task;

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
        health_checker_handle: Arc<TokioMutex<Option<tokio::task::JoinHandle<()>>>>,
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
            self.app_state.proxy_service_holder.clone(),
            self.app_state.http_client.clone(),
            self.app_state.file_system.clone(),
        );

        Router::new()
            // TODO: Secure this endpoint. Add authentication/authorization.
            .route("/-/config", post(update_config_handler))
            .fallback(move |req: Request<AxumBody>| handle_request(general_handler.clone(), req))
            .with_state(self.app_state.clone())
            .layer(TraceLayer::new_for_http())
    }
}

async fn update_config_handler(
    State(app_state): State<AppState>,
    Json(new_config_payload): Json<ServerConfig>,
) -> Result<AxumResponse, AxumResponse> {
    tracing::info!("Received API request to update configuration.");

    // Validate the incoming configuration payload using ServerConfigBuilder.
    let mut builder = ServerConfig::builder()
        .listen_addr(new_config_payload.listen_addr.clone()) // Clone to avoid moving from new_config_payload
        .health_check(new_config_payload.health_check.clone());

    for (prefix, route_config) in new_config_payload.routes.iter() {
        builder = builder.route(prefix.clone(), route_config.clone());
    }

    if let Some(tls_config) = &new_config_payload.tls {
        builder = builder.tls(tls_config.cert_path.clone(), tls_config.key_path.clone());
    }

    for (backend, path) in new_config_payload.backend_health_paths.iter() {
        builder = builder.backend_health_path(backend.clone(), path.clone());
    }

    if let Err(validation_err) = builder.build() {
        tracing::warn!("Validation failed: {}", validation_err);
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid config payload: {}", validation_err),
        )
            .into_response());
    }

    // If validation passes, proceed with the validated config (new_config_payload can be used directly
    // as its structure matches ServerConfig, and builder was primarily for validation here)
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
            "API Reload".to_string(),
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

        // Read values from config_guard and then drop it
        let (listen_addr_str, tls_config_opt_owned) = {
            let config_guard = self.app_state.config_holder.read().unwrap();
            let addr_str = config_guard.listen_addr.clone();
            let tls_opt = config_guard.tls.clone(); // Clone the Option<TlsConfig>
            (addr_str, tls_opt)
        }; // config_guard is dropped here

        let addr = listen_addr_str.parse::<SocketAddr>().with_context(|| {
            format!(
                "Failed to parse listen address: \\\"{}\\\"",
                listen_addr_str
            )
        })?;

        tracing::info!("Server listening on {}", addr);

        if let Some(tls_config_data) = tls_config_opt_owned { // Use the owned Option
            tracing::info!(
                "TLS is ENABLED. Certificate: {}, Key: {}",
                tls_config_data.cert_path,
                tls_config_data.key_path
            );
            let rustls_config = RustlsConfig::from_pem_file(
                &tls_config_data.cert_path,
                &tls_config_data.key_path,
            )
            .await // This await is now safe
            .with_context(|| {
                format!(
                    "Failed to load TLS certificate/key from paths: cert='{}', key='{}'",
                    tls_config_data.cert_path, tls_config_data.key_path
                )
            })?;
            axum_server::bind_rustls(addr, rustls_config)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await // This await is now safe
                .context("Rustls server error")?;
        } else {
            tracing::info!("TLS is DISABLED.");
            axum_server::bind(addr)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await // This await is now safe
                .context("Server error")?;
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
                axum::BoxError::from(e)
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
                HandlerError::InternalError(err) => {
                    tracing::error!("Internal error: {}", err);
                    (StatusCode::INTERNAL_SERVER_ERROR, err).into_response()
                }
                HandlerError::BadGateway(err) => {
                    tracing::error!("Bad gateway: {}", err);
                    (StatusCode::BAD_GATEWAY, err).into_response()
                }
                HandlerError::GatewayTimeout(err) => {
                    tracing::error!("Gateway timeout: {}", err);
                    (StatusCode::GATEWAY_TIMEOUT, err).into_response()
                }
                HandlerError::BadRequest(err) => {
                    tracing::error!("Bad request: {}", err);
                    (StatusCode::BAD_REQUEST, err).into_response()
                }
            };
            Ok(response)
        }
    }
}
