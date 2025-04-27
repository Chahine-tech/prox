use anyhow::Result;
use hyper::{Body, Request, Response, StatusCode};
use std::future::Future;
use std::pin::Pin;
use thiserror::Error;

/// Custom error type for HTTP client operations
#[derive(Error, Debug)]
#[non_exhaustive]  // Signal that more variants may be added in the future
pub enum HttpClientError {
    /// Error when connection to backend fails
    #[error("Connection error: {0}")]
    ConnectionError(String),
    
    /// Error when request times out
    #[error("Timeout error after {0} seconds")]
    TimeoutError(u64),
    
    /// Error when request is invalid
    #[error("Invalid request: {0}")]
    InvalidRequestError(String),
    
    /// Error when backend returns an error status code
    #[error("Backend returned error status: {status}, url: {url}")]
    BackendError {
        /// The URL that was requested
        url: String,
        /// The status code returned by the backend
        status: StatusCode,
    },
}

/// Result type alias for HTTP client operations
pub type HttpClientResult<T> = Result<T, HttpClientError>;

/// Type alias for async HTTP request responses
pub type HttpResponseFuture<'a> = Pin<Box<dyn Future<Output = HttpClientResult<Response<Body>>> + Send + 'a>>;

/// Type alias for async health check responses
pub type HealthCheckFuture<'a> = Pin<Box<dyn Future<Output = HttpClientResult<bool>> + Send + 'a>>;

/// HttpClient defines the port (interface) for making HTTP requests to backends
pub trait HttpClient: Send + Sync + 'static {
    /// Send an HTTP request to a backend server
    /// 
    /// # Arguments
    /// * `req` - The HTTP request to send to the backend
    /// 
    /// # Returns
    /// A future that resolves to the backend's response or an error
    fn send_request<'a>(&'a self, req: Request<Body>) -> HttpResponseFuture<'a>;
    
    /// Perform a health check on a backend
    /// 
    /// # Arguments
    /// * `url` - The URL to check
    /// * `timeout_secs` - Timeout in seconds
    /// 
    /// # Returns
    /// A future that resolves to true if the backend is healthy, false otherwise
    fn health_check<'a>(&'a self, url: &'a str, timeout_secs: u64) -> HealthCheckFuture<'a>;
}