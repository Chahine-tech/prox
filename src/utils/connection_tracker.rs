use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::broadcast;
use tokio::time::sleep;

/// Unique identifier for a connection
pub type ConnectionId = u64;

/// Information about an active connection
#[derive(Debug)]
pub struct ConnectionInfo {
    pub id: ConnectionId,
    pub remote_addr: SocketAddr,
    pub established_at: Instant,
    pub active_requests: AtomicU64,
}

impl Clone for ConnectionInfo {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            remote_addr: self.remote_addr,
            established_at: self.established_at,
            active_requests: AtomicU64::new(self.active_requests.load(Ordering::Relaxed)),
        }
    }
}

impl ConnectionInfo {
    pub fn new(id: ConnectionId, remote_addr: SocketAddr) -> Self {
        Self {
            id,
            remote_addr,
            established_at: Instant::now(),
            active_requests: AtomicU64::new(0),
        }
    }

    pub fn increment_requests(&self) {
        self.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement_requests(&self) {
        self.active_requests.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn active_request_count(&self) -> u64 {
        self.active_requests.load(Ordering::Relaxed)
    }

    pub fn is_idle(&self) -> bool {
        self.active_request_count() == 0
    }

    pub fn age(&self) -> Duration {
        self.established_at.elapsed()
    }
}

/// Manages active connections and provides graceful draining capabilities
#[derive(Clone)]
pub struct ConnectionTracker {
    connections: Arc<DashMap<ConnectionId, Arc<ConnectionInfo>>>,
    next_id: Arc<AtomicU64>,
    shutdown_tx: broadcast::Sender<()>,
}

impl ConnectionTracker {
    pub fn new() -> Self {
        let (shutdown_tx, _) = broadcast::channel(16);
        Self {
            connections: Arc::new(DashMap::new()),
            next_id: Arc::new(AtomicU64::new(1)),
            shutdown_tx,
        }
    }

    /// Register a new connection and return its info
    pub fn register_connection(&self, remote_addr: SocketAddr) -> Arc<ConnectionInfo> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let info = Arc::new(ConnectionInfo::new(id, remote_addr));

        self.connections.insert(id, info.clone());

        tracing::debug!(
            "Connection registered: id={}, remote_addr={}, total_connections={}",
            id,
            remote_addr,
            self.connections.len()
        );

        info
    }

    /// Unregister a connection
    pub fn unregister_connection(&self, connection_id: ConnectionId) {
        if let Some((_, info)) = self.connections.remove(&connection_id) {
            tracing::debug!(
                "Connection unregistered: id={}, remote_addr={}, duration={:?}, total_connections={}",
                info.id,
                info.remote_addr,
                info.age(),
                self.connections.len()
            );
        }
    }

    /// Get connection info by ID
    pub fn get_connection(&self, connection_id: ConnectionId) -> Option<Arc<ConnectionInfo>> {
        self.connections
            .get(&connection_id)
            .map(|entry| entry.clone())
    }

    /// Get total number of active connections
    pub fn active_connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Get total number of active requests across all connections
    pub fn total_active_requests(&self) -> u64 {
        self.connections
            .iter()
            .map(|entry| entry.value().active_request_count())
            .sum()
    }

    /// Get connections that are currently idle (no active requests)
    pub fn idle_connections(&self) -> Vec<Arc<ConnectionInfo>> {
        self.connections
            .iter()
            .filter_map(|entry| {
                let info = entry.value();
                if info.is_idle() {
                    Some(info.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get connections that have active requests
    pub fn busy_connections(&self) -> Vec<Arc<ConnectionInfo>> {
        self.connections
            .iter()
            .filter_map(|entry| {
                let info = entry.value();
                if !info.is_idle() {
                    Some(info.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Signal all connections to stop accepting new requests (for graceful shutdown)
    pub fn initiate_shutdown(&self) {
        tracing::info!(
            "Initiating connection shutdown signal for {} active connections",
            self.connections.len()
        );
        let _ = self.shutdown_tx.send(());
    }

    /// Get a receiver for shutdown signals
    pub fn shutdown_signal(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Wait for all connections to become idle or timeout
    pub async fn drain_connections(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        let mut log_interval = tokio::time::interval(Duration::from_secs(5));

        tracing::info!(
            "Starting connection drain: {} active connections, {} total active requests, timeout={:?}",
            self.active_connection_count(),
            self.total_active_requests(),
            timeout
        );

        while start.elapsed() < timeout {
            let active_requests = self.total_active_requests();
            let connection_count = self.active_connection_count();

            if active_requests == 0 {
                tracing::info!(
                    "All connections drained successfully in {:?} ({} connections remain idle)",
                    start.elapsed(),
                    connection_count
                );
                return true;
            }

            // Log progress periodically
            tokio::select! {
                _ = log_interval.tick() => {
                    let busy_conns = self.busy_connections();
                    tracing::info!(
                        "Connection drain in progress: {} active requests across {} busy connections (elapsed: {:?})",
                        active_requests,
                        busy_conns.len(),
                        start.elapsed()
                    );

                    // Log detailed info for long-running requests in debug mode
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        for conn in busy_conns.iter().take(5) { // Limit to first 5 to avoid spam
                            tracing::debug!(
                                "Busy connection: id={}, remote_addr={}, active_requests={}, age={:?}",
                                conn.id,
                                conn.remote_addr,
                                conn.active_request_count(),
                                conn.age()
                            );
                        }
                    }
                }
                _ = sleep(Duration::from_millis(100)) => {
                    // Continue checking
                }
            }
        }

        let remaining_requests = self.total_active_requests();
        let remaining_connections = self.active_connection_count();

        if remaining_requests > 0 {
            tracing::warn!(
                "Connection drain timeout exceeded: {} requests still active across {} connections after {:?}",
                remaining_requests,
                remaining_connections,
                timeout
            );
            false
        } else {
            tracing::info!("All connections drained at timeout boundary");
            true
        }
    }

    /// Get statistics about current connections
    pub fn get_stats(&self) -> ConnectionStats {
        let connections: Vec<_> = self
            .connections
            .iter()
            .map(|entry| entry.value().clone())
            .collect();

        let total_connections = connections.len();
        let total_requests = connections.iter().map(|c| c.active_request_count()).sum();
        let idle_connections = connections.iter().filter(|c| c.is_idle()).count();
        let busy_connections = total_connections - idle_connections;

        let oldest_connection = connections.iter().max_by_key(|c| c.age()).map(|c| c.age());

        ConnectionStats {
            total_connections,
            idle_connections,
            busy_connections,
            total_active_requests: total_requests,
            oldest_connection_age: oldest_connection,
        }
    }
}

impl Default for ConnectionTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about current connections
#[derive(Debug, Clone)]
pub struct ConnectionStats {
    pub total_connections: usize,
    pub idle_connections: usize,
    pub busy_connections: usize,
    pub total_active_requests: u64,
    pub oldest_connection_age: Option<Duration>,
}

/// RAII guard for tracking connection lifecycle
pub struct ConnectionGuard {
    connection_info: Arc<ConnectionInfo>,
    tracker: ConnectionTracker,
}

impl ConnectionGuard {
    pub fn new(tracker: ConnectionTracker, remote_addr: SocketAddr) -> Self {
        let connection_info = tracker.register_connection(remote_addr);
        Self {
            connection_info,
            tracker,
        }
    }

    pub fn connection_id(&self) -> ConnectionId {
        self.connection_info.id
    }

    pub fn connection_info(&self) -> &Arc<ConnectionInfo> {
        &self.connection_info
    }

    /// Create a request guard for this connection
    pub fn request_guard(&self) -> RequestGuard {
        RequestGuard::new(self.connection_info.clone())
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.tracker.unregister_connection(self.connection_info.id);
    }
}

/// RAII guard for tracking individual request lifecycle within a connection
pub struct RequestGuard {
    connection_info: Arc<ConnectionInfo>,
}

impl RequestGuard {
    fn new(connection_info: Arc<ConnectionInfo>) -> Self {
        connection_info.increment_requests();
        Self { connection_info }
    }

    pub fn connection_id(&self) -> ConnectionId {
        self.connection_info.id
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.connection_info.decrement_requests();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080)
    }

    #[tokio::test]
    async fn test_connection_registration() {
        let tracker = ConnectionTracker::new();
        let addr = test_addr();

        assert_eq!(tracker.active_connection_count(), 0);

        let conn_info = tracker.register_connection(addr);
        assert_eq!(tracker.active_connection_count(), 1);
        assert_eq!(conn_info.remote_addr, addr);
        assert!(conn_info.is_idle());

        tracker.unregister_connection(conn_info.id);
        assert_eq!(tracker.active_connection_count(), 0);
    }

    #[tokio::test]
    async fn test_request_tracking() {
        let tracker = ConnectionTracker::new();
        let addr = test_addr();

        let conn_info = tracker.register_connection(addr);
        assert_eq!(tracker.total_active_requests(), 0);
        assert!(conn_info.is_idle());

        // Simulate request start
        conn_info.increment_requests();
        assert_eq!(tracker.total_active_requests(), 1);
        assert!(!conn_info.is_idle());

        // Simulate request end
        conn_info.decrement_requests();
        assert_eq!(tracker.total_active_requests(), 0);
        assert!(conn_info.is_idle());
    }

    #[tokio::test]
    async fn test_connection_guard() {
        let tracker = ConnectionTracker::new();
        let addr = test_addr();

        assert_eq!(tracker.active_connection_count(), 0);

        {
            let _guard = ConnectionGuard::new(tracker.clone(), addr);
            assert_eq!(tracker.active_connection_count(), 1);

            {
                let _req_guard = _guard.request_guard();
                assert_eq!(tracker.total_active_requests(), 1);
            }
            assert_eq!(tracker.total_active_requests(), 0);
        }

        assert_eq!(tracker.active_connection_count(), 0);
    }

    #[tokio::test]
    async fn test_drain_connections() {
        let tracker = ConnectionTracker::new();
        let addr = test_addr();

        let conn_info = tracker.register_connection(addr);

        // Test immediate drain when no active requests
        let drained = tracker.drain_connections(Duration::from_millis(100)).await;
        assert!(drained);

        // Test drain with active request
        conn_info.increment_requests();

        let start = Instant::now();
        let drained = tracker.drain_connections(Duration::from_millis(100)).await;
        let elapsed = start.elapsed();

        assert!(!drained); // Should timeout with active request
        assert!(elapsed >= Duration::from_millis(100));

        conn_info.decrement_requests();
    }
}
