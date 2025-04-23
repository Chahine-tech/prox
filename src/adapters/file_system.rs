use anyhow::Result;
use hyper::{Body, Request, Response};
use std::convert::TryFrom;
use std::future::Future;
use std::pin::Pin;
use tower::ServiceExt;
use tower_http::services::ServeDir;
use futures_util::stream::{self};

use crate::ports::file_system::FileSystem;

pub struct TowerFileSystem;

impl TowerFileSystem {
    pub fn new() -> Self {
        Self
    }
}

impl FileSystem for TowerFileSystem {
    fn serve_file(&self, root: &str, path: &str, req: Request<Body>) -> Pin<Box<dyn Future<Output = Result<Response<Body>>> + Send + '_>> {
        let root = root.to_string();
        let path = path.to_string();
        
        Box::pin(async move {
            // Create a new request with the path adjusted for ServeDir
            let uri_string = format!("/{}", path.trim_start_matches('/'));
            let uri = hyper::Uri::try_from(uri_string)?;

            let (parts, body) = req.into_parts();
            let mut new_req = Request::from_parts(parts, body);
            *new_req.uri_mut() = uri;

            // Use ServeDir from tower-http
            let serve_dir = ServeDir::new(root);
            let response = serve_dir.oneshot(new_req).await?;
            
            // Convert the tower-http response to hyper response
            let (parts, body) = response.into_parts();
            
            // Collect body into bytes and create a simpler stream
            let bytes = match hyper::body::to_bytes(body).await {
                Ok(bytes) => bytes,
                Err(err) => return Err(anyhow::anyhow!("Failed to read body: {}", err)),
            };
            
            // Create a simple one-element stream from the collected bytes
            let stream = stream::once(async move { Ok::<_, hyper::Error>(bytes) });
            
            // Create a hyper Body
            let body = Body::wrap_stream(stream);
            
            Ok(Response::from_parts(parts, body))
        })
    }
}