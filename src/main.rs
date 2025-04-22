use axum::{
    extract::Extension,
    http::{uri::Uri, Request, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use clap::Parser;
use dashmap::DashMap;
use hyper::{Body, Client};
use hyper::client::connect::HttpConnector as HyperHttpConnector;
use hyper_tls::HttpsConnector;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::time::Duration;
use std::{convert::Infallible, net::SocketAddr, sync::Arc};
use tokio::fs;
use tower::{ServiceBuilder, ServiceExt};
use tower_http::services::ServeDir;

// Use HTTPS-capable client
type HyperClient = Client<HttpsConnector<HyperHttpConnector>>;

#[derive(Debug, Serialize, Deserialize)]
struct ServerConfig {
    listen_addr: String,
    routes: HashMap<String, RouteConfig>,
    #[serde(default)]
    tls: Option<TlsConfig>,
    #[serde(default)]
    health_check: HealthCheckConfig,
    #[serde(default)]
    backend_health_paths: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TlsConfig {
    cert_path: String,
    key_path: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
struct HealthCheckConfig {
    enabled: bool,
    interval_secs: u64,
    timeout_secs: u64,
    path: String,
    unhealthy_threshold: u32,
    healthy_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 10,
            timeout_secs: 2,
            path: "/health".to_string(),
            unhealthy_threshold: 3,
            healthy_threshold: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum RouteConfig {
    #[serde(rename = "static")]
    Static { root: String },
    #[serde(rename = "redirect")]
    Redirect {
        target: String,
        status_code: Option<u16>,
    },
    #[serde(rename = "proxy")]
    Proxy { target: String },
    #[serde(rename = "load_balance")]
    LoadBalance {
        targets: Vec<String>,
        strategy: LoadBalanceStrategy,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum LoadBalanceStrategy {
    #[serde(rename = "round_robin")]
    RoundRobin,
    #[serde(rename = "random")]
    Random,
}

#[derive(Parser, Debug)]
#[clap(author, version, about)]
struct Args {
    #[clap(short, long, default_value = "config.yaml")]
    config: String,
}

struct AppState {
    routes: HashMap<String, RouteConfig>,
    client: HyperClient,
    // For round-robin load balancing
    counter: std::sync::atomic::AtomicUsize,
    // For health checking
    backend_health: Arc<DashMap<String, BackendHealth>>,
    health_config: HealthCheckConfig,
    backend_paths: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthStatus {
    Healthy,
    Unhealthy,
}

#[derive(Debug)]
struct BackendHealth {
    status: std::sync::atomic::AtomicU8, // 0 = Unhealthy, 1 = Healthy
    consecutive_successes: std::sync::atomic::AtomicU32,
    consecutive_failures: std::sync::atomic::AtomicU32,
}

impl BackendHealth {
    fn new(_target: String) -> Self {
        Self {
            status: std::sync::atomic::AtomicU8::new(1), // Start as healthy
            consecutive_successes: std::sync::atomic::AtomicU32::new(0),
            consecutive_failures: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn status(&self) -> HealthStatus {
        if self.status.load(std::sync::atomic::Ordering::Relaxed) == 1 {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        }
    }

    fn mark_healthy(&self) {
        self.status.store(1, std::sync::atomic::Ordering::Relaxed);
        self.consecutive_failures
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    fn mark_unhealthy(&self) {
        self.status.store(0, std::sync::atomic::Ordering::Relaxed);
        self.consecutive_successes
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Parse command line arguments
    let args = Args::parse();

    // Load configuration
    tracing::info!("Loading configuration from {}", args.config);
    let config_content = match fs::read_to_string(&args.config).await {
        Ok(content) => content,
        Err(err) => {
            tracing::error!(
                "Failed to read config file: {}, error: {}",
                args.config,
                err
            );
            return Err(anyhow::anyhow!("Failed to read config file: {}", err));
        }
    };

    let config: ServerConfig = match serde_yaml::from_str(&config_content) {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("Failed to parse config: {}", err);
            return Err(anyhow::anyhow!("Failed to parse config: {}", err));
        }
    };

    // Parse listen address
    let addr: SocketAddr = match config.listen_addr.parse() {
        Ok(addr) => addr,
        Err(err) => {
            tracing::error!(
                "Invalid listen address: {}, error: {}",
                config.listen_addr,
                err
            );
            return Err(anyhow::anyhow!("Invalid listen address: {}", err));
        }
    };

    // Setup HTTP client for proxying with HTTPS support
    let https = HttpsConnector::new();
    let client = Client::builder().build::<_, Body>(https);

    // Initialize backend health tracking
    let backend_health = Arc::new(DashMap::new());
    
    // Collect all backend targets
    let backends = collect_backends(&config.routes);
    
    // Initialize health status for all backends
    for backend in &backends {
        backend_health.insert(backend.clone(), BackendHealth::new(backend.clone()));
    }

    // Create shared state
    let state = Arc::new(AppState {
        routes: config.routes,
        client,
        counter: std::sync::atomic::AtomicUsize::new(0),
        backend_health,
        health_config: config.health_check.clone(),
        backend_paths: config.backend_health_paths.clone(),
    });

    // Log configured routes
    for (prefix, route) in &state.routes {
        tracing::info!("Configured route: {} -> {:?}", prefix, route);
    }

    // Start health checking if enabled
    if config.health_check.enabled && !backends.is_empty() {
        let health_check_state = state.clone();
        tokio::spawn(async move {
            health_checker(health_check_state).await;
        });
    }

    // Build our application with a route
    let app = Router::new().fallback(handle_request).layer(
        ServiceBuilder::new()
            .layer(Extension(state))
            .layer(tower_http::trace::TraceLayer::new_for_http()),
    );

    // Start the server
    tracing::info!("Server listening on {}", addr);
    println!("Server listening on {}", addr);

    // Check if TLS is configured
    if let Some(tls_config) = &config.tls {
        // Start HTTPS server
        tracing::info!("Starting server with TLS");

        // Load certificate and private key
        let cert_path = &tls_config.cert_path;
        let key_path = &tls_config.key_path;

        let cert = match fs::read(cert_path).await {
            Ok(cert) => cert,
            Err(err) => {
                tracing::error!(
                    "Failed to read certificate file: {}, error: {}",
                    cert_path,
                    err
                );
                return Err(anyhow::anyhow!("Failed to read certificate file: {}", err));
            }
        };

        let key = match fs::read(key_path).await {
            Ok(key) => key,
            Err(err) => {
                tracing::error!(
                    "Failed to read private key file: {}, error: {}",
                    key_path,
                    err
                );
                return Err(anyhow::anyhow!("Failed to read private key file: {}", err));
            }
        };

        // Parse the certificate and private key
        let cert_chain = match rustls_pemfile::certs(&mut cert.as_slice()) {
            Ok(certs) => certs.into_iter().map(rustls::Certificate).collect(),
            Err(err) => {
                tracing::error!("Failed to parse certificate: {}", err);
                return Err(anyhow::anyhow!("Failed to parse certificate: {}", err));
            }
        };

        let mut keys = match rustls_pemfile::pkcs8_private_keys(&mut key.as_slice()) {
            Ok(keys) => keys,
            Err(err) => {
                tracing::error!("Failed to parse PKCS8 private key: {}", err);
                // Try RSA key format if PKCS8 fails
                match rustls_pemfile::rsa_private_keys(&mut key.as_slice()) {
                    Ok(rsa_keys) => rsa_keys,
                    Err(err) => {
                        tracing::error!("Failed to parse RSA private key: {}", err);
                        return Err(anyhow::anyhow!("Failed to parse private key: {}", err));
                    }
                }
            }
        };

        if keys.is_empty() {
            tracing::error!("No private keys found in the key file");
            return Err(anyhow::anyhow!("No private keys found in the key file"));
        }

        let server_key = rustls::PrivateKey(keys.remove(0));

        // Configure TLS
        let tls_config = rustls::ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(cert_chain, server_key)
            .map_err(|err| {
                tracing::error!("TLS configuration error: {}", err);
                anyhow::anyhow!("TLS configuration error: {}", err)
            })?;

        // Create server configuration
        let tls_acceptor = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

        // Start TLS server
        tracing::info!("Starting secure server on {}", addr);
        axum_server::bind_rustls(addr, tls_acceptor)
            .serve(app.into_make_service())
            .await
            .map_err(|err| {
                tracing::error!("Server error: {}", err);
                anyhow::anyhow!("Server error: {}", err)
            })?;

        Ok(())
    } else {
        // Standard HTTP server
        tracing::info!("Starting server without TLS");
        axum::Server::bind(&addr)
            .serve(app.into_make_service())
            .await
            .map_err(|err| {
                tracing::error!("Server error: {}", err);
                anyhow::anyhow!("Server error: {}", err)
            })?;

        Ok(())
    }
}

// Collect all load balanced targets for health checking
fn collect_backends(routes: &HashMap<String, RouteConfig>) -> Vec<String> {
    let mut backends = Vec::new();
    
    for route_config in routes.values() {
        match route_config {
            RouteConfig::LoadBalance { targets, .. } => {
                backends.extend(targets.clone());
            }
            RouteConfig::Proxy { target } => {
                backends.push(target.clone());
            }
            _ => {}
        }
    }
    
    // Deduplicate backends
    backends.sort();
    backends.dedup();
    backends
}

async fn handle_request(
    Extension(state): Extension<Arc<AppState>>,
    req: Request<Body>,
) -> Result<Response, Infallible> {
    let uri = req.uri().clone();
    let path = uri.path();

    // Find matching route
    let mut matched_route = None;
    let mut matched_prefix = "";

    for (route_prefix, route_config) in &state.routes {
        if path.starts_with(route_prefix) && route_prefix.len() > matched_prefix.len() {
            matched_route = Some(route_config);
            matched_prefix = route_prefix;
        }
    }

    let response = match matched_route {
        Some(route_config) => match route_config {
            RouteConfig::Static { root } => handle_static(root, matched_prefix, req).await,
            RouteConfig::Redirect {
                target,
                status_code,
            } => handle_redirect(target, path, matched_prefix, *status_code).await,
            RouteConfig::Proxy { target } => {
                handle_proxy(&state.client, target, req, matched_prefix).await
            }
            RouteConfig::LoadBalance { targets, strategy } => {
                handle_load_balance(&state, targets, strategy, req, matched_prefix).await
            }
        },
        None => {
            // No route matched
            (StatusCode::NOT_FOUND, "Not Found").into_response()
        }
    };

    Ok(response)
}

async fn handle_static(root: &str, prefix: &str, req: Request<Body>) -> Response {
    let path = req.uri().path();
    let rel_path = &path[prefix.len()..];

    // Create a new request with the path adjusted for ServeDir
    let uri_string = format!("/{}", rel_path.trim_start_matches('/'));
    let uri = Uri::try_from(uri_string).unwrap_or_else(|_| Uri::default());

    let (parts, body) = req.into_parts();
    let mut new_req = Request::from_parts(parts, body);
    *new_req.uri_mut() = uri;

    // Use ServeDir from tower-http
    let serve_dir = ServeDir::new(root);

    match serve_dir.oneshot(new_req).await {
        Ok(response) => response.into_response(),
        Err(err) => {
            tracing::error!("Static file error: {:?}", err);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}

async fn handle_redirect(
    target: &str,
    path: &str,
    prefix: &str,
    status_code: Option<u16>,
) -> Response {
    let rel_path = &path[prefix.len()..];
    let redirect_url = format!("{}{}", target, rel_path);

    let status = match status_code
        .map(StatusCode::from_u16)
        .unwrap_or(Ok(StatusCode::TEMPORARY_REDIRECT))
    {
        Ok(status) => status,
        Err(_) => StatusCode::TEMPORARY_REDIRECT,
    };

    tracing::debug!("Redirecting to: {} with status: {}", redirect_url, status);

    match Response::builder()
        .status(status)
        .header("Location", redirect_url)
        .body(Body::empty())
    {
        Ok(response) => response.into_response(),
        Err(err) => {
            tracing::error!("Failed to build redirect response: {}", err);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}

async fn handle_proxy(
    client: &HyperClient,
    target: &str,
    mut req: Request<Body>,
    prefix: &str,
) -> Response {
    let path = req.uri().path();
    let rel_path = &path[prefix.len()..];
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    // Construct the new URI
    let target_uri = format!("{}{}{}", target, rel_path, query);
    let uri: Uri = match target_uri.parse() {
        Ok(uri) => uri,
        Err(err) => {
            tracing::error!("Failed to parse target URI: {}, error: {}", target_uri, err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Invalid target URI").into_response();
        }
    };

    // Log the URI before moving it
    tracing::debug!("Proxying request to: {}", uri);

    // Update request URI
    *req.uri_mut() = uri;

    // NOTE: Some servers require a Host header. If experiencing routing issues with specific targets,
    // uncomment this section:
    // if !req.headers().contains_key("host") {
    //     if let Some(host) = uri.host() {
    //         let port_str = uri.port_u16().map(|p| format!(":{}", p)).unwrap_or_default();
    //         let host_val = format!("{}{}", host, port_str);
    //         req.headers_mut().insert("host", host_val.parse().unwrap());
    //     }
    // }

    // Forward the request
    match client.request(req).await {
        Ok(response) => response.into_response(),
        Err(err) => {
            tracing::error!("Proxy error: {:?}", err);
            (StatusCode::BAD_GATEWAY, "Bad Gateway").into_response()
        }
    }
}

async fn handle_load_balance(
    state: &Arc<AppState>,
    targets: &[String],
    strategy: &LoadBalanceStrategy,
    req: Request<Body>,
    prefix: &str,
) -> Response {
    if targets.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "No targets configured").into_response();
    }

    // Filter for healthy backends only if health checking is enabled
    let healthy_targets: Vec<&String> = if state.health_config.enabled {
        targets
            .iter()
            .filter(|target| {
                if let Some(backend) = state.backend_health.get(*target) {
                    backend.status() == HealthStatus::Healthy
                } else {
                    // If we don't have health info, assume it's healthy
                    true
                }
            })
            .collect()
    } else {
        targets.iter().collect()
    };

    // If no healthy targets available, return error
    if healthy_targets.is_empty() {
        tracing::warn!("No healthy backends available for route prefix: {}", prefix);
        return (StatusCode::SERVICE_UNAVAILABLE, "No healthy backends available").into_response();
    }

    // Select target based on strategy
    let target = match strategy {
        LoadBalanceStrategy::RoundRobin => {
            let count = state
                .counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let index = count % healthy_targets.len();
            healthy_targets[index]
        }
        LoadBalanceStrategy::Random => {
            let mut rng = rand::thread_rng();
            let index = rng.gen_range(0..healthy_targets.len());
            healthy_targets[index]
        }
    };

    tracing::debug!("Load balancing to healthy target: {}", target);

    // Forward to the selected target
    handle_proxy(&state.client, target, req, prefix).await
}

async fn health_checker(state: Arc<AppState>) {
    let health_config = &state.health_config;
    let client = &state.client; // Use the HTTPS-capable client from AppState
    let interval = Duration::from_secs(health_config.interval_secs);
    let timeout = Duration::from_secs(health_config.timeout_secs);
    
    tracing::info!("Starting health checker with interval: {}s, timeout: {}s, default path: {}", 
        health_config.interval_secs, health_config.timeout_secs, health_config.path);
    
    loop {
        // Sleep at the beginning to allow the server to start up
        tokio::time::sleep(interval).await;
        
        // Check each backend
        for backend_entry in state.backend_health.iter() {
            let target = backend_entry.key().clone();
            let backend_health = backend_entry.value();
            
            // Get backend-specific health check path or use default
            let backend_path = match state.backend_paths.get(&target) {
                Some(path) => {
                    tracing::debug!("Using custom health path for {}: {}", target, path);
                    path.clone()
                },
                None => health_config.path.clone()
            };
            
            // Construct health check URL
            let health_check_url = format!("{}{}", target, backend_path);
            
            tracing::debug!("Health checking: {}", health_check_url);
            
            // Create request with timeout
            let req = match Request::builder()
                .method("GET")
                .uri(&health_check_url)
                .body(Body::empty()) {
                    Ok(req) => req,
                    Err(err) => {
                        tracing::error!("Failed to build health check request for {}: {}", health_check_url, err);
                        continue;
                    }
                };
            
            // Perform the health check with timeout
            let result = tokio::time::timeout(timeout, client.request(req)).await;
            
            match result {
                Ok(Ok(response)) => {
                    let status = response.status();
                    
                    // Check if status code indicates healthy (2xx range)
                    if status.is_success() {
                        // Increment success counter
                        let successes = backend_health.consecutive_successes
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        
                        // If we've reached the threshold, mark as healthy
                        if successes >= health_config.healthy_threshold 
                            && backend_health.status() == HealthStatus::Unhealthy {
                                
                            tracing::info!("Backend {} is now HEALTHY (after {} consecutive successes)", 
                                           target, successes);
                            backend_health.mark_healthy();
                        }
                    } else {
                        // Non-2xx status is a failure
                        handle_health_check_failure(&target, backend_health, health_config, 
                                                   format!("Unhealthy status code: {}", status).as_str());
                    }
                },
                Ok(Err(err)) => {
                    handle_health_check_failure(&target, backend_health, health_config, 
                                               format!("Request failed: {}", err).as_str());
                },
                Err(_) => {
                    handle_health_check_failure(&target, backend_health, health_config, "Request timed out");
                }
            }
        }
    }
}

fn handle_health_check_failure(
    target: &str, 
    backend_health: &BackendHealth, 
    health_config: &HealthCheckConfig, 
    reason: &str
) {
    // Increment failure counter
    let failures = backend_health.consecutive_failures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    
    // Reset success counter
    backend_health.consecutive_successes
        .store(0, std::sync::atomic::Ordering::Relaxed);
    
    // If we've reached the threshold, mark as unhealthy
    if failures >= health_config.unhealthy_threshold 
        && backend_health.status() == HealthStatus::Healthy {
            
        tracing::warn!("Backend {} is now UNHEALTHY (after {} consecutive failures): {}", 
                      target, failures, reason);
        backend_health.mark_unhealthy();
    } else {
        tracing::debug!("Health check failed for {}: {} (failures: {}/{})", 
                      target, reason, failures, health_config.unhealthy_threshold);
    }
}
