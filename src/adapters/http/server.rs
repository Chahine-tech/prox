use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Result};
use axum::{
    extract::Extension,
    http::Request,
    response::{IntoResponse, Response},
    Router,
};
use hyper::{Body, StatusCode};
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;

use crate::adapters::http_handler::HyperHandler;
use crate::config::ServerConfig;
use crate::core::ProxyService;
use crate::ports::http_server::{HttpServer, HttpHandler, HandlerError};
use crate::ports::{file_system::FileSystem, http_client::HttpClient};

pub struct HyperServer {
    proxy_service: Arc<ProxyService>,
    config: Arc<ServerConfig>,
    http_client: Arc<dyn HttpClient>,
    file_system: Arc<dyn FileSystem>,
}

impl HyperServer {
    pub fn with_dependencies(
        proxy_service: Arc<ProxyService>,
        config: Arc<ServerConfig>,
        http_client: Arc<dyn HttpClient>,
        file_system: Arc<dyn FileSystem>,
    ) -> Self {
        Self {
            proxy_service,
            config,
            http_client,
            file_system,
        }
    }

    async fn build_server(&self) -> Result<axum::Server<hyper::server::conn::AddrIncoming, axum::routing::IntoMakeService<Router>>> {
        // Parse listen address
        let addr: SocketAddr = self.config.listen_addr.parse()?;
        
        // Create the HTTP handler
        let handler = HyperHandler::new(
            self.proxy_service.clone(),
            self.http_client.clone(),
            self.file_system.clone(),
        );
        
        // Build our application with a fallback route
        let app = Router::new().fallback(move |req: Request<Body>| {
            handle_request(handler.clone(), req)
        }).layer(
            ServiceBuilder::new()
                .layer(Extension(self.proxy_service.clone()))
                .layer(TraceLayer::new_for_http()),
        );
        
        // Create the server
        let server = axum::Server::bind(&addr).serve(app.into_make_service());
        
        Ok(server)
    }
}

impl HttpServer for HyperServer {
    fn run<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Check if TLS is configured
            if let Some(tls_config) = &self.config.tls {
                // Start HTTPS server
                tracing::info!("Starting server with TLS");

                // Load certificate and private key
                let cert_path = &tls_config.cert_path;
                let key_path = &tls_config.key_path;
                
                use tokio::fs;
                
                let cert = fs::read(cert_path).await?;
                let key = fs::read(key_path).await?;

                // Parse the certificate and private key
                let cert_chain = rustls_pemfile::certs(&mut cert.as_slice())?
                    .into_iter().map(rustls::Certificate).collect();

                let mut keys = match rustls_pemfile::pkcs8_private_keys(&mut key.as_slice()) {
                    Ok(keys) => keys,
                    Err(_) => {
                        // Try RSA key format if PKCS8 fails
                        rustls_pemfile::rsa_private_keys(&mut key.as_slice())?
                    }
                };

                if keys.is_empty() {
                    return Err(anyhow!("No private keys found in the key file"));
                }

                let server_key = rustls::PrivateKey(keys.remove(0));

                // Configure TLS
                let tls_config = rustls::ServerConfig::builder()
                    .with_safe_defaults()
                    .with_no_client_auth()
                    .with_single_cert(cert_chain, server_key)?;

                // Create server configuration
                let tls_acceptor = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

                // Parse listen address
                let addr: SocketAddr = self.config.listen_addr.parse()?;
                
                // Create the HTTP handler
                let handler = HyperHandler::new(
                    self.proxy_service.clone(),
                    self.http_client.clone(),
                    self.file_system.clone(),
                );
                
                let app = Router::new().fallback(move |req: Request<Body>| {
                    handle_request(handler.clone(), req)
                }).layer(
                    ServiceBuilder::new()
                        .layer(Extension(self.proxy_service.clone()))
                        .layer(TraceLayer::new_for_http()),
                );

                // Start TLS server
                tracing::info!("Starting secure server on {}", addr);
                axum_server::bind_rustls(addr, tls_acceptor)
                    .serve(app.into_make_service())
                    .await
                    .map_err(|e| anyhow!("Server error: {}", e))?;
            } else {
                // Standard HTTP server
                tracing::info!("Starting server without TLS on {}", self.config.listen_addr);
                
                let server = self.build_server().await?;
                server.await.map_err(|e| anyhow!("Server error: {}", e))?;
            }

            Ok(())
        })
    }
}

async fn handle_request(
    handler: HyperHandler,
    req: Request<Body>,
) -> Result<Response, Infallible> {
    match handler.handle_request(req).await {
        Ok(response) => {
            // Convert response to the axum expected type
            let (parts, body) = response.into_parts();
            let body = axum::body::boxed(body);
            Ok(Response::from_parts(parts, body))
        },
        Err(e) => {
            // Map error to appropriate status code
            let response = match e {
                HandlerError::RequestError(err) => {
                    tracing::error!("Request error: {}", err);
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("Request error: {}", err)).into_response()
                }
            };
            
            Ok(response)
        }
    }
}