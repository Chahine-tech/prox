use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use axum::response::{IntoResponse, Response};
use hyper::{Body, Request, StatusCode};
use rand::Rng;

use crate::config::{LoadBalanceStrategy, RouteConfig};
use crate::core::ProxyService;
use crate::ports::file_system::FileSystem;
use crate::ports::http_client::HttpClient;
use crate::ports::http_server::HttpHandler;

#[derive(Clone)]
pub struct HyperHandler {
    proxy_service: Arc<ProxyService>,
    http_client: Arc<dyn HttpClient>,
    file_system: Arc<dyn FileSystem>,
}

impl HyperHandler {
    pub fn new(
        proxy_service: Arc<ProxyService>,
        http_client: Arc<dyn HttpClient>,
        file_system: Arc<dyn FileSystem>,
    ) -> Self {
        Self {
            proxy_service,
            http_client,
            file_system,
        }
    }
    
    async fn handle_static(&self, root: &str, prefix: &str, req: Request<Body>) -> Response {
        let path = req.uri().path().to_string();
        let rel_path = &path[prefix.len()..];

        // Create a new request to avoid borrowing issues
        let (parts, body) = req.into_parts();
        let new_req = Request::from_parts(parts, body);

        match self.file_system.serve_file(root, rel_path, new_req).await {
            Ok(response) => response.into_response(),
            Err(err) => {
                tracing::error!("Static file error: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
            }
        }
    }
    
    async fn handle_redirect(
        &self,
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
    
    async fn handle_proxy(&self, target: &str, mut req: Request<Body>, prefix: &str) -> Response {
        let path = req.uri().path();
        let rel_path = &path[prefix.len()..];
        let query = req
            .uri()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();

        // Construct the new URI
        let target_uri = format!("{}{}{}", target, rel_path, query);
        let uri: hyper::Uri = match target_uri.parse() {
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

        // Forward the request through our HTTP client
        match self.http_client.send_request(req).await {
            Ok(response) => response.into_response(),
            Err(err) => {
                tracing::error!("Proxy error: {:?}", err);
                (StatusCode::BAD_GATEWAY, "Bad Gateway").into_response()
            }
        }
    }
    
    async fn handle_load_balance(
        &self,
        targets: &[String],
        strategy: &LoadBalanceStrategy,
        req: Request<Body>,
        prefix: &str,
    ) -> Response {
        if targets.is_empty() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "No targets configured").into_response();
        }

        // Filter for healthy backends
        let healthy_targets = self.proxy_service.get_healthy_backends(targets);
        
        // If no healthy targets available, return error
        if healthy_targets.is_empty() {
            tracing::warn!("No healthy backends available for route prefix: {}", prefix);
            return (StatusCode::SERVICE_UNAVAILABLE, "No healthy backends available").into_response();
        }

        // Select target based on strategy
        let target = match strategy {
            LoadBalanceStrategy::RoundRobin => {
                let count = self.proxy_service.counter
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let index = count % healthy_targets.len();
                &healthy_targets[index]
            }
            LoadBalanceStrategy::Random => {
                let mut rng = rand::thread_rng();
                let index = rng.gen_range(0..healthy_targets.len());
                &healthy_targets[index]
            }
        };

        tracing::debug!("Load balancing to healthy target: {}", target);

        // Forward to the selected target
        self.handle_proxy(target, req, prefix).await
    }
}

impl HttpHandler for HyperHandler {
    fn handle_request<'a>(&'a self, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>, anyhow::Error>> + Send + 'a>> {
        Box::pin(async move {
            let uri = req.uri().clone();
            let path = uri.path();

            // Find matching route using the proxy service
            let matched_route = self.proxy_service.find_matching_route(path);
            
            let response = match matched_route {
                Some((prefix, route_config)) => match route_config {
                    RouteConfig::Static { root } => {
                        self.handle_static(&root, &prefix, req).await
                    }
                    RouteConfig::Redirect { target, status_code } => {
                        self.handle_redirect(&target, path, &prefix, status_code).await
                    }
                    RouteConfig::Proxy { target } => {
                        self.handle_proxy(&target, req, &prefix).await
                    }
                    RouteConfig::LoadBalance { targets, strategy } => {
                        self.handle_load_balance(&targets, &strategy, req, &prefix).await
                    }
                },
                None => {
                    // No route matched
                    (StatusCode::NOT_FOUND, "Not Found").into_response()
                }
            };

            // Convert the response to the expected hyper::Response<Body> type
            let (parts, body) = response.into_parts();
            
            // Collect body into bytes and create a simpler stream
            let bytes = match hyper::body::to_bytes(body).await {
                Ok(bytes) => bytes,
                Err(err) => return Err(anyhow::anyhow!("Failed to read body: {}", err)),
            };
            
            // Create a simple one-element stream from the collected bytes
            let stream = futures_util::stream::once(async move { Ok::<_, hyper::Error>(bytes) });
            
            // Create a hyper Body
            let body = Body::wrap_stream(stream);
            
            Ok(Response::from_parts(parts, body))
        })
    }
}