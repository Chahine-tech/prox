use axum::body::Body as AxumBody;
use http_body_util::BodyExt;
use hyper::{Request, Response};
use std::convert::TryFrom;
use tower::ServiceExt;
use tower_http::services::ServeDir;

use crate::ports::file_system::{FileSystem, FileSystemError, FileSystemResult};

#[derive(Debug, Default, Clone)]
pub struct TowerFileSystem;

impl TowerFileSystem {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileSystem for TowerFileSystem {
    async fn serve_file(
        &self,
        root: &str,
        path: &str,
        req: Request<AxumBody>,
    ) -> FileSystemResult<Response<AxumBody>> {
        let root = root.to_string();
        let path = path.to_string();

        // Create a new request with the path adjusted for ServeDir
        let uri_string = format!("/{}", path.trim_start_matches('/'));
        let uri = hyper::Uri::try_from(uri_string)
            .map_err(|e| FileSystemError::InvalidPath(e.to_string()))?;

        let (parts, body) = req.into_parts();
        let mut new_req = Request::from_parts(parts, body);
        *new_req.uri_mut() = uri;

        // Use ServeDir from tower-http
        let serve_dir = ServeDir::new(&root);
        let response = serve_dir.oneshot(new_req).await.map_err(|e| {
            FileSystemError::IoError(std::io::Error::other(format!("ServeDir error: {e}")))
        })?;

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
        let _fs2 = TowerFileSystem {};
        // Both instantiation methods are valid and equivalent
    }
}
