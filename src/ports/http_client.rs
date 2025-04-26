use anyhow::Result;
use hyper::{Body, Request, Response};
use std::future::Future;
use std::pin::Pin;
use thiserror::Error;

/// Custom error type for HTTP client operations
#[derive(Error, Debug)]
pub enum HttpClientError {
    #[error("Adapter error: {0}")]
    AdapterError(String),
}

/// Result type alias for HTTP client operations
pub type HttpClientResult<T> = Result<T, HttpClientError>;

// HttpClient defines the port (interface) for making HTTP requests to backends
pub trait HttpClient: Send + Sync + 'static {
    // Send an HTTP request to a backend server
    fn send_request<'a>(&'a self, req: Request<Body>) -> Pin<Box<dyn Future<Output = HttpClientResult<Response<Body>>> + Send + 'a>>;
    
    // Perform a health check on a backend
    fn health_check<'a>(&'a self, url: &'a str, timeout_secs: u64) -> Pin<Box<dyn Future<Output = HttpClientResult<bool>> + Send + 'a>>;
}