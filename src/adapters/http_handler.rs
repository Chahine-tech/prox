use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::body::Body as AxumBody;
use hyper::{Request, Response, StatusCode};
use http_body_util::BodyExt;

use crate::config::{LoadBalanceStrategy, RouteConfig};
use crate::core::{ProxyService, LoadBalancerFactory};
use crate::ports::file_system::FileSystem;
use crate::ports::http_client::{HttpClient, HttpClientError};
use crate::ports::http_server::{HttpHandler, HandlerError};

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

    async fn handle_static(&self, root: &str, prefix: &str, req: Request<AxumBody>) -> AxumResponse {
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

    async fn handle_proxy(&self, target: &str, mut req: Request<AxumBody>, prefix: &str) -> AxumResponse {
        let path = req.uri().path();
        let rel_path = &path[prefix.len()..];
        let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
        let target_uri = format!("{}{}{}", target, rel_path, query);

        match target_uri.parse::<hyper::Uri>() {
            Ok(uri) => {
                tracing::debug!("Proxying request to: {}", uri);
                *req.uri_mut() = uri;
                match self.http_client.send_request(req).await {
                    Ok(response) => response.into_response(),
                    Err(err) => match err {
                        HttpClientError::ConnectionError(msg) => {
                            tracing::error!("Connection error: {}", msg);
                            (StatusCode::BAD_GATEWAY, format!("Cannot connect to backend: {}", msg)).into_response()
                        },
                        HttpClientError::TimeoutError(secs) => {
                            tracing::error!("Request timed out after {} seconds", secs);
                            (StatusCode::GATEWAY_TIMEOUT, format!("Backend timeout after {} seconds", secs)).into_response()
                        },
                        HttpClientError::BackendError { url, status } => {
                            tracing::error!("Backend {} returned error status: {}", url, status);
                            (StatusCode::BAD_GATEWAY, format!("Backend error: {}", status)).into_response()
                        },
                        _ => {
                            tracing::error!("Proxy error: {:?}", err);
                            (StatusCode::BAD_GATEWAY, "Bad Gateway").into_response()
                        }
                    }
                }
            }
            Err(err) => {
                tracing::error!("Failed to parse target URI: {}, error: {}", target_uri, err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Invalid target URI").into_response()
            }
        }
    }

    async fn handle_load_balance(
        &self,
        targets: &[String],
        strategy: &LoadBalanceStrategy,
        req: Request<AxumBody>,
        prefix: &str,
    ) -> AxumResponse {
        if targets.is_empty() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "No targets configured").into_response();
        }
        let healthy_targets = self.proxy_service.get_healthy_backends(targets);
        if healthy_targets.is_empty() {
            tracing::warn!("No healthy backends available for route prefix: {}", prefix);
            return (StatusCode::SERVICE_UNAVAILABLE, "No healthy backends available").into_response();
        }
        let lb_strategy = LoadBalancerFactory::create_strategy(strategy);
        let target = match lb_strategy.select_target(&healthy_targets) {
            Some(target) => target,
            None => {
                tracing::error!("Load balancer failed to select a target");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Load balancer error").into_response();
            }
        };
        tracing::debug!("Load balancing to healthy target: {}", target);
        self.handle_proxy(&target, req, prefix).await
    }
}

impl HttpHandler for HyperHandler {
    fn handle_request<'a>(&'a self, req: Request<AxumBody>) -> Pin<Box<dyn Future<Output = Result<Response<AxumBody>, HandlerError>> + Send + 'a>> {
        Box::pin(async move {
            let uri = req.uri().clone();
            let path = uri.path();
            let matched_route = self.proxy_service.find_matching_route(path);

            let axum_response: AxumResponse = match matched_route {
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
                    (StatusCode::NOT_FOUND, "Not Found").into_response()
                }
            };

            let (parts, axum_body) = axum_response.into_parts();

            let bytes = match axum_body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(err) => {
                    tracing::error!("Failed to collect response body: {}", err);
                    let error_response = Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(AxumBody::from("Internal Server Error"))
                        .unwrap();
                    return Ok(error_response);
                }
            };

            let body = AxumBody::from(bytes);

            Ok(Response::from_parts(parts, body))
        })
    }
}