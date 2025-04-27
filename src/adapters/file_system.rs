use hyper::{Body, Request, Response};
use std::convert::TryFrom;
use tower::ServiceExt;
use tower_http::services::ServeDir;
use futures_util::stream::{self};

use crate::ports::file_system::{FileSystem, FileSystemError, FileServeFuture};

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
    fn serve_file<'a>(&'a self, root: &'a str, path: &'a str, req: Request<Body>) -> FileServeFuture<'a> {
        let root = root.to_string();
        let path = path.to_string();
        
        Box::pin(async move {
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
                    format!("Failed to serve file: {}", e)
                )))?;
            
            // Convert the tower-http response to hyper response
            let (parts, body) = response.into_parts();
            
            // Collect body into bytes and create a simpler stream
            let bytes = match hyper::body::to_bytes(body).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    return Err(FileSystemError::IoError(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Failed to read body: {}", err)
                    )));
                }
            };
            
            // Create a simple one-element stream from the collected bytes
            let stream = stream::once(async move { Ok::<_, hyper::Error>(bytes) });
            
            // Create a hyper Body
            let body = Body::wrap_stream(stream);
            
            Ok(Response::from_parts(parts, body))
        })
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