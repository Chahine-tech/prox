use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};
use axum::Json;
use axum::body::Body as AxumBody;
use axum::extract::{ConnectInfo, State};
use axum::routing::{get, post};
use axum::{
    Router,
    http::Request,
    response::{IntoResponse, Response as AxumResponse},
};
use axum_prometheus::PrometheusMetricLayer;
use axum_server::tls_rustls::RustlsConfig;
use http_body_util::BodyExt;
use hyper::StatusCode;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::sync::Mutex as TokioMutex;
use tower_http::trace::TraceLayer;

use crate::adapters::acme::AcmeService;
use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient;
use crate::adapters::http_handler::HyperHandler;
use crate::config::models::ServerConfig;
use crate::core::ProxyService;
use crate::metrics::{RequestTimer, increment_request_total};
use crate::ports::http_server::{HandlerError, HttpHandler, HttpServer};
use crate::utils::connection_tracker::{ConnectionInfo, ConnectionTracker};
use crate::utils::graceful_shutdown::{GracefulShutdown, ShutdownToken};
use crate::utils::health_checker_utils::spawn_health_checker_task;

// RAII guard for request tracking that automatically decrements on drop
struct ConnectionRequestGuard {
    connection_info: Arc<ConnectionInfo>,
}

impl Drop for ConnectionRequestGuard {
    fn drop(&mut self) {
        self.connection_info.decrement_requests();
    }
}

// Define a struct to hold all shared state for Axum handlers
#[derive(Clone)]
struct AppState {
    proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
    config_holder: Arc<RwLock<Arc<ServerConfig>>>,
    http_client: Arc<HyperHttpClient>,
    file_system: Arc<TowerFileSystem>,
    health_checker_handle: Arc<TokioMutex<Option<tokio::task::JoinHandle<()>>>>,
    connection_tracker: ConnectionTracker,
    shutdown_token: ShutdownToken,
}

pub struct HyperServer {
    app_state: AppState,
    prometheus_layer: PrometheusMetricLayer<'static>,
    prometheus_handle: PrometheusHandle,
    graceful_shutdown: Arc<GracefulShutdown>,
}

impl HyperServer {
    pub fn with_dependencies(
        proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
        config_holder: Arc<RwLock<Arc<ServerConfig>>>,
        http_client: Arc<HyperHttpClient>,
        file_system: Arc<TowerFileSystem>,
        health_checker_handle: Arc<TokioMutex<Option<tokio::task::JoinHandle<()>>>>,
        graceful_shutdown: Arc<GracefulShutdown>,
    ) -> Self {
        let (prometheus_layer, prometheus_handle) = PrometheusMetricLayer::pair();
        let connection_tracker = ConnectionTracker::new();
        let shutdown_token = graceful_shutdown.shutdown_token();

        Self {
            app_state: AppState {
                proxy_service_holder,
                config_holder,
                http_client,
                file_system,
                health_checker_handle,
                connection_tracker,
                shutdown_token,
            },
            prometheus_layer,
            prometheus_handle,
            graceful_shutdown,
        }
    }

    async fn build_app(&self) -> Router {
        let general_handler = HyperHandler::new(
            self.app_state.proxy_service_holder.clone(),
            self.app_state.http_client.clone(),
            self.app_state.file_system.clone(),
        );

        let metrics_handle_for_route = self.prometheus_handle.clone();

        // Clone app_state for use in the fallback closure
        let app_state_for_fallback = self.app_state.clone();

        Router::new()
            .route("/-/config", post(update_config_handler))
            .route(
                "/metrics",
                get(move || async move { metrics_handle_for_route.render() }),
            )
            .fallback(
                move |ConnectInfo(addr): ConnectInfo<SocketAddr>, req: Request<AxumBody>| {
                    let handler = general_handler.clone();
                    let app_state = app_state_for_fallback.clone();
                    async move {
                        let path = req.uri().path().to_string();
                        let method = req.method().to_string();

                        // Create a tracing span for the request
                        let span = tracing::info_span!(
                            "http_request",
                            http.method = %method,
                            http.path = %path,
                            http.status_code = tracing::field::Empty,
                            connection.remote_addr = %addr,
                        );

                        let _enter = span.enter();

                        // Create connection guard for tracking
                        let connection_info =
                            app_state.connection_tracker.register_connection(addr);
                        let _request_guard = {
                            connection_info.increment_requests();
                            // Use a custom guard that decrements on drop
                            ConnectionRequestGuard {
                                connection_info: connection_info.clone(),
                            }
                        };

                        // Timer will record duration when dropped
                        let _timer = RequestTimer::new(path.clone(), method.clone());

                        // Check if shutdown is requested
                        if app_state.shutdown_token.is_shutdown_requested() {
                            tracing::warn!("Rejecting new request due to shutdown in progress");
                            let response =
                                (StatusCode::SERVICE_UNAVAILABLE, "Server is shutting down")
                                    .into_response();
                            increment_request_total(&path, &method, response.status().as_u16());
                            return response;
                        }

                        // Await the actual response. Since the error type is Infallible,
                        // we can safely unwrap the Result.
                        let response = handle_request(handler, req, addr).await.unwrap();

                        // Record the status code in the span
                        tracing::Span::current()
                            .record("http.status_code", response.status().as_u16());

                        // Now 'response' is of type AxumResponse (http::Response<axum::body::Body>)
                        increment_request_total(&path, &method, response.status().as_u16());

                        // Return the response. AxumResponse implements IntoResponse.
                        // The request guard will automatically decrement the request count when dropped
                        response
                    }
                },
            )
            .with_state(self.app_state.clone())
            .layer(self.prometheus_layer.clone())
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
        if let (Some(cert_path), Some(key_path)) = (&tls_config.cert_path, &tls_config.key_path) {
            builder = builder.tls(cert_path.clone(), key_path.clone());
        } else if let Some(acme_config) = &tls_config.acme {
            builder = builder.acme(acme_config.clone());
        }
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

        // Create shutdown signal receiver
        let mut shutdown_receiver = self.graceful_shutdown.subscribe();
        let connection_tracker = self.app_state.connection_tracker.clone();

        if let Some(tls_config_data) = tls_config_opt_owned {
            // Handle both manual certificates and ACME
            let (cert_path, key_path) = if let Some(acme_config) = &tls_config_data.acme {
                if acme_config.enabled {
                    tracing::info!(
                        "ACME is enabled. Requesting certificate for domains: {:?}",
                        acme_config.domains
                    );

                    let acme_service = AcmeService::new(acme_config.clone())
                        .context("Failed to create ACME service")?;

                    let cert_info = acme_service
                        .get_certificate()
                        .await
                        .context("Failed to get ACME certificate")?;

                    // Start renewal task
                    acme_service.start_renewal_task();

                    tracing::info!(
                        "ACME certificate obtained: cert={}, key={}",
                        cert_info.cert_path,
                        cert_info.key_path
                    );
                    (cert_info.cert_path, cert_info.key_path)
                } else {
                    return Err(anyhow!("ACME is configured but not enabled"));
                }
            } else if let (Some(cert_path), Some(key_path)) =
                (&tls_config_data.cert_path, &tls_config_data.key_path)
            {
                tracing::info!(
                    "Using manual TLS certificates: cert={}, key={}",
                    cert_path,
                    key_path
                );
                (cert_path.clone(), key_path.clone())
            } else {
                return Err(anyhow!(
                    "TLS is configured but neither manual certificates nor ACME configuration is provided"
                ));
            };

            tracing::info!(
                "TLS is ENABLED. Certificate: {}, Key: {}",
                cert_path,
                key_path
            );
            let rustls_config = RustlsConfig::from_pem_file(&cert_path, &key_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to load TLS certificate/key from paths: cert='{}', key='{}'",
                        cert_path, key_path
                    )
                })?;

            // Run server with graceful shutdown
            let server_future = axum_server::bind_rustls(addr, rustls_config)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>());

            tokio::select! {
                result = server_future => {
                    result.context("TLS server error")?;
                }
                shutdown_reason = shutdown_receiver.recv() => {
                    match shutdown_reason {
                        Ok(reason) => {
                            tracing::info!("Server shutdown initiated: {:?}", reason);
                            // Signal connection tracker to start draining
                            connection_tracker.initiate_shutdown();

                            // Wait for connections to drain (with timeout)
                            let drain_timeout = std::time::Duration::from_secs(30);
                            if connection_tracker.drain_connections(drain_timeout).await {
                                tracing::info!("All connections drained successfully");
                            } else {
                                tracing::warn!("Connection drain timeout exceeded, forcing shutdown");
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error receiving shutdown signal: {}", e);
                        }
                    }
                }
            }
        } else {
            tracing::info!("TLS is DISABLED.");

            // Run server with graceful shutdown
            let server_future = axum_server::bind(addr)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>());

            tokio::select! {
                result = server_future => {
                    result.context("Server error")?;
                }
                shutdown_reason = shutdown_receiver.recv() => {
                    match shutdown_reason {
                        Ok(reason) => {
                            tracing::info!("Server shutdown initiated: {:?}", reason);
                            // Signal connection tracker to start draining
                            connection_tracker.initiate_shutdown();

                            // Wait for connections to drain (with timeout)
                            let drain_timeout = std::time::Duration::from_secs(30);
                            if connection_tracker.drain_connections(drain_timeout).await {
                                tracing::info!("All connections drained successfully");
                            } else {
                                tracing::warn!("Connection drain timeout exceeded, forcing shutdown");
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error receiving shutdown signal: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

async fn handle_request(
    handler: HyperHandler, // This handler is created with a snapshot of ProxyService
    req: Request<AxumBody>,
    _remote_addr: SocketAddr,
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
