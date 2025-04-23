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
        
        Self { client }
    }
}

impl HttpClient for HyperHttpClient {
    fn send_request(&self, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>>> + Send + '_>> {
        let client = self.client.clone();
        
        Box::pin(async move {
            let response = client.request(req).await?;
            Ok(response)
        })
    }
    
    fn health_check(&self, url: &str, timeout_secs: u64) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>> {
        let client = self.client.clone();
        let url = url.to_string();
        
        Box::pin(async move {
            // Create request
            let req = Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())?;
            
            // Perform the health check with timeout
            let timeout_duration = Duration::from_secs(timeout_secs);
            let result = timeout(timeout_duration, client.request(req)).await;
            
            match result {
                Ok(Ok(response)) => {
                    // Check if status code is in the 2xx range
                    Ok(response.status().is_success())
                },
                _ => {
                    // Either timeout or error means unhealthy
                    Ok(false)
                }
            }
        })
    }
}