use axum::{
    extract::Extension,
    http::{uri::Uri, Request, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use clap::Parser;
use hyper::{client::HttpConnector, Body, Client};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::{convert::Infallible, net::SocketAddr, sync::Arc};
use tokio::fs;
use tower::{ServiceBuilder, ServiceExt};
use tower_http::services::ServeDir;

type HyperClient = Client<HttpConnector>;

#[derive(Debug, Serialize, Deserialize)]
struct ServerConfig {
    listen_addr: String,
    routes: HashMap<String, RouteConfig>,
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

    // Setup HTTP client for proxying
    let client = Client::new();

    // Create shared state
    let state = Arc::new(AppState {
        routes: config.routes,
        client,
        counter: std::sync::atomic::AtomicUsize::new(0),
    });

    // Log configured routes
    for (prefix, route) in &state.routes {
        tracing::info!("Configured route: {} -> {:?}", prefix, route);
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

    match axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
    {
        Ok(_) => Ok(()),
        Err(err) => {
            tracing::error!("Server error: {}", err);
            Err(anyhow::anyhow!("Server error: {}", err))
        }
    }
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

    // Select target based on strategy
    let target = match strategy {
        LoadBalanceStrategy::RoundRobin => {
            let count = state
                .counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let index = count % targets.len();
            &targets[index]
        }
        LoadBalanceStrategy::Random => {
            let mut rng = rand::thread_rng();
            let index = rng.gen_range(0..targets.len());
            &targets[index]
        }
    };

    tracing::debug!("Load balancing to target: {}", target);

    // Forward to the selected target
    handle_proxy(&state.client, target, req, prefix).await
}
