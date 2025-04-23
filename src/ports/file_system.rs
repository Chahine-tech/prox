use anyhow::Result;
use hyper::{Body, Request, Response};
use std::future::Future;
use std::pin::Pin;

// FileSystem defines the port (interface) for handling static files
pub trait FileSystem: Send + Sync + 'static {
    // Serve a static file from the filesystem
    fn serve_file(&self, root: &str, path: &str, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>>> + Send + '_>>;
}