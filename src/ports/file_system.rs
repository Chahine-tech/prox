use anyhow::Result;
use axum::body::Body as AxumBody; // Use Axum's Body type
use hyper::{Request, Response};
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

/// FileSystem defines the port (interface) for handling static files
pub trait FileSystem: Send + Sync + 'static {
    /// Serve a file from the file system
    ///
    /// # Arguments
    /// * `root` - The root directory to serve files from
    /// * `path` - The path to the file relative to the root
    /// * `req` - The original HTTP request
    ///
    /// # Returns
    /// A future that resolves to the file response or an error
    fn serve_file(
        &self,
        root: &str,
        path: &str,
        req: Request<AxumBody>,
    ) -> impl std::future::Future<Output = FileSystemResult<Response<AxumBody>>> + Send;
}
