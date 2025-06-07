use anyhow::Result;
use axum::body::Body as AxumBody; // Use Axum's Body type
use hyper::{Request, Response};
use thiserror::Error;

/// Error type for HTTP handler operations
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum HandlerError {
    /// Error when handling a request
    #[error("Request handling error: {0}")]
    RequestError(String),
    #[error("Internal server error: {0}")]
    InternalError(String),
    #[error("Bad gateway: {0}")]
    BadGateway(String),
    #[error("Gateway timeout: {0}")]
    GatewayTimeout(String),
    #[error("Bad request: {0}")]
    BadRequest(String),
}

/// HttpServer defines the port (interface) for handling HTTP requests
pub trait HttpServer: Send + Sync + 'static {
    /// Run the HTTP server
    ///
    /// # Returns
    /// A future that resolves when the server shuts down or encounters an error
    fn run(&self) -> impl std::future::Future<Output = Result<()>> + Send;
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
    fn handle_request(
        &self,
        req: Request<AxumBody>,
    ) -> impl std::future::Future<Output = Result<Response<AxumBody>, HandlerError>> + Send;
}
