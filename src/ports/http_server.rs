use anyhow::Result;
use hyper::{Body, Request, Response};
use std::future::Future;
use std::pin::Pin;

// HttpServer defines the port (interface) for handling HTTP requests
pub trait HttpServer: Send + Sync + 'static {
    // Run the HTTP server
    fn run<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

// HttpHandler defines the port for handling HTTP requests
pub trait HttpHandler: Send + Sync + 'static {
    // Handle an incoming HTTP request
    fn handle_request<'a>(&'a self, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>, anyhow::Error>> + Send + 'a>>;
}