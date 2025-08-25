use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

use anyhow::Result;
use axum::body::Body as AxumBody;
use axum::extract::ConnectInfo;
use axum::response::{IntoResponse, Response as AxumResponse};
use chrono::Utc;
use http_body_util::BodyExt;
use hyper::{
    Request, Response, StatusCode,
    header::{HeaderName, HeaderValue},
};
use regex::Regex;
use serde_json;
use std::net::SocketAddr;

fn substitute_placeholders_in_text(
    text: &str,
    ctx: &RequestConditionContext,
    client_ip_str: &str,
) -> String {
    let timestamp_iso = Utc::now().to_rfc3339();
    text.replace("{uri_path}", &ctx.uri_path)
        .replace("{timestamp_iso}", &timestamp_iso)
        .replace("{client_ip}", client_ip_str)
}

fn substitute_placeholders_in_json_value(
    json_value: &mut serde_json::Value,
    ctx: &RequestConditionContext,
    client_ip_str: &str,
) {
    match json_value {
        serde_json::Value::String(s) => {
            *s = substitute_placeholders_in_text(s, ctx, client_ip_str);
        }
        serde_json::Value::Array(arr) => {
            for val in arr {
                substitute_placeholders_in_json_value(val, ctx, client_ip_str);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, val) in map {
                substitute_placeholders_in_json_value(val, ctx, client_ip_str);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Debug)]
struct RequestConditionContext {
    uri_path: String,
    method: hyper::Method,
    headers: hyper::HeaderMap,
}

impl RequestConditionContext {
    fn from_request(req: &Request<AxumBody>) -> Self {
        Self {
            uri_path: req.uri().path().to_string(),
            method: req.method().clone(),
            headers: req.headers().clone(),
        }
    }
}

use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient;
use crate::config::{
    BodyActions, HeaderActions, LoadBalanceStrategy, RateLimitConfig, RequestCondition, RouteConfig,
};
use crate::core::{LoadBalancerFactory, ProxyService, RouteRateLimiter};
use crate::ports::file_system::FileSystem;
use crate::ports::http_client::{HttpClient, HttpClientError};
use crate::ports::http_server::{HandlerError, HttpHandler};

struct ProxyHandlerArgs<'a> {
    target: Option<&'a String>,
    targets: Option<&'a Vec<String>>,
    strategy: Option<&'a LoadBalanceStrategy>,
    req: Request<AxumBody>,
    prefix: &'a str,
    path_rewrite: Option<&'a str>,
    request_headers_actions: Option<&'a HeaderActions>,
    response_headers_actions: Option<&'a HeaderActions>,
    request_body_actions: Option<&'a BodyActions>,
    response_body_actions: Option<&'a BodyActions>,
    client_ip: Option<SocketAddr>,
    initial_req_ctx: &'a RequestConditionContext,
}

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
            rate_limiters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn compute_final_path(original_path: &str, prefix: &str, path_rewrite: Option<&str>) -> String {
        if let Some(rewrite_template) = path_rewrite {
            let stripped_path = if let Some(stripped) = original_path.strip_prefix(prefix) {
                stripped
            } else {
                tracing::warn!(
                    original_path = %original_path,
                    prefix = %prefix,
                    "Original path does not start with the expected prefix during path rewrite. This might indicate an internal logic issue."
                );
                return String::new();
            };
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
        let redirect_url = format!("{target}{rel_path}");
        let status = status_code
            .and_then(|code| StatusCode::from_u16(code).ok())
            .unwrap_or(StatusCode::TEMPORARY_REDIRECT);

        tracing::debug!("Redirecting to: {} with status: {}", redirect_url, status);

        Self::build_redirect_response(status, redirect_url)
    }

    fn check_condition(ctx: &RequestConditionContext, condition_config: &RequestCondition) -> bool {
        if let Some(path_regex_str) = &condition_config.path_matches {
            if let Ok(regex) = Regex::new(path_regex_str) {
                if !regex.is_match(&ctx.uri_path) {
                    tracing::debug!(
                        "Condition failed: path '{}' does not match regex '{}'",
                        ctx.uri_path,
                        path_regex_str
                    );
                    return false;
                }
            } else {
                tracing::warn!("Invalid regex for path_matches: {}", path_regex_str);
                return false; // Treat invalid regex as a failed condition
            }
        }

        // Check method condition
        match &condition_config.method_is {
            Some(method_str) if ctx.method.as_str() != method_str.to_uppercase() => {
                tracing::debug!(
                    "Condition failed: method '{}' does not match '{}'",
                    ctx.method,
                    method_str.to_uppercase()
                );
                return false;
            }
            _ => {}
        }

        if let Some(header_cond) = &condition_config.has_header {
            if let Some(header_value) = ctx.headers.get(&header_cond.name) {
                if let Some(value_regex_str) = &header_cond.value_matches {
                    if let Ok(header_value_str) = header_value.to_str() {
                        if let Ok(regex) = Regex::new(value_regex_str) {
                            if !regex.is_match(header_value_str) {
                                tracing::debug!(
                                    "Condition failed: header '{}' value '{}' does not match regex '{}'",
                                    header_cond.name,
                                    header_value_str,
                                    value_regex_str
                                );
                                return false;
                            }
                        } else {
                            tracing::warn!(
                                "Invalid regex for header value_matches: {}",
                                value_regex_str
                            );
                            return false;
                        }
                    } else {
                        tracing::debug!(
                            "Condition failed: header '{}' value is not valid UTF-8",
                            header_cond.name
                        );
                        return false; // Header value not valid UTF-8
                    }
                } // If no value_matches, presence of header is enough and we're good here.
            } else {
                tracing::debug!("Condition failed: header '{}' not found", header_cond.name);
                return false; // Header not found
            }
        }
        tracing::debug!("All conditions met.");
        true // All conditions met or no conditions specified
    }

    fn apply_header_actions(
        headers_to_modify: &mut hyper::HeaderMap,
        actions_config_opt: Option<&HeaderActions>,
        client_ip: Option<SocketAddr>,
        condition_check_ctx: Option<&RequestConditionContext>,
    ) {
        if let Some(actions_config) = actions_config_opt {
            if let Some(condition) = &actions_config.condition {
                if let Some(ctx) = condition_check_ctx {
                    if !Self::check_condition(ctx, condition) {
                        return; // Condition not met, skip actions
                    }
                } else {
                    tracing::warn!(
                        "Condition specified for header actions, but no context provided for check. Skipping actions."
                    );
                    return;
                }
            }

            for name in &actions_config.remove {
                if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                    headers_to_modify.remove(header_name);
                }
            }
            for (name, value_template) in &actions_config.add {
                if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                    let value_str = match value_template.as_str() {
                        "{client_ip}" => {
                            client_ip.map(|ip| ip.ip().to_string()).unwrap_or_default()
                        }
                        "{timestamp}" => Utc::now().to_rfc3339(),
                        _ => value_template.clone(),
                    };
                    if let Ok(header_value) = HeaderValue::from_str(&value_str) {
                        headers_to_modify.insert(header_name, header_value);
                    }
                }
            }
        }
    }

    async fn apply_body_actions_to_request(
        req: &mut Request<AxumBody>,
        actions_config_opt: Option<&BodyActions>,
        client_ip: Option<SocketAddr>,
    ) -> Result<(), HandlerError> {
        if let Some(actions_config) = actions_config_opt {
            let ctx = RequestConditionContext::from_request(req);

            // Check condition before applying actions
            if matches!(actions_config.condition.as_ref(), Some(condition) if !Self::check_condition(&ctx, condition))
            {
                return Ok(());
            }

            let client_ip_str = client_ip.map(|ip| ip.ip().to_string()).unwrap_or_default();

            if let Some(text_content_template) = &actions_config.set_text {
                let final_text_content =
                    substitute_placeholders_in_text(text_content_template, &ctx, &client_ip_str);

                *req.body_mut() = AxumBody::from(final_text_content.clone());
                req.headers_mut().remove(hyper::header::CONTENT_TYPE);
                req.headers_mut().remove(hyper::header::CONTENT_LENGTH);
                req.headers_mut().insert(
                    hyper::header::CONTENT_LENGTH,
                    HeaderValue::from(final_text_content.len()),
                );
            } else if let Some(json_content_template) = &actions_config.set_json {
                let mut final_json_content = json_content_template.clone();
                substitute_placeholders_in_json_value(
                    &mut final_json_content,
                    &ctx,
                    &client_ip_str,
                );

                match serde_json::to_string(&final_json_content) {
                    Ok(json_str) => {
                        *req.body_mut() = AxumBody::from(json_str.clone());
                        req.headers_mut().remove(hyper::header::CONTENT_TYPE); // Ensure old one is removed or type is correctly set
                        req.headers_mut().insert(
                            hyper::header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        );
                        req.headers_mut().remove(hyper::header::CONTENT_LENGTH);
                        req.headers_mut().insert(
                            hyper::header::CONTENT_LENGTH,
                            HeaderValue::from(json_str.len()),
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to serialize JSON for request body: {}", e);
                        return Err(HandlerError::InternalError(
                            "Failed to serialize JSON for request body".to_string(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    async fn apply_body_actions_to_response(
        response_to_modify: AxumResponse,
        actions_config_opt: Option<&BodyActions>,
        initial_req_ctx_opt: Option<&RequestConditionContext>,
        client_ip: Option<SocketAddr>,
    ) -> Result<AxumResponse, HandlerError> {
        let actions_config = match actions_config_opt {
            Some(config) => config,
            None => return Ok(response_to_modify),
        };

        if let Some(condition) = &actions_config.condition {
            match initial_req_ctx_opt {
                Some(ctx) => {
                    if !Self::check_condition(ctx, condition) {
                        return Ok(response_to_modify);
                    }
                }
                None => {
                    tracing::warn!(
                        "Condition specified for response body actions, but no context provided for check. Skipping actions."
                    );
                    return Ok(response_to_modify);
                }
            }
        }

        if actions_config.set_text.is_none() && actions_config.set_json.is_none() {
            return Ok(response_to_modify);
        }

        let initial_req_ctx = match initial_req_ctx_opt {
            Some(ctx) => ctx,
            None => {
                tracing::warn!(
                    "Response body actions (set_text/set_json) require context for placeholders, but none provided. Skipping actions."
                );
                return Ok(response_to_modify); // No context for placeholders
            }
        };

        let client_ip_str = client_ip.map(|ip| ip.ip().to_string()).unwrap_or_default();
        let (mut parts, original_body_stream) = response_to_modify.into_parts(); // Consumes response_to_modify
        let final_body_data: Vec<u8>;

        if let Some(text_content_template) = &actions_config.set_text {
            let substituted_text = substitute_placeholders_in_text(
                text_content_template,
                initial_req_ctx,
                &client_ip_str,
            );
            final_body_data = substituted_text.into_bytes();
            parts.headers.remove(hyper::header::CONTENT_TYPE); // Clear old content type
            parts.headers.remove(hyper::header::CONTENT_LENGTH); // Clear old length
            parts.headers.insert(
                hyper::header::CONTENT_LENGTH,
                HeaderValue::from(final_body_data.len()),
            );
        } else if let Some(json_content_template) = &actions_config.set_json {
            let mut substituted_json = json_content_template.clone();
            substitute_placeholders_in_json_value(
                &mut substituted_json,
                initial_req_ctx,
                &client_ip_str,
            );
            match serde_json::to_vec(&substituted_json) {
                Ok(json_vec) => {
                    final_body_data = json_vec;
                    parts.headers.remove(hyper::header::CONTENT_TYPE); // Clear old content type
                    parts.headers.insert(
                        hyper::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    parts.headers.remove(hyper::header::CONTENT_LENGTH); // Clear old length
                    parts.headers.insert(
                        hyper::header::CONTENT_LENGTH,
                        HeaderValue::from(final_body_data.len()),
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to serialize JSON for response body: {}", e);
                    return Err(HandlerError::InternalError(
                        "Failed to serialize JSON for response body".to_string(),
                    ));
                }
            }
        } else {
            // This case should ideally be caught by the (is_none && is_none) check earlier.
            // If somehow reached, it means no modification was intended by set_text/set_json.
            // We must reconstruct the response with the original body.
            let collected_body_bytes = match original_body_stream.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(e) => {
                    tracing::error!(
                        "Failed to read original response body when no modification applied: {}",
                        e
                    );
                    return Err(HandlerError::InternalError(format!(
                        "Failed to read response body: {e}"
                    )));
                }
            };
            final_body_data = collected_body_bytes.to_vec();
            // Content-Type and Content-Length from original `parts` should be preserved if no modification.
        }

        Ok(Response::from_parts(parts, AxumBody::from(final_body_data)).into_response())
    }

    async fn handle_proxy(&self, args: ProxyHandlerArgs<'_>) -> AxumResponse {
        let target = match args.target {
            Some(target) => target,
            None => {
                tracing::error!("Proxy route missing target configuration");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Proxy route missing target configuration",
                )
                    .into_response();
            }
        };
        let mut req = args.req; // Make req mutable from args
        let original_path = req.uri().path().to_string();
        let query = req.uri().query().map_or("", |q| q).to_string();

        // For request_headers, create a context from the current state of `req`
        let current_req_ctx_for_req_headers = RequestConditionContext::from_request(&req);
        Self::apply_header_actions(
            req.headers_mut(),
            args.request_headers_actions,
            args.client_ip,
            Some(&current_req_ctx_for_req_headers),
        );

        // apply_body_actions_to_request creates its own context from `req` before modification
        if let Err(e) =
            Self::apply_body_actions_to_request(&mut req, args.request_body_actions, args.client_ip)
                .await
        {
            // Convert HandlerError to AxumResponse
            return match e {
                HandlerError::InternalError(msg) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
                }
                // Add other HandlerError variants as needed
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An unexpected error occurred",
                )
                    .into_response(),
            };
        }

        let final_path = Self::compute_final_path(&original_path, args.prefix, args.path_rewrite);

        let target_uri_string = format!("{}{final_path}{query}", target.trim_end_matches('/'));

        match target_uri_string.parse::<hyper::Uri>() {
            Ok(uri) => {
                *req.uri_mut() = uri;
                match self.http_client.send_request(req).await {
                    Ok(response) => {
                        let mut axum_resp = response.map(AxumBody::new);
                        // For response_headers, use the initial_req_ctx
                        Self::apply_header_actions(
                            axum_resp.headers_mut(),
                            args.response_headers_actions,
                            args.client_ip,
                            Some(args.initial_req_ctx),
                        );
                        // For response_body, use the initial_req_ctx
                        match Self::apply_body_actions_to_response(
                            axum_resp,
                            args.response_body_actions,
                            Some(args.initial_req_ctx),
                            args.client_ip, // Pass client_ip
                        )
                        .await
                        {
                            Ok(resp_with_body_actions) => resp_with_body_actions,
                            Err(e) => match e {
                                HandlerError::InternalError(msg) => {
                                    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
                                }
                                _ => (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "An unexpected error occurred",
                                )
                                    .into_response(),
                            },
                        }
                    }
                    Err(e) => {
                        tracing::error!("Proxy request failed: {}", e);
                        // Map HttpClientError to an appropriate AxumResponse
                        let status_code = match e {
                            HttpClientError::ConnectionError(_) => StatusCode::BAD_GATEWAY,
                            HttpClientError::TimeoutError(_) => StatusCode::GATEWAY_TIMEOUT,
                            HttpClientError::InvalidRequestError(_) => StatusCode::BAD_REQUEST,
                            HttpClientError::BackendError { .. } => StatusCode::BAD_GATEWAY,
                        };
                        Self::build_response_with_fallback(
                            status_code,
                            format!("Proxy request failed: {e}"),
                            "proxy error response",
                        )
                    }
                }
            }
            Err(err) => {
                tracing::error!(
                    "Failed to parse target URI: {}, error: {}",
                    target_uri_string,
                    err
                );
                Self::build_response_with_fallback(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to parse target URI",
                    "URI parsing failure",
                )
            }
        }
    }

    async fn handle_load_balance(&self, args: ProxyHandlerArgs<'_>) -> AxumResponse {
        let targets = match args.targets {
            Some(targets) => targets,
            None => {
                tracing::error!("Load balance route missing targets configuration");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Load balance route missing targets configuration",
                )
                    .into_response();
            }
        };
        let strategy = match args.strategy {
            Some(strategy) => strategy,
            None => {
                tracing::error!("Load balance route missing strategy configuration");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Load balance route missing strategy configuration",
                )
                    .into_response();
            }
        };
        let mut req = args.req; // Make req mutable from args

        if targets.is_empty() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "No targets available").into_response();
        }

        let current_proxy_service = match self.proxy_service_holder.read() {
            Ok(service) => service.clone(),
            Err(e) => {
                tracing::error!("Failed to acquire proxy service read lock: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
                    .into_response();
            }
        };
        let healthy_targets = current_proxy_service.get_healthy_backends(targets);

        if healthy_targets.is_empty() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "No healthy targets available",
            )
                .into_response();
        }

        let lb_strategy = LoadBalancerFactory::create_strategy(strategy);
        let selected_target = match lb_strategy.select_target(&healthy_targets) {
            Some(t) => t,
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to select a target",
                )
                    .into_response();
            }
        };

        // For request_headers, create a context from the current state of `req`
        let current_req_ctx_for_req_headers = RequestConditionContext::from_request(&req);
        Self::apply_header_actions(
            req.headers_mut(),
            args.request_headers_actions,
            args.client_ip,
            Some(&current_req_ctx_for_req_headers),
        );

        // apply_body_actions_to_request creates its own context from `req` before modification
        if let Err(e) =
            Self::apply_body_actions_to_request(&mut req, args.request_body_actions, args.client_ip)
                .await
        {
            return match e {
                HandlerError::InternalError(msg) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
                }
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An unexpected error occurred",
                )
                    .into_response(),
            };
        }

        let original_path = req.uri().path().to_string();
        let query = req.uri().query().map_or("", |q| q).to_string(); // Store query as String

        let final_path = Self::compute_final_path(&original_path, args.prefix, args.path_rewrite);

        let target_uri_string = format!(
            "{}{}{}",
            selected_target.trim_end_matches('/'),
            final_path,
            query
        );

        match target_uri_string.parse::<hyper::Uri>() {
            Ok(uri) => {
                *req.uri_mut() = uri;
                match self.http_client.send_request(req).await {
                    Ok(response) => {
                        let mut axum_resp = response.map(AxumBody::new);
                        // For response_headers, use the initial_req_ctx
                        Self::apply_header_actions(
                            axum_resp.headers_mut(),
                            args.response_headers_actions,
                            args.client_ip,
                            Some(args.initial_req_ctx),
                        );
                        // For response_body, use the initial_req_ctx
                        match Self::apply_body_actions_to_response(
                            axum_resp,
                            args.response_body_actions,
                            Some(args.initial_req_ctx),
                            args.client_ip, // Pass client_ip
                        )
                        .await
                        {
                            Ok(resp_with_body_actions) => resp_with_body_actions,
                            Err(e) => match e {
                                HandlerError::InternalError(msg) => {
                                    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
                                }
                                _ => (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "An unexpected error occurred",
                                )
                                    .into_response(),
                            },
                        }
                    }
                    Err(e) => {
                        tracing::error!("Load balanced request failed: {}", e);
                        // Map HttpClientError to an appropriate AxumResponse
                        let status_code = match e {
                            HttpClientError::ConnectionError(_) => StatusCode::BAD_GATEWAY,
                            HttpClientError::TimeoutError(_) => StatusCode::GATEWAY_TIMEOUT,
                            HttpClientError::InvalidRequestError(_) => StatusCode::BAD_REQUEST,
                            HttpClientError::BackendError { .. } => StatusCode::BAD_GATEWAY,
                        };
                        Self::build_response_with_fallback(
                            status_code,
                            format!("Load balanced request failed: {e}"),
                            "load balancer error response",
                        )
                    }
                }
            }
            Err(err) => {
                tracing::error!(
                    "Failed to parse load balanced target URI: {}, error: {}",
                    target_uri_string,
                    err
                );
                Self::build_response_with_fallback(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to parse load balanced target URI",
                    "load balancer URI parsing failure",
                )
            }
        }
    }

    async fn handle_websocket_proxy(
        &self,
        _target: &str,
        _prefix: &str,
        _path_rewrite: Option<&str>,
        req: Request<AxumBody>,
        _client_ip: Option<SocketAddr>,
    ) -> AxumResponse {
        // Check if this is a WebSocket upgrade request
        let is_websocket_upgrade = req
            .headers()
            .get("upgrade")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_lowercase() == "websocket")
            .unwrap_or(false);

        if !is_websocket_upgrade {
            tracing::warn!(
                "Non-WebSocket request to WebSocket route: {}",
                req.uri().path()
            );
            return (
                StatusCode::BAD_REQUEST,
                "This route only supports WebSocket connections",
            )
                .into_response();
        }

        // For WebSocket, we need to establish a connection to the backend
        // This is a complex operation that requires WebSocket client support
        tracing::warn!("WebSocket proxying is not yet fully implemented");
        (
            StatusCode::NOT_IMPLEMENTED,
            "WebSocket proxying is not yet implemented",
        )
            .into_response()
    }

    async fn get_or_create_rate_limiter(
        &self,
        route_path: &str,
        config: &RateLimitConfig,
    ) -> Result<Arc<RouteRateLimiter>, AxumResponse> {
        // Create a cache key that includes the config details to ensure cache invalidation
        // when configuration changes
        let cache_key = format!(
            "{}:{:?}:{}:{}:{}:{}",
            route_path,
            config.by,
            config.requests,
            config.period,
            config.status_code,
            config.message
        );

        tracing::debug!("Rate limiter cache key: {}", cache_key);

        let mut limiters = self.rate_limiters.lock().await;
        if let Some(limiter) = limiters.get(&cache_key) {
            tracing::debug!("Rate limiter cache HIT for key: {}", cache_key);
            return Ok(limiter.clone());
        }

        tracing::debug!("Rate limiter cache MISS for key: {}", cache_key);

        match RouteRateLimiter::new(config) {
            Ok(limiter) => {
                let arc_limiter = Arc::new(limiter);
                limiters.insert(cache_key, arc_limiter.clone());
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
                    format!("Failed to configure rate limiter: {e}"),
                )
                    .into_response())
            }
        }
    }

    // Helper function to build responses with consistent error handling
    fn build_response_with_fallback<T>(
        status: StatusCode,
        body: T,
        error_context: &str,
    ) -> AxumResponse
    where
        T: Into<AxumBody>,
    {
        Response::builder()
            .status(status)
            .body(body.into())
            .unwrap_or_else(|_| {
                tracing::error!("Failed to build response: {}", error_context);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
            })
    }

    // Helper function for redirect responses
    fn build_redirect_response(status: StatusCode, location: String) -> AxumResponse {
        Response::builder()
            .status(status)
            .header("Location", location)
            .body(AxumBody::empty())
            .map(IntoResponse::into_response)
            .unwrap_or_else(|err| {
                tracing::error!("Failed to build redirect response: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
            })
    }
}

impl HttpHandler for HyperHandler {
    async fn handle_request(
        &self,
        mut req: Request<AxumBody>, // Made req mutable here
    ) -> Result<Response<AxumBody>, HandlerError> {
        let client_ip_info = req.extensions().get::<ConnectInfo<SocketAddr>>().cloned();
        let client_ip = client_ip_info.as_ref().map(|ci| ci.0);
        // let uri = req.uri().clone(); // Not strictly needed here if using initial_req_ctx
        // let path = uri.path(); // Not strictly needed here if using initial_req_ctx

        // Create the context from the *initial* request. This is cheap.
        let initial_req_ctx = RequestConditionContext::from_request(&req);

        let current_proxy_service = match self.proxy_service_holder.read() {
            Ok(service) => service.clone(),
            Err(e) => {
                tracing::error!(
                    "Failed to acquire proxy service read lock in handle_request: {}",
                    e
                );
                return Err(HandlerError::InternalError("Service unavailable".into()));
            }
        };
        // Use initial_req_ctx.uri_path for finding the route
        let matched_route_opt =
            current_proxy_service.find_matching_route(&initial_req_ctx.uri_path);

        let axum_response: AxumResponse = match matched_route_opt {
            Some((prefix_str, route_config)) => {
                // Rate Limiting (if configured) - This part remains largely the same
                let maybe_rate_limit_config = match &route_config {
                    RouteConfig::Static { rate_limit, .. } => rate_limit.as_ref(),
                    RouteConfig::Redirect { rate_limit, .. } => rate_limit.as_ref(),
                    RouteConfig::Proxy { rate_limit, .. } => rate_limit.as_ref(),
                    RouteConfig::LoadBalance { rate_limit, .. } => rate_limit.as_ref(),
                    RouteConfig::Websocket { rate_limit, .. } => rate_limit.as_ref(),
                };

                if let Some(rate_limit_config) = maybe_rate_limit_config {
                    match self
                        .get_or_create_rate_limiter(&prefix_str, rate_limit_config)
                        .await
                    {
                        Ok(limiter) => {
                            // The `check` method on RouteRateLimiter expects the request and connect_info
                            // We pass a reference to the original request's parts for header checking etc.
                            // and the cloned ConnectInfo.
                            // We need to temporarily take ownership of `req` to pass to `limiter.check`
                            // then put it back if not rate limited.
                            let (parts, body) = req.into_parts();
                            // temp_req_for_check needs headers, method, uri from `parts`
                            // and client_ip_info for the check method.
                            // The `check` method in RouteRateLimiter might need to be adapted or
                            // we ensure it can work with parts + connect_info.
                            // For now, assuming it works with a request reconstructed from parts.
                            let mut temp_req_builder = Request::builder()
                                .method(parts.method.clone())
                                .uri(parts.uri.clone())
                                .version(parts.version);
                            for (name, value) in &parts.headers {
                                temp_req_builder = temp_req_builder.header(name, value);
                            }
                            // Pass an empty body for the check, actual body is preserved.
                            let temp_req_for_check = match temp_req_builder.body(AxumBody::empty())
                            {
                                Ok(req) => req,
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to build temporary request for rate limiting: {}",
                                        e
                                    );
                                    return Ok((
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        "Internal server error",
                                    )
                                        .into_response());
                                }
                            };

                            match limiter.check(&temp_req_for_check, client_ip_info.as_ref()) {
                                Ok(_) => {
                                    // If check passes, reconstruct the original request to proceed
                                    req = Request::from_parts(parts, body);
                                }
                                Err(limit_response_boxed) => {
                                    return Ok(*limit_response_boxed); // Return the rate limit response
                                }
                            }
                        }
                        Err(e) => return Ok(e), // Already an AxumResponse from get_or_create_rate_limiter
                    }
                }

                match route_config {
                    RouteConfig::Static { root, .. } => {
                        self.handle_static(&root, &prefix_str, req).await
                    }
                    RouteConfig::Redirect {
                        target,
                        status_code,
                        ..
                    } => {
                        // handle_redirect uses path from the original URI.
                        // initial_req_ctx.uri_path can be used here.
                        self.handle_redirect(
                            &target,
                            &initial_req_ctx.uri_path,
                            &prefix_str,
                            status_code,
                        )
                        .await
                    }
                    RouteConfig::Proxy {
                        ref target,
                        path_rewrite,
                        request_headers,
                        response_headers,
                        request_body,
                        response_body,
                        ..
                    } => {
                        let args = ProxyHandlerArgs {
                            target: Some(target),
                            targets: None,
                            strategy: None,
                            req, // Original req is moved here
                            prefix: &prefix_str,
                            path_rewrite: path_rewrite.as_deref(),
                            request_headers_actions: request_headers.as_ref(),
                            response_headers_actions: response_headers.as_ref(),
                            request_body_actions: request_body.as_ref(),
                            response_body_actions: response_body.as_ref(),
                            client_ip,
                            initial_req_ctx: &initial_req_ctx,
                        };
                        self.handle_proxy(args).await
                    }
                    RouteConfig::LoadBalance {
                        ref targets,
                        ref strategy,
                        path_rewrite,
                        request_headers,
                        response_headers,
                        request_body,
                        response_body,
                        ..
                    } => {
                        let args = ProxyHandlerArgs {
                            target: None,
                            targets: Some(targets),
                            strategy: Some(strategy),
                            req, // Original req is moved here
                            prefix: &prefix_str,
                            path_rewrite: path_rewrite.as_deref(),
                            request_headers_actions: request_headers.as_ref(),
                            response_headers_actions: response_headers.as_ref(),
                            request_body_actions: request_body.as_ref(),
                            response_body_actions: response_body.as_ref(),
                            client_ip,
                            initial_req_ctx: &initial_req_ctx,
                        };
                        self.handle_load_balance(args).await
                    }
                    RouteConfig::Websocket {
                        ref target,
                        path_rewrite,
                        ..
                    } => {
                        self.handle_websocket_proxy(
                            target,
                            &prefix_str,
                            path_rewrite.as_deref(),
                            req,
                            client_ip,
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
