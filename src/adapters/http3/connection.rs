use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use quiche::{Connection, ConnectionId};
use tokio::sync::Mutex;

use crate::adapters::http3::QuicheConfig;
use crate::config::models::Http3Config;

pub struct QuicConnection {
    connection: Connection,
    h3_connection: Option<quiche::h3::Connection>,
}

impl QuicConnection {
    pub fn new(
        conn_id: &ConnectionId<'_>,
        odcid: Option<&ConnectionId<'_>>,
        local_addr: SocketAddr,
        peer_addr: SocketAddr,
        config: &mut quiche::Config,
    ) -> Result<Self> {
        let connection = quiche::accept(conn_id, odcid, local_addr, peer_addr, config)
            .context("Failed to accept QUIC connection")?;

        Ok(Self {
            connection,
            h3_connection: None,
        })
    }

    pub fn connection(&mut self) -> &mut Connection {
        &mut self.connection
    }

    pub fn h3_connection(&mut self) -> Option<&mut quiche::h3::Connection> {
        self.h3_connection.as_mut()
    }

    pub fn establish_h3(&mut self, h3_config: &quiche::h3::Config) -> Result<()> {
        if self.h3_connection.is_none() {
            let h3_conn = quiche::h3::Connection::with_transport(&mut self.connection, h3_config)
                .context("Failed to create HTTP/3 connection")?;
            self.h3_connection = Some(h3_conn);
        }
        Ok(())
    }

    pub fn poll_h3_events(&mut self) -> Result<Vec<(u64, quiche::h3::Event)>> {
        let mut events = Vec::new();

        if let Some(ref mut h3_conn) = self.h3_connection {
            loop {
                match h3_conn.poll(&mut self.connection) {
                    Ok((stream_id, event)) => events.push((stream_id, event)),
                    Err(quiche::h3::Error::Done) => break,
                    Err(e) => return Err(anyhow::anyhow!("HTTP/3 poll error: {}", e)),
                }
            }
        }

        Ok(events)
    }

    pub fn send_response(
        &mut self,
        stream_id: u64,
        headers: &[quiche::h3::Header],
        body: Option<Bytes>,
        fin: bool,
    ) -> Result<()> {
        if let Some(ref mut h3_conn) = self.h3_connection {
            // Send headers
            h3_conn
                .send_response(
                    &mut self.connection,
                    stream_id,
                    headers,
                    fin && body.is_none(),
                )
                .map_err(|e| anyhow::anyhow!("Failed to send response headers: {}", e))?;

            // Send body if present
            if let Some(body_data) = body {
                h3_conn
                    .send_body(&mut self.connection, stream_id, &body_data, fin)
                    .map_err(|e| anyhow::anyhow!("Failed to send response body: {}", e))?;
            }

            Ok(())
        } else {
            Err(anyhow::anyhow!("HTTP/3 connection not established"))
        }
    }
}

pub struct ConnectionManager {
    connections: Arc<Mutex<HashMap<Vec<u8>, QuicConnection>>>,
    http3_config: Http3Config,
    cert_path: String,
    key_path: String,
    h3_config: quiche::h3::Config,
}

impl ConnectionManager {
    pub fn new(http3_config: Http3Config, cert_path: &str, key_path: &str) -> Result<Self> {
        let h3_config = quiche::h3::Config::new().context("Failed to create HTTP/3 config")?;

        Ok(Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            http3_config,
            cert_path: cert_path.to_string(),
            key_path: key_path.to_string(),
            h3_config,
        })
    }

    fn create_quiche_config(&self) -> Result<QuicheConfig> {
        QuicheConfig::new(&self.http3_config, &self.cert_path, &self.key_path)
    }

    pub async fn get_or_create_connection(
        &self,
        conn_id: &ConnectionId<'_>,
        odcid: Option<&ConnectionId<'_>>,
        local_addr: SocketAddr,
        peer_addr: SocketAddr,
    ) -> Result<()> {
        let mut connections = self.connections.lock().await;
        let conn_id_vec = conn_id.to_vec();

        if let std::collections::hash_map::Entry::Vacant(e) = connections.entry(conn_id_vec) {
            // Create a new config for this connection
            let quiche_config = self.create_quiche_config()?;
            let mut config = quiche_config.into_inner();

            let mut quic_conn =
                QuicConnection::new(conn_id, odcid, local_addr, peer_addr, &mut config)?;

            // Establish HTTP/3 connection if QUIC handshake is complete
            if quic_conn.connection().is_established() {
                quic_conn.establish_h3(&self.h3_config)?;
            }

            e.insert(quic_conn);
        }

        Ok(())
    }

    pub async fn process_connection_events(
        &self,
        conn_id: &[u8],
    ) -> Result<Vec<(u64, quiche::h3::Event)>> {
        let mut connections = self.connections.lock().await;

        if let Some(quic_conn) = connections.get_mut(conn_id) {
            // Try to establish HTTP/3 connection if not already done
            if quic_conn.h3_connection().is_none() && quic_conn.connection().is_established() {
                quic_conn.establish_h3(&self.h3_config)?;
            }

            quic_conn.poll_h3_events()
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn send_response(
        &self,
        conn_id: &[u8],
        stream_id: u64,
        headers: &[quiche::h3::Header],
        body: Option<Bytes>,
        fin: bool,
    ) -> Result<()> {
        let mut connections = self.connections.lock().await;

        if let Some(quic_conn) = connections.get_mut(conn_id) {
            quic_conn.send_response(stream_id, headers, body, fin)
        } else {
            Err(anyhow::anyhow!("Connection not found"))
        }
    }
}
