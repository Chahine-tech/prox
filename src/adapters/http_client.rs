use thiserror::Error;
use hyper::{Body, Request, Response, header};
use std::future::Future;
use std::pin::Pin;
use tokio::time::timeout;
use std::time::Duration;

use crate::ports::http_client::{HttpClient, HttpClientError, HttpClientResult};

/// Custom error type for HTTP client operations
#[derive(Error, Debug)]
pub enum HyperClientError {
    #[error("HTTP request error: {0}")]
    RequestError(#[from] hyper::Error),
    
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
        HttpClientError::AdapterError(err.to_string())
    }
}

pub struct HyperHttpClient {
    client: hyper::Client<hyper_tls::HttpsConnector<hyper::client::connect::HttpConnector>>
}

impl HyperHttpClient {
    pub fn new() -> Self {
        // Create HTTPS-capable client
        // Using hyper-tls with insecure mode turned on
        let mut http = hyper::client::connect::HttpConnector::new();
        http.enforce_http(false);
        
        // Configure HTTPS connector to accept invalid certs for development
        let https = hyper_tls::HttpsConnector::new_with_connector(http);
        
        // Build the client with our custom connector
        let client = hyper::Client::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build::<_, Body>(https);
        
        tracing::info!("Created new HTTPS-capable HTTP client");
        Self { client }
    }
    
    // Add common headers to make requests more browser-like
    fn add_common_headers(req: &mut Request<Body>) {
        let headers = req.headers_mut();
        
        // Only add headers if they don't exist already
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
        
        // Add headers to look like a regular browser request
        if !headers.contains_key(header::CACHE_CONTROL) {
            headers.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("max-age=0")
            );
        }
    }
}

impl HttpClient for HyperHttpClient {
    fn send_request<'a>(&'a self, mut req: Request<Body>) -> Pin<Box<dyn Future<Output = HttpClientResult<Response<Body>>> + Send + 'a>> {
        // Add common headers to make the request more browser-like
        Self::add_common_headers(&mut req);
        
        let client = self.client.clone();
        let method = req.method().clone();
        let uri = req.uri().clone();
        let uri_string = uri.to_string();
        
        Box::pin(async move {
            tracing::info!("Sending request: {} {}", method, uri);
            
            match client.request(req).await {
                Ok(response) => {
                    let status = response.status();
                    tracing::info!("Received response from {} {}: status={}", method, uri, status);
                    
                    if status.is_client_error() || status.is_server_error() {
                        Err(HyperClientError::FailedRequest { 
                            url: uri_string, 
                            status 
                        }.into())
                    } else {
                        Ok(response)
                    }
                },
                Err(err) => {
                    tracing::error!("Error making request to {} {}: {}", method, uri, err);
                    Err(HyperClientError::RequestError(err).into())
                }
            }
        })
    }
    
    fn health_check<'a>(&'a self, url: &'a str, timeout_secs: u64) -> Pin<Box<dyn Future<Output = HttpClientResult<bool>> + Send + 'a>> {
        let client = self.client.clone();
        let url = url.to_string();
        
        Box::pin(async move {
            // Create request
            let mut req = Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .map_err(HyperClientError::from)?;
            
            // Add common headers to make the request more browser-like
            HyperHttpClient::add_common_headers(&mut req);
            
            tracing::debug!("Health checking URL: {}", url);
            
            // Perform the health check with timeout
            let timeout_duration = Duration::from_secs(timeout_secs);
            let result = timeout(timeout_duration, client.request(req)).await;
            
            match result {
                Ok(Ok(response)) => {
                    // Check if status code is in the 2xx range
                    let is_healthy = response.status().is_success();
                    tracing::debug!("Health check for {} result: {}", url, is_healthy);
                    Ok(is_healthy)
                },
                Ok(Err(err)) => {
                    tracing::debug!("Health check error for {}: {}", url, err);
                    Ok(false)
                },
                Err(_) => {
                    tracing::debug!("Health check timeout for {}", url);
                    Err(HyperClientError::Timeout(timeout_secs).into())
                }
            }
        })
    }
}