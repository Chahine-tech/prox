use anyhow::Result;
use hyper::{Body, Request, Response};
use std::future::Future;
use std::pin::Pin;

// HttpClient defines the port (interface) for making HTTP requests to backends
pub trait HttpClient: Send + Sync + 'static {
    // Send an HTTP request to a backend server
    fn send_request(&self, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>>> + Send + '_>>;
    
    // Perform a health check on a backend
    fn health_check(&self, url: &str, timeout_secs: u64) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>>;
}