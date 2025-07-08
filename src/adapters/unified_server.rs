use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};

use crate::adapters::file_system::TowerFileSystem;
use crate::adapters::http::server::HyperServer;
use crate::adapters::http_client::HyperHttpClient;
use crate::adapters::http3::Http3Server;
use crate::config::models::ServerConfig;
use crate::core::ProxyService;
use crate::ports::http_server::HttpServer;
use crate::utils::graceful_shutdown::GracefulShutdown;

pub struct UnifiedServer {
    http_server: HyperServer,
    http3_server: Option<Http3Server>,
    graceful_shutdown: Arc<GracefulShutdown>,
}

impl UnifiedServer {
    pub async fn new(
        proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
        config_holder: Arc<RwLock<Arc<ServerConfig>>>,
        http_client: Arc<HyperHttpClient>,
        file_system: Arc<TowerFileSystem>,
        health_checker_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
        graceful_shutdown: Arc<GracefulShutdown>,
    ) -> Result<Self> {
        // Create the traditional HTTP server (handles HTTP/1.1 and HTTP/2 over TCP)
        let http_server = HyperServer::with_dependencies(
            proxy_service_holder.clone(),
            config_holder.clone(),
            http_client.clone(),
            file_system.clone(),
            health_checker_handle,
            graceful_shutdown.clone(),
        );

        // Check if HTTP/3 is enabled and create HTTP/3 server if needed
        let http3_server = {
            let config = config_holder.read().map_err(|e| {
                anyhow::anyhow!("Failed to acquire config read lock for HTTP/3 setup: {}", e)
            })?;

            if config.protocols.http3_enabled {
                // HTTP/3 requires TLS
                if let Some(ref tls_config) = config.tls {
                    let (cert_path, key_path) = if let Some(acme_config) = &tls_config.acme {
                        if acme_config.enabled {
                            // For ACME, we would need to get the certificate paths
                            // This is simplified - in practice, you'd coordinate with ACME service
                            return Err(anyhow!(
                                "ACME with HTTP/3 requires coordination - not implemented in this example"
                            ));
                        } else {
                            return Err(anyhow!("ACME is configured but not enabled"));
                        }
                    } else if let (Some(cert_path), Some(key_path)) =
                        (&tls_config.cert_path, &tls_config.key_path)
                    {
                        (cert_path.clone(), key_path.clone())
                    } else {
                        return Err(anyhow!("HTTP/3 requires TLS certificate configuration"));
                    };

                    // Parse the listen address and create UDP address for HTTP/3
                    let tcp_addr: SocketAddr = config
                        .listen_addr
                        .parse()
                        .context("Invalid listen address")?;
                    let udp_addr = SocketAddr::new(tcp_addr.ip(), tcp_addr.port());

                    let http3_config = config
                        .protocols
                        .http3_config
                        .as_ref()
                        .cloned()
                        .unwrap_or_default();

                    let server = Http3Server::new(
                        udp_addr,
                        &http3_config,
                        &cert_path,
                        &key_path,
                        proxy_service_holder.clone(),
                    )
                    .await
                    .context("Failed to create HTTP/3 server")?;

                    Some(server)
                } else {
                    return Err(anyhow!("HTTP/3 requires TLS configuration"));
                }
            } else {
                None
            }
        };

        Ok(Self {
            http_server,
            http3_server,
            graceful_shutdown,
        })
    }

    pub async fn run(&self) -> Result<()> {
        let mut shutdown_receiver = self.graceful_shutdown.subscribe();

        match &self.http3_server {
            Some(h3_server) => {
                tracing::info!(
                    "Starting unified server with HTTP/1.1, HTTP/2 (TCP) and HTTP/3 (UDP) support"
                );

                // Run both HTTP and HTTP/3 servers concurrently
                tokio::select! {
                    result = self.http_server.run() => {
                        result.context("HTTP server error")?;
                    }
                    result = h3_server.run() => {
                        result.context("HTTP/3 server error")?;
                    }
                    shutdown_reason = shutdown_receiver.recv() => {
                        match shutdown_reason {
                            Ok(reason) => {
                                tracing::info!("Unified server shutdown initiated: {:?}", reason);
                                // Both servers will be dropped and cleaned up
                            }
                            Err(e) => {
                                tracing::error!("Error receiving shutdown signal: {}", e);
                            }
                        }
                    }
                }
            }
            None => {
                tracing::info!(
                    "Starting HTTP server with HTTP/1.1 and HTTP/2 support (HTTP/3 disabled)"
                );

                // Run only the HTTP server
                tokio::select! {
                    result = self.http_server.run() => {
                        result.context("HTTP server error")?;
                    }
                    shutdown_reason = shutdown_receiver.recv() => {
                        match shutdown_reason {
                            Ok(reason) => {
                                tracing::info!("HTTP server shutdown initiated: {:?}", reason);
                            }
                            Err(e) => {
                                tracing::error!("Error receiving shutdown signal: {}", e);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn http3_enabled(&self) -> bool {
        self.http3_server.is_some()
    }

    pub fn http3_local_addr(&self) -> Option<SocketAddr> {
        self.http3_server.as_ref().map(|s| s.local_addr())
    }
}
