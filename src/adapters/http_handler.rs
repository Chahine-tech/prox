use std::sync::Arc;

use anyhow::Result;
use axum::body::Body as AxumBody;
use axum::response::{IntoResponse, Response as AxumResponse};
use http_body_util::BodyExt;
use hyper::{Request, Response, StatusCode};

use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient; // Import concrete type
use crate::config::{LoadBalanceStrategy, RouteConfig};
use crate::core::{LoadBalancerFactory, ProxyService};
use crate::ports::file_system::FileSystem; // Import the FileSystem trait
// Ensure HttpClient trait is imported to use its methods like send_request
use crate::ports::http_client::{HttpClient, HttpClientError};
use crate::ports::http_server::{HandlerError, HttpHandler}; // Import concrete type

#[derive(Clone)]
pub struct HyperHandler {
    proxy_service: Arc<ProxyService>,
    http_client: Arc<HyperHttpClient>, // Use concrete type
    file_system: Arc<TowerFileSystem>, // Use concrete type
}

impl HyperHandler {
    pub fn new(
        proxy_service: Arc<ProxyService>,
        http_client: Arc<HyperHttpClient>, // Use concrete type
        file_system: Arc<TowerFileSystem>, // Use concrete type
    ) -> Self {
        Self {
            proxy_service,
            http_client,
            file_system,
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
        let original_path = req.uri().path();
        let query = req.uri().query().map_or("".to_string(), |q| format!("?{}", q));

        let final_path = if let Some(rewrite_template) = path_rewrite {
            let stripped_path = original_path.strip_prefix(prefix).unwrap_or(original_path);
            if rewrite_template == "/" {
                stripped_path.to_string()
            } else {
                format!("{}{}", rewrite_template.trim_end_matches('/'), stripped_path)
            }
        } else {
            // If no path_rewrite, the path relative to the prefix is appended to the target.
            original_path.strip_prefix(prefix).unwrap_or(original_path).to_string()
        };

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
                tracing::error!("Failed to parse target URI: {}, error: {}", target_uri_string, err);
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

        let healthy_targets = self.proxy_service.get_healthy_backends(targets);
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

        let original_path = req.uri().path();
        let query = req.uri().query().map_or("".to_string(), |q| format!("?{}", q));

        let final_path = if let Some(rewrite_template) = path_rewrite {
            let stripped_path = original_path.strip_prefix(prefix).unwrap_or(original_path);
            if rewrite_template == "/" {
                stripped_path.to_string()
            } else {
                format!("{}{}", rewrite_template.trim_end_matches('/'), stripped_path)
            }
        } else {
            original_path.strip_prefix(prefix).unwrap_or(original_path).to_string()
        };

        let target_uri_string = format!("{}{}{}", selected_target.trim_end_matches('/'), final_path, query);

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
                            .body(AxumBody::from(format!("Load balanced request failed: {}", e)))
                            .unwrap()
                    }
                }
            }
            Err(err) => {
                tracing::error!("Failed to parse load balanced target URI: {}, error: {}", target_uri_string, err);
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(AxumBody::from("Failed to parse load balanced target URI"))
                    .unwrap()
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
        let matched_route = self.proxy_service.find_matching_route(path);

        let axum_response: AxumResponse = match matched_route {
            Some((prefix, route_config)) => match route_config {
                RouteConfig::Static { root } => self.handle_static(&root, &prefix, req).await,
                RouteConfig::Redirect {
                    target,
                    status_code,
                } => {
                    self.handle_redirect(&target, path, &prefix, status_code)
                        .await
                }
                RouteConfig::Proxy { target, path_rewrite } => {
                    self.handle_proxy(&target, req, &prefix, path_rewrite.as_deref()).await
                }
                RouteConfig::LoadBalance { targets, strategy, path_rewrite } => {
                    self.handle_load_balance(&targets, &strategy, req, &prefix, path_rewrite.as_deref())
                        .await
                }
            },
            None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        };

        let (parts, axum_body) = axum_response.into_parts();

        let bytes = match axum_body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                tracing::error!("Failed to collect response body: {}", err);
                return Err(HandlerError::RequestError(format!(
                    "Failed to collect response body: {}",
                    err
                )));
            }
        };

        let body = AxumBody::from(bytes);

        Ok(Response::from_parts(parts, body))
    }
}
