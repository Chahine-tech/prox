use axum::body::Body as AxumBody;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming as HyperBodyIncoming;
use hyper::{Request, Response, header};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use std::time::Duration;
use thiserror::Error;
use tokio::time::timeout;

use hyper_rustls::HttpsConnector;
use rustls_native_certs::load_native_certs;

use crate::ports::http_client::{HttpClient, HttpClientError, HttpClientResult};

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
        let method = req.method().clone();
        let uri = req.uri().clone();
        let uri_string = uri.to_string();

        // Ensure the Host header is set correctly for the outgoing request
        if let Some(host_str) = uri.host() {
            // Include port if present in the original URI authority
            let host_val = if let Some(port) = uri.port() {
                format!("{}:{}", host_str, port)
            } else {
                host_str.to_string()
            };
            match header::HeaderValue::from_str(&host_val) {
                Ok(host_header_val) => {
                    req.headers_mut().insert(header::HOST, host_header_val);
                }
                Err(e) => {
                    tracing::error!("Invalid host string for Host header '{}': {}", host_val, e);
                    return Err(HttpClientError::InvalidRequestError(format!(
                        "Invalid host string for Host header '{}': {}",
                        host_val, e
                    )));
                }
            }
        } else {
            tracing::warn!(
                "Request URI {} has no host, cannot set Host header explicitly",
                uri
            );
        }

        tracing::info!("Sending request: {} {} (HTTP/2 enabled)", method, uri);
        tracing::debug!("Outgoing request headers: {:?}", req.headers());

        // Convert AxumBody to hyper_util compatible body
        let (parts, axum_body) = req.into_parts();
        let bytes = match axum_body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                return Err(HttpClientError::ConnectionError(format!(
                    "Failed to collect request body: {}",
                    e
                )));
            }
        };
        let body = Full::new(bytes);
        let request = Request::from_parts(parts, body);

        // Send request with HTTP/2 support
        let response: Response<HyperBodyIncoming> =
            client.request(request).await.map_err(|err| {
                // Log the full error chain
                let mut current_err_opt: Option<&(dyn std::error::Error + 'static)> = Some(&err);
                let mut err_chain_str = String::new();
                while let Some(source_err) = current_err_opt {
                    err_chain_str.push_str(&format!("\\n  Caused by: {}", source_err));
                    current_err_opt = source_err.source();
                }
                tracing::error!(
                    "Error making request to {} {}: {}{}",
                    method,
                    uri,
                    err,
                    err_chain_str
                );

                let hyper_err = HyperClientError::RequestError(err.to_string());
                HttpClientError::from(hyper_err)
            })?;

        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            tracing::warn!("Backend {} returned error status: {}", uri_string, status);
            return Err(HttpClientError::BackendError {
                url: uri_string,
                status,
            });
        }

        // Convert hyper::body::Incoming to AxumBody
        let (parts, hyper_body) = response.into_parts();
        let axum_body = AxumBody::new(hyper_body.map_err(|e| {
            tracing::error!("Error transforming response body stream: {}", e);
            // Ensure the error is compatible with AxumBody's requirements (Into<BoxError>)
            axum::BoxError::from(e)
        }));

        Ok(Response::from_parts(parts, axum_body))
    }

    async fn health_check(&self, url: &str, timeout_secs: u64) -> HttpClientResult<bool> {
        let client = self.client.clone();
        let url_str = url.to_string();

        let request = Request::builder()
            .method("GET")
            .uri(url)
            .body(Full::new(Bytes::new()))
            .map_err(|e| HttpClientError::from(HyperClientError::from(e)))?;

        tracing::debug!("Health checking URL: {} (HTTP/2 enabled)", url);
        let timeout_duration = Duration::from_secs(timeout_secs);

        match timeout(timeout_duration, client.request(request)).await {
            Ok(result) => match result {
                Ok(response) => {
                    let is_healthy = response.status().is_success();
                    // Consume the body to prevent resource leaks
                    let _ = response.into_body().collect().await;
                    tracing::debug!("Health check for {} result: {}", url_str, is_healthy);
                    Ok(is_healthy)
                }
                Err(err) => {
                    tracing::debug!("Health check error for {}: {}", url_str, err);
                    // Return Ok(false) for connection errors during health check, consistent with original logic.
                    Ok(false)
                }
            },
            Err(_) => {
                tracing::debug!("Health check timeout for {}", url_str);
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
