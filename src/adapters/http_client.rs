use axum::body::Body as AxumBody; // Use Axum's Body type
use http_body_util::BodyExt;
use hyper::body::Incoming as HyperBodyIncoming; // Added for clarity
use hyper::{Request, Response, header}; // Added Response
// Use legacy client from hyper-util for Hyper 1.0
use hyper_util::client::legacy::Client as HyperClientV1;
use hyper_util::client::legacy::connect::HttpConnector as HyperUtilHttpConnector; // Use legacy connector
use hyper_util::rt::TokioExecutor;
use std::time::Duration;
use thiserror::Error;
use tokio::time::timeout; // Added for map_frame

// Imports for hyper-rustls
use hyper_rustls::HttpsConnector;
use rustls_native_certs::load_native_certs;

use crate::ports::http_client::{
    HttpClient,
    HttpClientError,
    HttpClientResult, // Removed HttpResponseFuture, HealthCheckFuture and added HttpClientResult
};

/// Custom error type for HTTP client operations
#[derive(Error, Debug)]
pub enum HyperClientError {
    #[error("HTTP request error: {0}")]
    RequestError(String), // Changed from hyper::Error to String to accommodate hyper_util error

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
    client: HyperClientV1<
        HttpsConnector<HyperUtilHttpConnector>, // Changed from hyper_tls::HttpsConnector
        AxumBody,
    >,
}

impl HyperHttpClient {
    pub fn new() -> Self {
        let mut http_connector = HyperUtilHttpConnector::new(); // Create the modern connector and make it mutable
        http_connector.enforce_http(false); // Allow non-HTTP URIs for the HttpsConnector to handle
        // This is crucial for https_or_http() to work correctly,
        // as the HttpsConnector needs to pass https URIs to the
        // underlying connector for TCP stream setup before TLS.

        // Build rustls client config
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
                // Depending on policy, you might panic here or proceed with an empty store
                // which will likely cause handshake failures.
            }
        }
        if root_cert_store.is_empty() {
            tracing::warn!(
                "Rustls RootCertStore is empty. HTTPS connections will likely fail handshake unless custom certs are used for specific endpoints or server sends full chain."
            );
        }

        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http() // Allow both HTTPS and HTTP
            .enable_http1() // enable_http2() is often not needed explicitly or handled by underlying hyper/http-body-util
            // .enable_http2() // Typically, HTTP/2 is negotiated if both client and server support it.
            // If you specifically need to force or ensure HTTP/2, ensure your hyper and http-body-util versions support it well.
            .wrap_connector(http_connector);

        // Build the Hyper 1.0 client
        let client = HyperClientV1::builder(TokioExecutor::new())
            // Optional: Configure Hyper 1.0 client's pooling, e.g.:
            // .pool_idle_timeout(Duration::from_secs(90))
            // .pool_max_idle_per_host(10)
            .build::<_, AxumBody>(https_connector);

        tracing::info!("Created new HTTPS-capable HTTP client (Hyper 1.0 with hyper-rustls based)");
        Self { client }
    }

    // Update function signature to use AxumBody
    fn add_common_headers(req: &mut Request<AxumBody>) {
        let headers = req.headers_mut();
        if !headers.contains_key(header::USER_AGENT) {
            headers.insert(
                header::USER_AGENT,
                header::HeaderValue::from_static("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
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
    // Update function signature to use async fn and remove Pin<Box<...>>
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
            // Optionally, return an error or rely on Hyper's default behavior
        }

        tracing::info!("Sending request: {} {}", method, uri);
        tracing::debug!("Outgoing request headers: {:?}", req.headers());

        // Make the request - response body is hyper::body::Incoming
        let response: Response<HyperBodyIncoming> = client.request(req).await.map_err(|err| {
            // Log the full error chain
            let mut current_err_opt: Option<&(dyn std::error::Error + 'static)> = Some(&err);
            let mut err_chain_str = String::new();
            while let Some(source_err) = current_err_opt {
                err_chain_str.push_str(&format!("\n  Caused by: {}", source_err));
                current_err_opt = source_err.source();
            }
            tracing::error!(
                "Error making request to {} {}: {}{}",
                method,
                uri,
                err,           // Original top-level error
                err_chain_str  // Formatted chain of source errors
            );

            // Convert hyper_util error to string for HyperClientError::RequestError
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

    // Update function signature to use async fn and remove Pin<Box<...>>
    async fn health_check(&self, url: &str, timeout_secs: u64) -> HttpClientResult<bool> {
        let client = self.client.clone();
        let url = url.to_string();

        // Removed Box::pin wrapper
        // Create request with an empty AxumBody
        let mut req = Request::builder()
            .method("GET")
            .uri(&url)
            .body(AxumBody::empty()) // Use AxumBody::empty()
            .map_err(|e| {
                let hyper_err = HyperClientError::from(e);
                HttpClientError::from(hyper_err)
            })?;

        HyperHttpClient::add_common_headers(&mut req);
        tracing::debug!("Health checking URL: {}", url);
        let timeout_duration = Duration::from_secs(timeout_secs);
        match timeout(timeout_duration, client.request(req)).await {
            // request takes Request<AxumBody>
            Ok(result) => match result {
                Ok(response) => {
                    let is_healthy = response.status().is_success();
                    // Consume the body to prevent resource leaks
                    let _ = response.into_body().collect().await;
                    tracing::debug!("Health check for {} result: {}", url, is_healthy);
                    Ok(is_healthy)
                }
                Err(err) => {
                    // Convert hyper_util error to string before creating HttpClientError
                    tracing::debug!("Health check error for {}: {}", url, err);
                    // Directly return Ok(false) as per original logic for connection errors during health check
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
