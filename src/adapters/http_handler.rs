use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

use anyhow::Result;
use axum::body::Body as AxumBody;
use axum::extract::ConnectInfo;
use axum::response::{IntoResponse, Response as AxumResponse};
use hyper::{Request, Response, StatusCode};
use std::net::SocketAddr;

use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient;
use crate::config::{LoadBalanceStrategy, RateLimitConfig, RouteConfig};
use crate::core::{LoadBalancerFactory, ProxyService, RouteRateLimiter};
use crate::ports::file_system::FileSystem;
use crate::ports::http_client::{HttpClient, HttpClientError};
use crate::ports::http_server::{HandlerError, HttpHandler};

#[derive(Clone)]
pub struct HyperHandler {
    proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
    http_client: Arc<HyperHttpClient>,
    file_system: Arc<TowerFileSystem>,
    rate_limiters: Arc<Mutex<HashMap<String, Arc<RouteRateLimiter>>>>,
}

impl HyperHandler {
    pub fn new(
        proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
        http_client: Arc<HyperHttpClient>,
        file_system: Arc<TowerFileSystem>,
    ) -> Self {
        Self {
            proxy_service_holder,
            http_client,
            file_system,
            rate_limiters: Arc::new(Mutex::new(HashMap::new())), // Initialize here
        }
    }

    // Helper function to compute the final path after considering rewrite rules
    fn compute_final_path(original_path: &str, prefix: &str, path_rewrite: Option<&str>) -> String {
        if let Some(rewrite_template) = path_rewrite {
            let stripped_path = if let Some(stripped) = original_path.strip_prefix(prefix) {
                stripped
            } else {
                // Log a warning or handle the case where the prefix is not found
                tracing::warn!(
                    original_path = %original_path, // Using % for Display trait
                    prefix = %prefix,
                    "Original path does not start with the expected prefix during path rewrite. This might indicate an internal logic issue."
                );
                // Fallback to an empty path or handle as per application logic
                return String::new(); // Or handle appropriately
            };
            // If rewrite_template is "/", use the stripped_path as-is, effectively removing the original prefix
            // and not adding any new prefix from the rewrite_template itself.
            // For example, if original_path is "/api/v1/users", prefix is "/api/v1", and rewrite_template is "/",
            // then stripped_path is "/users", and final_path becomes "/users".
            if rewrite_template == "/" {
                stripped_path.to_string()
            } else {
                format!(
                    "{}{}",
                    rewrite_template.trim_end_matches('/'),
                    stripped_path
                )
            }
        } else {
            // If no path_rewrite, the path relative to the prefix is used.
            original_path
                .strip_prefix(prefix)
                .unwrap_or(original_path)
                .to_string()
        }
    }

    async fn handle_static(
        &self,
        root: &str,
        prefix: &str,
        req: Request<AxumBody>,
    ) -> AxumResponse {
        let path = req.uri().path().to_string();
        let rel_path = &path[prefix.len()..];
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
    ) -> AxumResponse {
        let rel_path = &path[prefix.len()..];
        let redirect_url = format!("{}{}", target, rel_path);
        let status = status_code
            .and_then(|code| StatusCode::from_u16(code).ok())
            .unwrap_or(StatusCode::TEMPORARY_REDIRECT);

        tracing::debug!("Redirecting to: {} with status: {}", redirect_url, status);

        Response::builder()
            .status(status)
            .header("Location", redirect_url)
            .body(AxumBody::empty())
            .map(IntoResponse::into_response)
            .unwrap_or_else(|err| {
                tracing::error!("Failed to build redirect response: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
            })
    }

    async fn handle_proxy(
        &self,
        target: &str,
        mut req: Request<AxumBody>,
        prefix: &str,
        path_rewrite: Option<&str>,
    ) -> AxumResponse {
        let original_path = req.uri().path().to_string(); // Keep as String for lifetime reasons if needed by helper
        let query = req
            .uri()
            .query()
            .map_or("".to_string(), |q| format!("?{}", q));

        let final_path = Self::compute_final_path(&original_path, prefix, path_rewrite);

        let target_uri_string = format!("{}{}{}", target.trim_end_matches('/'), final_path, query);

        match target_uri_string.parse::<hyper::Uri>() {
            Ok(uri) => {
                *req.uri_mut() = uri;
                // Use send_request as defined in the HttpClient trait
                match self.http_client.send_request(req).await {
                    Ok(response) => response.map(AxumBody::new),
                    Err(e) => {
                        tracing::error!("Proxy request failed: {}", e);
                        // Map HttpClientError to an appropriate AxumResponse
                        let status_code = match e {
                            HttpClientError::ConnectionError(_) => StatusCode::BAD_GATEWAY,
                            HttpClientError::TimeoutError(_) => StatusCode::GATEWAY_TIMEOUT,
                            HttpClientError::InvalidRequestError(_) => StatusCode::BAD_REQUEST,
                            HttpClientError::BackendError { .. } => StatusCode::BAD_GATEWAY,
                        };
                        Response::builder()
                            .status(status_code)
                            .body(AxumBody::from(format!("Proxy request failed: {}", e)))
                            .unwrap()
                    }
                }
            }
            Err(err) => {
                tracing::error!(
                    "Failed to parse target URI: {}, error: {}",
                    target_uri_string,
                    err
                );
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(AxumBody::from("Failed to parse target URI"))
                    .unwrap()
            }
        }
    }

    async fn handle_load_balance(
        &self,
        targets: &[String],
        strategy: &LoadBalanceStrategy,
        mut req: Request<AxumBody>,
        prefix: &str,
        path_rewrite: Option<&str>,
    ) -> AxumResponse {
        if targets.is_empty() {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(AxumBody::from("No targets configured for load balancing"))
                .unwrap();
        }

        // Get current ProxyService snapshot to access health data
        let current_proxy_service = self.proxy_service_holder.read().unwrap().clone();
        let healthy_targets = current_proxy_service.get_healthy_backends(targets);

        if healthy_targets.is_empty() {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(AxumBody::from("No healthy backends available"))
                .unwrap();
        }

        let lb_strategy = LoadBalancerFactory::create_strategy(strategy);
        let selected_target = match lb_strategy.select_target(&healthy_targets) {
            Some(t) => t,
            None => {
                // This case should ideally not be reached if healthy_targets is not empty
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(AxumBody::from("Load balancer failed to select a target"))
                    .unwrap();
            }
        };

        let original_path = req.uri().path().to_string(); // Keep as String for lifetime reasons if needed by helper
        let query = req
            .uri()
            .query()
            .map_or("".to_string(), |q| format!("?{}", q));

        let final_path = Self::compute_final_path(&original_path, prefix, path_rewrite);

        let target_uri_string = format!(
            "{}{}{}",
            selected_target.trim_end_matches('/'),
            final_path,
            query
        );

        match target_uri_string.parse::<hyper::Uri>() {
            Ok(uri) => {
                *req.uri_mut() = uri;
                // Use send_request as defined in the HttpClient trait
                match self.http_client.send_request(req).await {
                    Ok(response) => response.map(AxumBody::new),
                    Err(e) => {
                        tracing::error!("Load balanced request failed: {}", e);
                        // Map HttpClientError to an appropriate AxumResponse
                        let status_code = match e {
                            HttpClientError::ConnectionError(_) => StatusCode::BAD_GATEWAY,
                            HttpClientError::TimeoutError(_) => StatusCode::GATEWAY_TIMEOUT,
                            HttpClientError::InvalidRequestError(_) => StatusCode::BAD_REQUEST,
                            HttpClientError::BackendError { .. } => StatusCode::BAD_GATEWAY,
                        };
                        Response::builder()
                            .status(status_code)
                            .body(AxumBody::from(format!(
                                "Load balanced request failed: {}",
                                e
                            )))
                            .unwrap()
                    }
                }
            }
            Err(err) => {
                tracing::error!(
                    "Failed to parse load balanced target URI: {}, error: {}",
                    target_uri_string,
                    err
                );
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(AxumBody::from("Failed to parse load balanced target URI"))
                    .unwrap()
            }
        }
    }

    async fn get_or_create_rate_limiter(
        &self,
        route_path: &str,
        config: &RateLimitConfig,
    ) -> Result<Arc<RouteRateLimiter>, AxumResponse> {
        let mut limiters = self.rate_limiters.lock().await;
        if let Some(limiter) = limiters.get(route_path) {
            return Ok(limiter.clone());
        }

        match RouteRateLimiter::new(config) {
            Ok(limiter) => {
                let arc_limiter = Arc::new(limiter);
                limiters.insert(route_path.to_string(), arc_limiter.clone());
                Ok(arc_limiter)
            }
            Err(e) => {
                tracing::error!(
                    "Failed to create rate limiter for path '{}': {}",
                    route_path,
                    e
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to configure rate limiter: {}", e),
                )
                    .into_response())
            }
        }
    }
}

impl HttpHandler for HyperHandler {
    async fn handle_request(
        &self,
        req: Request<AxumBody>,
    ) -> Result<Response<AxumBody>, HandlerError> {
        let uri = req.uri().clone();
        let path = uri.path();

        // Get current ProxyService snapshot for routing
        let current_proxy_service = self.proxy_service_holder.read().unwrap().clone();
        let matched_route_opt = current_proxy_service.find_matching_route(path);

        let axum_response: AxumResponse = match matched_route_opt {
            Some((prefix, route_config)) => {
                // Rate Limiting Check
                let rate_limit_config_opt = match &route_config {
                    RouteConfig::Static { rate_limit, .. } => rate_limit.as_ref(),
                    RouteConfig::Redirect { rate_limit, .. } => rate_limit.as_ref(),
                    RouteConfig::Proxy { rate_limit, .. } => rate_limit.as_ref(),
                    RouteConfig::LoadBalance { rate_limit, .. } => rate_limit.as_ref(),
                };

                if let Some(rl_config) = rate_limit_config_opt {
                    // Use the route prefix as the key for the limiter
                    match self.get_or_create_rate_limiter(&prefix, rl_config).await {
                        Ok(limiter) => {
                            // Extract ConnectInfo for IP-based rate limiting
                            // This assumes ConnectInfo<SocketAddr> is available in request extensions,
                            // which requires the Axum server to be started with `into_make_service_with_connect_info`.
                            let client_addr_info =
                                req.extensions().get::<ConnectInfo<SocketAddr>>();

                            if let Err(response) = limiter.check(&req, client_addr_info) {
                                return Ok(response); // Return rate limited response
                            }
                        }
                        Err(response) => return Ok(response), // Error creating limiter
                    }
                }

                // Proceed with handling after rate limit check
                match route_config {
                    RouteConfig::Static { root, .. } => {
                        self.handle_static(&root, &prefix, req).await
                    }
                    RouteConfig::Redirect {
                        target,
                        status_code,
                        ..
                    } => {
                        self.handle_redirect(&target, path, &prefix, status_code)
                            .await
                    }
                    RouteConfig::Proxy {
                        target,
                        path_rewrite,
                        ..
                    } => {
                        tracing::debug!(target = %target, path = %path, prefix = %prefix, path_rewrite = ?path_rewrite, "Entering handle_proxy in http_handler.rs");
                        let response = self
                            .handle_proxy(&target, req, &prefix, path_rewrite.as_deref())
                            .await;
                        tracing::debug!(status = ?response.status(), headers = ?response.headers(), "Exiting handle_proxy in http_handler.rs, response prepared.");
                        response
                    }
                    RouteConfig::LoadBalance {
                        targets,
                        strategy,
                        path_rewrite,
                        ..
                    } => {
                        self.handle_load_balance(
                            &targets,
                            &strategy,
                            req,
                            &prefix,
                            path_rewrite.as_deref(),
                        )
                        .await
                    }
                }
            }
            None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        };

        // Directly return the AxumResponse without collecting the body.
        // The AxumBody within axum_response should already be the streaming body from http_client.
        tracing::debug!(response_status = ?axum_response.status(), response_headers = ?axum_response.headers(), "HyperHandler::handle_request: Final AxumResponse before returning to server.");
        Ok(axum_response)
    }
}
