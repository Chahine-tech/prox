use anyhow::Result;
use hyper::{Body, Request, Response};
use std::future::Future;
use std::pin::Pin;
use tokio::time::timeout;
use std::time::Duration;

use crate::ports::http_client::HttpClient;

pub struct HyperHttpClient {
    client: hyper::Client<hyper_tls::HttpsConnector<hyper::client::connect::HttpConnector>>
}

impl HyperHttpClient {
    pub fn new() -> Self {
        // Create HTTPS-capable client
        let https = hyper_tls::HttpsConnector::new();
        let client = hyper::Client::builder().build::<_, Body>(https);
        
        tracing::info!("Created new HTTPS-capable HTTP client");
        Self { client }
    }
}

impl HttpClient for HyperHttpClient {
    fn send_request<'a>(&'a self, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>>> + Send + 'a>> {
        let client = self.client.clone();
        let method = req.method().clone();
        let uri = req.uri().clone();
        
        Box::pin(async move {
            tracing::info!("Sending request: {} {}", method, uri);
            
            match client.request(req).await {
                Ok(response) => {
                    tracing::info!("Received response from {} {}: status={}", method, uri, response.status());
                    Ok(response)
                },
                Err(err) => {
                    tracing::error!("Error making request to {} {}: {}", method, uri, err);
                    Err(anyhow::anyhow!("HTTP request error: {}", err))
                }
            }
        })
    }
    
    fn health_check<'a>(&'a self, url: &'a str, timeout_secs: u64) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        let client = self.client.clone();
        let url = url.to_string();
        
        Box::pin(async move {
            // Create request
            let req = Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())?;
            
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
                    Ok(false)
                }
            }
        })
    }
}