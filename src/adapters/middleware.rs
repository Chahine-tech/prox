use std::sync::{Arc, RwLock};

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};

use crate::config::models::ServerConfig;

/// Middleware that adds Alt-Svc header when HTTP/3 is enabled
pub async fn add_alt_svc_header(
    req: Request,
    next: Next,
    config_holder: Arc<RwLock<Arc<ServerConfig>>>,
) -> Response {
    let mut response = next.run(req).await;

    // Check if HTTP/3 is enabled in the configuration
    let should_add_alt_svc = {
        match config_holder.read() {
            Ok(config) => config.protocols.http3_enabled && config.tls.is_some(),
            Err(e) => {
                tracing::warn!(
                    "Failed to acquire config read lock for Alt-Svc header: {}",
                    e
                );
                false
            }
        }
    };

    if should_add_alt_svc {
        // Add Alt-Svc header to advertise HTTP/3 support
        let header_value = HeaderValue::from_static("h3=\":443\"; ma=3600");
        response.headers_mut().insert("alt-svc", header_value);
    }

    response
}

/// Creates a closure for the Alt-Svc middleware
pub fn create_alt_svc_middleware(
    config_holder: Arc<RwLock<Arc<ServerConfig>>>,
) -> impl Fn(Request, Next) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>
+ Clone {
    move |req, next| {
        let config_holder = config_holder.clone();
        Box::pin(async move { add_alt_svc_header(req, next, config_holder).await })
    }
}
