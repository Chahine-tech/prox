use anyhow::Result;
use hyper::{Body, Request, Response};
use std::future::Future;
use std::pin::Pin;
use thiserror::Error;

/// Error type for file system operations
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum FileSystemError {
    /// Error when encountering an IO issue
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    /// Error when path is invalid
    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

/// Result type for file system operations
pub type FileSystemResult<T> = Result<T, FileSystemError>;

/// Type alias for async file serving responses
pub type FileServeFuture<'a> = Pin<Box<dyn Future<Output = FileSystemResult<Response<Body>>> + Send + 'a>>;

/// FileSystem defines the port (interface) for handling static files
pub trait FileSystem: Send + Sync + 'static {
    /// Serve a static file from the filesystem
    /// 
    /// # Arguments
    /// * `root` - The root directory from which to serve files
    /// * `path` - The relative path to the requested file
    /// * `req` - The original HTTP request
    /// 
    /// # Returns
    /// A future that resolves to the file response or an error
    fn serve_file<'a>(&'a self, root: &'a str, path: &'a str, req: Request<Body>) -> FileServeFuture<'a>;
}