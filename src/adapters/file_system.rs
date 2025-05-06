use axum::body::Body as AxumBody; // Use Axum's Body type
use hyper::{Request, Response};
use std::convert::TryFrom;
use tower::ServiceExt;
use tower_http::services::ServeDir;
use http_body_util::BodyExt; // Added import

use crate::ports::file_system::{FileSystem, FileSystemError, FileSystemResult}; // Removed FileServeFuture and added FileSystemResult

/// A file system implementation that uses tower-http's ServeDir
#[derive(Debug, Default, Clone)]
pub struct TowerFileSystem;

impl TowerFileSystem {
    /// Creates a new TowerFileSystem
    ///
    /// This is equivalent to calling `Default::default()` since TowerFileSystem has no state.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FileSystem for TowerFileSystem {
    // Update function signature to use async fn and remove Pin<Box<...>>
    async fn serve_file(&self, root: &str, path: &str, req: Request<AxumBody>) -> FileSystemResult<Response<AxumBody>> {
        let root = root.to_string();
        let path = path.to_string();
        
        // Removed Box::pin wrapper
        // Create a new request with the path adjusted for ServeDir
        let uri_string = format!("/{}", path.trim_start_matches('/'));
        let uri = hyper::Uri::try_from(uri_string)
            .map_err(|e| FileSystemError::InvalidPath(e.to_string()))?;

        let (parts, body) = req.into_parts();
        let mut new_req = Request::from_parts(parts, body);
        *new_req.uri_mut() = uri;

        // Use ServeDir from tower-http
        let serve_dir = ServeDir::new(&root);
        let response = serve_dir.oneshot(new_req).await
            .map_err(|e| FileSystemError::IoError(std::io::Error::new(
                std::io::ErrorKind::Other, 
                format!("ServeDir error: {}", e)
            )))?;

        // Convert tower_http::Response<ServeFileSystemResponseBody> to hyper::Response<AxumBody>
        let (parts, tower_body) = response.into_parts();
        let axum_body = AxumBody::new(tower_body.map_err(|e| {
            tracing::error!("Error reading static file body: {}", e);
            // Convert Infallible to a type compatible with AxumBody's error
            axum::Error::new(e) 
        }));

        Ok(Response::from_parts(parts, axum_body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_default_construction() {
        let _fs1 = TowerFileSystem::new();
        let _fs2 = TowerFileSystem::default();
        // Both instantiation methods are valid and equivalent
    }
}