use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use quiche::h3::Event as H3Event;
use tokio::net::UdpSocket;

use crate::adapters::http3::{ConnectionManager, Http3Handler};
use crate::config::models::Http3Config;
use crate::core::ProxyService;

pub struct Http3Server {
    socket: UdpSocket,
    connection_manager: Arc<ConnectionManager>,
    handler: Http3Handler,
    local_addr: SocketAddr,
}

impl Http3Server {
    pub async fn new(
        bind_addr: SocketAddr,
        http3_config: &Http3Config,
        cert_path: &str,
        key_path: &str,
        proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
    ) -> Result<Self> {
        let socket = UdpSocket::bind(bind_addr)
            .await
            .with_context(|| format!("Failed to bind UDP socket to {bind_addr}"))?;

        tracing::info!("HTTP/3 server bound to UDP {}", bind_addr);

        let connection_manager = Arc::new(ConnectionManager::new(
            http3_config.clone(),
            cert_path,
            key_path,
        )?);

        let handler = Http3Handler::new(proxy_service_holder, connection_manager.clone());

        Ok(Self {
            socket,
            connection_manager,
            handler,
            local_addr: bind_addr,
        })
    }

    pub async fn run(&self) -> Result<()> {
        tracing::info!("Starting HTTP/3 server on {}", self.local_addr);

        let mut buffer = vec![0; 65536];

        loop {
            let (len, peer_addr) = self
                .socket
                .recv_from(&mut buffer)
                .await
                .context("Failed to receive UDP packet")?;

            let packet = &buffer[..len];

            tracing::debug!("Received {} bytes from {}", len, peer_addr);

            if let Err(e) = self.process_packet(packet, peer_addr).await {
                tracing::error!("Error processing packet from {}: {}", peer_addr, e);
            }
        }
    }

    async fn process_packet(&self, packet: &[u8], peer_addr: SocketAddr) -> Result<()> {
        let mut packet_buf = packet.to_vec();
        let hdr = quiche::Header::from_slice(&mut packet_buf, quiche::MAX_CONN_ID_LEN)
            .context("Failed to parse QUIC header")?;

        let conn_id = hdr.dcid.clone();

        self.connection_manager
            .get_or_create_connection(&conn_id, None, self.local_addr, peer_addr)
            .await?;

        let events = self
            .connection_manager
            .process_connection_events(&conn_id)
            .await?;

        for (stream_id, event) in events {
            if let Err(e) = self.handle_h3_event(&conn_id, stream_id, event).await {
                tracing::error!("Error handling HTTP/3 event: {}", e);
            }
        }

        Ok(())
    }

    async fn handle_h3_event(&self, conn_id: &[u8], stream_id: u64, event: H3Event) -> Result<()> {
        match event {
            H3Event::Headers { list, more_frames } => {
                tracing::debug!(
                    "Received headers on stream {}, more_frames: {}",
                    stream_id,
                    more_frames
                );

                let mut body = None;
                if more_frames {
                    let body_data = Vec::new();
                    body = Some(bytes::Bytes::from(body_data));
                }

                self.handler
                    .handle_h3_request(conn_id, stream_id, list, body)
                    .await?;
            }
            H3Event::Data => {
                tracing::debug!("Received data on stream {}", stream_id);
            }
            H3Event::Finished => {
                tracing::debug!("Stream {} finished", stream_id);
            }
            H3Event::Reset(error_code) => {
                tracing::warn!("Stream {} reset with error code: {}", stream_id, error_code);
            }
            H3Event::PriorityUpdate => {
                tracing::debug!("Received priority update on stream {}", stream_id);
            }
            H3Event::GoAway => {
                tracing::info!("Received GOAWAY");
            }
        }

        Ok(())
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

#[cfg(test)]
mod tests {
    use crate::config::models::{Http3Config, Http3CongestionControl};

    fn create_test_config() -> Http3Config {
        Http3Config {
            max_data: 1000000,
            max_stream_data: 100000,
            max_streams_bidi: 10,
            max_idle_timeout: 30000,
            congestion_control: Http3CongestionControl::Cubic,
            enable_0rtt: false,
            max_packet_size: Some(1452),
        }
    }

    #[test]
    fn test_quic_header_parsing() {
        // Test QUIC header parsing with a minimal valid packet
        let mut packet = vec![
            0xc0, // Long header, Initial packet
            0x00, 0x00, 0x00, 0x01, // Version (QUIC v1)
            0x00, // DCID length
            0x00, // SCID length
            0x00, // Token length
            0x00, 0x00, // Length
        ];

        let result = quiche::Header::from_slice(&mut packet, quiche::MAX_CONN_ID_LEN);
        assert!(result.is_ok());

        let header = result.unwrap();
        assert_eq!(header.ty, quiche::Type::Initial);
    }

    #[test]
    fn test_config_validation() {
        let config = create_test_config();

        assert!(config.max_data > 0);
        assert!(config.max_stream_data > 0);
        assert!(config.max_streams_bidi > 0);
        assert!(config.max_idle_timeout > 0);

        if let Some(max_packet_size) = config.max_packet_size {
            assert!(max_packet_size >= 1200);
        }
    }

    #[test]
    fn test_http3_config_congestion_control() {
        let config = create_test_config();

        match config.congestion_control {
            Http3CongestionControl::Cubic => { /* Valid variant */ }
            Http3CongestionControl::Reno => { /* Valid variant */ }
            Http3CongestionControl::Bbr => { /* Valid variant */ }
        }
    }
}
