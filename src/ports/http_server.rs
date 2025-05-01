use anyhow::Result;
use axum::body::Body as AxumBody; // Use Axum's Body type
use hyper::{Request, Response};
use std::future::Future;
use std::pin::Pin;
use thiserror::Error;

/// Error type for HTTP handler operations
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum HandlerError {
    /// Error when handling a request
    #[error("Request handling error: {0}")]
    RequestError(String),
}

/// Type alias for HTTP server run futures
pub type ServerRunFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Type alias for HTTP handler response futures
// Use AxumBody in Response generic
pub type HandlerResponseFuture<'a> = Pin<Box<dyn Future<Output = Result<Response<AxumBody>, HandlerError>> + Send + 'a>>;

/// HttpServer defines the port (interface) for handling HTTP requests
pub trait HttpServer: Send + Sync + 'static {
    /// Run the HTTP server
    /// 
    /// # Returns
    /// A future that resolves when the server shuts down or encounters an error
    fn run<'a>(&'a self) -> ServerRunFuture<'a>;
}

/// HttpHandler defines the port for handling HTTP requests
pub trait HttpHandler: Send + Sync + 'static {
    /// Handle an incoming HTTP request
    /// 
    /// # Arguments
    /// * `req` - The HTTP request to handle
    /// 
    /// # Returns
    /// A future that resolves to an HTTP response or an error
    // Use AxumBody in Request generic
    fn handle_request<'a>(&'a self, req: Request<AxumBody>) -> HandlerResponseFuture<'a>;
}