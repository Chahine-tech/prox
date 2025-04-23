use anyhow::Result;
use hyper::{Body, Request, Response};
use std::future::Future;
use std::pin::Pin;

// HttpServer defines the port (interface) for handling HTTP requests
pub trait HttpServer: Send + Sync + 'static {
    // Run the HTTP server
    fn run(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

// HttpHandler defines the port for handling HTTP requests
pub trait HttpHandler: Send + Sync + 'static {
    // Handle an incoming HTTP request
    fn handle_request(&self, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>, anyhow::Error>> + Send + '_>>;
}