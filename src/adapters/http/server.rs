use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::body::Body as AxumBody; // Use Axum's Body type
use axum::{
    Router,
    extract::Extension,
    http::Request,
    response::{IntoResponse, Response as AxumResponse},
};
use axum_server::tls_rustls::RustlsConfig;
use http_body_util::BodyExt; // Removed unnecessary braces
use hyper::StatusCode;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer; // Import CryptoProvider

use crate::adapters::http_handler::HyperHandler;
use crate::config::ServerConfig;
use crate::core::ProxyService;
use crate::ports::http_server::{HandlerError, HttpHandler, HttpServer};
// Remove direct imports of HttpClient and FileSystem traits if only concrete types are used.
// use crate::ports::{file_system::FileSystem, http_client::HttpClient};
use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http_client::HyperHttpClient; // Import concrete type // Import concrete type

pub struct HyperServer {
    proxy_service: Arc<ProxyService>,
    config: Arc<ServerConfig>,
    http_client: Arc<HyperHttpClient>, // Use concrete type
    file_system: Arc<TowerFileSystem>, // Use concrete type
}

impl HyperServer {
    pub fn with_dependencies(
        proxy_service: Arc<ProxyService>,
        config: Arc<ServerConfig>,
        http_client: Arc<HyperHttpClient>, // Use concrete type
        file_system: Arc<TowerFileSystem>, // Use concrete type
    ) -> Self {
        Self {
            proxy_service,
            config,
            http_client,
            file_system,
        }
    }

    async fn build_app(&self) -> Router {
        let handler = HyperHandler::new(
            self.proxy_service.clone(),
            self.http_client.clone(),
            self.file_system.clone(),
        );

        // Update fallback closure to accept Request<AxumBody>
        Router::new()
            .fallback(move |req: Request<AxumBody>| handle_request(handler.clone(), req))
            .layer(
                ServiceBuilder::new()
                    .layer(Extension(self.proxy_service.clone()))
                    .layer(TraceLayer::new_for_http()),
            )
    }
}

impl HttpServer for HyperServer {
    // Changed signature to use async fn and removed Pin<Box<...>>
    async fn run(&self) -> Result<()> {
        // Removed Box::pin wrapper
        let app = self.build_app().await;
        let addr: SocketAddr = self
            .config
            .listen_addr
            .parse()
            .with_context(|| format!("Invalid listen address: {}", self.config.listen_addr))?;

        if let Some(tls_config) = &self.config.tls {
            tracing::info!("Starting server with TLS on {}", addr);
            let cert_path = &tls_config.cert_path;
            let key_path = &tls_config.key_path;
            use tokio::fs;
            let cert_data = fs::read(cert_path)
                .await
                .with_context(|| format!("Failed to read certificate file: {}", cert_path))?;
            let key_data = fs::read(key_path)
                .await
                .with_context(|| format!("Failed to read key file: {}", key_path))?;
            let cert_chain: Vec<CertificateDer<'static>> =
                rustls_pemfile::certs(&mut cert_data.as_slice())
                    .collect::<Result<_, _>>()
                    .context("Failed to parse certificate PEM")?;
            let key_der: PrivateKeyDer<'static> =
                rustls_pemfile::private_key(&mut key_data.as_slice())
                    .with_context(|| format!("Failed to parse private key file: {}", key_path))?
                    .ok_or_else(|| anyhow!("No private key found in {}", key_path))?;

            // Configure TLS with a crypto provider
            CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider()) // Install aws_lc_rs as default
                .map_err(|e| anyhow!("Failed to install default crypto provider: {:?}", e))?; // Use debug formatting

            // Build the config using the installed default provider
            let server_config = rustls::ServerConfig::builder() // Build without explicit provider
                .with_no_client_auth()
                .with_single_cert(cert_chain, key_der)
                .context("Failed to create TLS server config")?;

            let tls_acceptor = RustlsConfig::from_config(Arc::new(server_config));

            axum_server::bind_rustls(addr, tls_acceptor)
                .serve(app.into_make_service())
                .await
                .map_err(|e| anyhow!("TLS Server error: {}", e))?;
        } else {
            tracing::info!("Starting server without TLS on {}", addr);
            let listener = TcpListener::bind(addr)
                .await
                .with_context(|| format!("Failed to bind to address: {}", addr))?;
            axum::serve(listener, app.into_make_service()) // Use axum::serve
                .await
                .map_err(|e| anyhow!("HTTP Server error: {}", e))?;
        }

        Ok(())
    }
}

// Update handle_request signature
async fn handle_request(
    handler: HyperHandler,
    req: Request<AxumBody>, // Use AxumBody
) -> Result<AxumResponse, Infallible> {
    // Return AxumResponse
    match handler.handle_request(req).await {
        // handle_request now takes AxumBody
        Ok(response) => {
            // Convert hyper::Response<AxumBody> to axum::response::Response
            let (parts, hyper_body) = response.into_parts();

            // Collect body into bytes
            let bytes = match hyper_body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(err) => {
                    tracing::error!("Failed to collect response body in handler: {}", err);
                    return Ok((StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
                        .into_response());
                }
            };

            // Create an Axum body (which is already the correct type)
            let axum_body = AxumBody::from(bytes);
            Ok(AxumResponse::from_parts(parts, axum_body))
        }
        Err(e) => {
            let response = match e {
                HandlerError::RequestError(err) => {
                    tracing::error!("Request error: {}", err);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Request error: {}", err),
                    )
                        .into_response()
                }
            };
            Ok(response)
        }
    }
}
