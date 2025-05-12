use axum::body::Body as AxumBody; // Use Axum's Body type
use hyper::{Request, header, Response}; // Added Response
use hyper::body::Incoming as HyperBodyIncoming; // Added for clarity
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::time::Duration;
use tokio::time::timeout;
use thiserror::Error;
use http_body_util::BodyExt; // Added for map_frame

use crate::ports::http_client::{
    HttpClient, HttpClientError, HttpClientResult // Removed HttpResponseFuture, HealthCheckFuture and added HttpClientResult
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
    }
}

impl From<HyperClientError> for HttpClientError {
    fn from(err: HyperClientError) -> Self {
        match err {
            HyperClientError::RequestError(e) => HttpClientError::ConnectionError(e.to_string()),
            HyperClientError::Timeout(secs) => HttpClientError::TimeoutError(secs),
            HyperClientError::InvalidRequest(e) => HttpClientError::InvalidRequestError(e.to_string()),
            HyperClientError::FailedRequest { url, status } => HttpClientError::BackendError { url, status },
        }
    }
}

pub struct HyperHttpClient {
    // Use AxumBody as the concrete body type for the client
    client: Client<hyper_tls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>, AxumBody>
}

impl HyperHttpClient {
    pub fn new() -> Self {
        let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
        http.enforce_http(false);
        let https = hyper_tls::HttpsConnector::new_with_connector(http);

        // Build the client specifying AxumBody
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build::<_, AxumBody>(https); // Specify AxumBody type

        tracing::info!("Created new HTTPS-capable HTTP client");
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
                header::HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8")
            );
        }
        if !headers.contains_key(header::ACCEPT_LANGUAGE) {
            headers.insert(
                header::ACCEPT_LANGUAGE,
                header::HeaderValue::from_static("en-US,en;q=0.5")
            );
        }
        if !headers.contains_key(header::CACHE_CONTROL) {
            headers.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("max-age=0")
            );
        }
    }
}

impl HttpClient for HyperHttpClient {
    // Update function signature to use async fn and remove Pin<Box<...>>
    async fn send_request(&self, mut req: Request<AxumBody>) -> HttpClientResult<Response<AxumBody>> {
        Self::add_common_headers(&mut req);

        let client = self.client.clone();
        let method = req.method().clone();
        let uri = req.uri().clone();
        let uri_string = uri.to_string();

        // Removed Box::pin wrapper
        tracing::info!("Sending request: {} {}", method, uri);
        
        // Make the request - response body is hyper::body::Incoming
        let response: Response<HyperBodyIncoming> = client.request(req).await
            .map_err(|err| {
                tracing::error!("Error making request to {} {}: {}", method, uri, err);
                // Convert hyper_util error to string for HyperClientError::RequestError
                let hyper_err = HyperClientError::RequestError(err.to_string()); 
                HttpClientError::from(hyper_err)
            })?;

        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            tracing::warn!("Backend {} returned error status: {}", uri_string, status);
            return Err(HttpClientError::BackendError { url: uri_string, status });
        }

        // Convert hyper::body::Incoming to AxumBody
        let (parts, hyper_body) = response.into_parts();
        let axum_body = AxumBody::new(hyper_body.map_err(|e| {
            tracing::error!("Error reading response body: {}", e);
            // Convert hyper::Error to a type compatible with AxumBody's error
            axum::Error::new(e)
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
         match timeout(timeout_duration, client.request(req)).await { // request takes Request<AxumBody>
            Ok(result) => match result {
                Ok(response) => {
                    let is_healthy = response.status().is_success();
                    // Consume the body to prevent resource leaks
                    let _ = response.into_body().collect().await;
                    tracing::debug!("Health check for {} result: {}", url, is_healthy);
                    Ok(is_healthy)
                },
                Err(err) => {
                    // Convert hyper_util error to string before creating HttpClientError
                    tracing::debug!("Health check error for {}: {}", url, err);
                    // Directly return Ok(false) as per original logic for connection errors during health check
                    Ok(false) 
                },
            },
            Err(_) => {
                tracing::debug!("Health check timeout for {}", url);
                Err(HttpClientError::from(HyperClientError::Timeout(timeout_secs)))
            }
         }
    }
}

impl Default for HyperHttpClient {
    fn default() -> Self {
        Self::new()
    }
}