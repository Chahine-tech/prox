use axum::body::Body as AxumBody;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, Version, header, header::HeaderValue};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use std::time::Duration;
use thiserror::Error;
use tokio::time::timeout;

use hyper_rustls::HttpsConnector;
use rustls_native_certs::load_native_certs;

use crate::ports::http_client::{HttpClient, HttpClientError, HttpClientResult};
use crate::metrics::{BackendRequestTimer, increment_backend_request_total}; // Added

/// Custom error type for HTTP client operations
#[derive(Error, Debug)]
pub enum HyperClientError {
    #[error("HTTP request error: {0}")]
    RequestError(String),

    #[error("Request timeout after {0} seconds")]
    Timeout(u64),

    #[error("Invalid request: {0}")]
    InvalidRequest(#[from] hyper::http::Error),

    #[error("Request to {url} failed with status: {status}")]
    FailedRequest {
        url: String,
        status: hyper::StatusCode,
    },

    #[error("TLS configuration error: {0}")]
    TlsConfigError(String),
}

impl From<HyperClientError> for HttpClientError {
    fn from(err: HyperClientError) -> Self {
        match err {
            HyperClientError::RequestError(e) => HttpClientError::ConnectionError(e.to_string()),
            HyperClientError::Timeout(secs) => HttpClientError::TimeoutError(secs),
            HyperClientError::InvalidRequest(e) => {
                HttpClientError::InvalidRequestError(e.to_string())
            }
            HyperClientError::FailedRequest { url, status } => {
                HttpClientError::BackendError { url, status }
            }
            HyperClientError::TlsConfigError(e) => {
                HttpClientError::ConnectionError(format!("TLS Config error: {}", e))
            }
        }
    }
}

pub struct HyperHttpClient {
    // Updated client type for HTTP/2 support
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl HyperHttpClient {
    pub fn new() -> Self {
        let mut http_connector = HttpConnector::new();
        http_connector.enforce_http(false); // Allow HTTPS URLs

        // Build rustls client config with modern protocols
        let mut root_cert_store = rustls::RootCertStore::empty();
        match load_native_certs() {
            Ok(certs) => {
                for cert in certs {
                    if root_cert_store.add(cert).is_err() {
                        tracing::warn!("Failed to add native certificate to rustls RootCertStore");
                    }
                }
                tracing::info!("Loaded {} native root certificates.", root_cert_store.len());
            }
            Err(e) => {
                tracing::error!("Could not load native root certificates: {}", e);
            }
        }

        // Configure TLS. hyper-rustls will set ALPN based on enabled HTTP versions.
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        // Build HTTPS connector with HTTP/2 support
        // HTTP/2 is already enabled via ALPN in the TLS config
        let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1() // Support HTTP/1.1
            .wrap_connector(http_connector);

        // Create client with TokioExecutor for async runtime
        let client = Client::builder(TokioExecutor::new()).build::<_, Full<Bytes>>(https_connector);

        tracing::info!("Created new HTTP client with HTTP/2 and HTTP/1.1 support");
        Self { client }
    }

    fn add_common_headers(req: &mut Request<AxumBody>) {
        let headers = req.headers_mut();
        if !headers.contains_key(header::USER_AGENT) {
            headers.insert(
                header::USER_AGENT,
                header::HeaderValue::from_static("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36")
            );
        }
        if !headers.contains_key(header::ACCEPT) {
            headers.insert(
                header::ACCEPT,
                header::HeaderValue::from_static(
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
                ),
            );
        }
        if !headers.contains_key(header::ACCEPT_LANGUAGE) {
            headers.insert(
                header::ACCEPT_LANGUAGE,
                header::HeaderValue::from_static("en-US,en;q=0.5"),
            );
        }
        if !headers.contains_key(header::CACHE_CONTROL) {
            headers.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("max-age=0"),
            );
        }
    }
}

impl HttpClient for HyperHttpClient {
    async fn send_request(
        &self,
        mut req: Request<AxumBody>,
    ) -> HttpClientResult<Response<AxumBody>> {
        Self::add_common_headers(&mut req);

        let client = self.client.clone();

        // For backend metrics, we'll use the scheme, host, and port as the backend identifier.
        let backend_identifier = format!(
            "{}://{}",
            req.uri().scheme_str().unwrap_or("http"),
            req.uri().authority().map_or_else(|| "unknown".to_string(), |a| a.to_string())
        );
        let request_path = req.uri().path().to_string();
        let request_method = req.method().to_string();

        // Start timer for backend request duration
        let _backend_timer = BackendRequestTimer::new(
            backend_identifier.clone(), 
            request_path.clone(), 
            request_method.clone()
        );

        if let Some(host_str) = req.uri().host() {
            let host_header_val = if let Some(port) = req.uri().port() {
                HeaderValue::from_str(&format!("{}:{}", host_str, port.as_u16()))
                    .unwrap_or_else(|_| HeaderValue::from_static(""))
            } else {
                HeaderValue::from_str(host_str).unwrap_or_else(|_| HeaderValue::from_static(""))
            };
            if !host_header_val.is_empty() {
                req.headers_mut()
                    .insert(hyper::header::HOST, host_header_val);
            }
        } else {
            tracing::error!("Outgoing URI has no host: {}", req.uri());
            return Err(
                HyperClientError::RequestError("Outgoing URI has no host".to_string()).into(),
            );
        }

        let (mut parts, axum_body) = req.into_parts();
        parts.version = Version::HTTP_11;

        tracing::info!(
            "Sending request: {} {} (Version set to HTTP/1.1 for upstream, ALPN negotiates)",
            parts.method,
            parts.uri
        );
        tracing::debug!("Outgoing request headers: {:?}", parts.headers);

        let bytes = match axum_body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                tracing::error!(
                    "Failed to collect request body for {} {}: {}",
                    parts.method,
                    parts.uri,
                    e
                );
                return Err(HttpClientError::ConnectionError(format!(
                    "Failed to collect request body: {}",
                    e
                )));
            }
        };
        let body = Full::new(bytes);
        let outgoing_hyper_request = Request::from_parts(parts, body);

        let method_for_error_log = outgoing_hyper_request.method().clone();
        let uri_for_error_log = outgoing_hyper_request.uri().clone();

        let response_result = client.request(outgoing_hyper_request).await;

        match response_result {
            Ok(res) => {
                // Increment backend request total counter
                increment_backend_request_total(
                    &backend_identifier,
                    &request_path,
                    &request_method,
                    res.status().as_u16(),
                );

                // Convert Hyper response body back to AxumBody
                let (parts, hyper_body) = res.into_parts();
                match hyper_body.collect().await {
                    Ok(collected_body) => {
                        let axum_body = AxumBody::from(collected_body.to_bytes());
                        Ok(Response::from_parts(parts, axum_body))
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to collect backend response body for {} {}: {}",
                            method_for_error_log,
                            uri_for_error_log,
                            e
                        );
                        Err(HttpClientError::ConnectionError(format!(
                            "Failed to collect backend response body: {}",
                            e
                        )))
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Error making request to backend {} ({} {}): {}",
                    backend_identifier,
                    method_for_error_log,
                    uri_for_error_log,
                    e
                );
                // Increment backend request total counter even for errors from the client itself (e.g., connection refused)
                // We might use a special status code like 0 or a specific error code if available.
                // For now, let's use 599 as a generic client-side error indicator for metrics.
                increment_backend_request_total(
                    &backend_identifier,
                    &request_path,
                    &request_method,
                    599, // Custom status code for client errors
                );
                Err(HyperClientError::RequestError(format!(
                    "Request to {} {} failed: {}",
                    method_for_error_log, uri_for_error_log, e
                ))
                .into())
            }
        }
    }

    async fn health_check(&self, url: &str, timeout_secs: u64) -> HttpClientResult<bool> {
        let client = self.client.clone();

        let request = Request::builder()
            .method("HEAD")
            .uri(url)
            .version(Version::HTTP_11)
            .body(Full::new(Bytes::new()))
            .map_err(HyperClientError::InvalidRequest)?;

        tracing::debug!("Health checking URL: {} (Version set to HTTP/1.1)", url);
        let timeout_duration = Duration::from_secs(timeout_secs);

        match timeout(timeout_duration, client.request(request)).await {
            Ok(result) => match result {
                Ok(response) => {
                    let is_healthy = response.status().is_success();
                    // Consume the body to prevent resource leaks
                    let _ = response.into_body().collect().await;
                    tracing::debug!("Health check for {} result: {}", url, is_healthy);
                    Ok(is_healthy)
                }
                Err(err) => {
                    tracing::debug!("Health check error for {}: {}", url, err);
                    // Return Ok(false) for connection errors during health check, consistent with original logic.
                    Ok(false)
                }
            },
            Err(_) => {
                tracing::debug!("Health check timeout for {}", url);
                Err(HttpClientError::from(HyperClientError::Timeout(
                    timeout_secs,
                )))
            }
        }
    }
}

impl Default for HyperHttpClient {
    fn default() -> Self {
        Self::new()
    }
}
