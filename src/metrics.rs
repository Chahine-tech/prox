use metrics::{
    Unit, counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram,
};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

pub const PROX_BACKEND_HEALTH_STATUS: &str = "prox_backend_health_status";

// --- General Request Metrics ---
pub const PROX_REQUESTS_TOTAL: &str = "prox_requests_total";
pub const PROX_REQUEST_DURATION_SECONDS: &str = "prox_request_duration_seconds";

// --- Backend Specific Metrics ---
pub const PROX_BACKEND_REQUESTS_TOTAL: &str = "prox_backend_requests_total";
pub const PROX_BACKEND_REQUEST_DURATION_SECONDS: &str = "prox_backend_request_duration_seconds";

// Using a Lazy static HashMap to store gauges per backend.
// The key could be the backend URL or a unique identifier.
// The value is the health status (1 for healthy, 0 for unhealthy).
pub static BACKEND_HEALTH_GAUGES: Lazy<Mutex<HashMap<String, f64>>> = Lazy::new(|| {
    describe_gauge!(
        PROX_BACKEND_HEALTH_STATUS,
        "Health status of individual backends (1 for healthy, 0 for unhealthy)"
    );
    // Also describe other metrics
    describe_counter!(
        PROX_REQUESTS_TOTAL,
        Unit::Count,
        "Total number of HTTP requests processed by the proxy."
    );
    describe_histogram!(
        PROX_REQUEST_DURATION_SECONDS,
        Unit::Seconds,
        "Latency of HTTP requests processed by the proxy."
    );
    describe_counter!(
        PROX_BACKEND_REQUESTS_TOTAL,
        Unit::Count,
        "Total number of HTTP requests forwarded to backend services."
    );
    describe_histogram!(
        PROX_BACKEND_REQUEST_DURATION_SECONDS,
        Unit::Seconds,
        "Latency of HTTP requests forwarded to backend services."
    );
    Mutex::new(HashMap::new())
});

pub fn set_backend_health_status(backend_id: &str, is_healthy: bool) {
    let health_value = if is_healthy { 1.0 } else { 0.0 };
    // Update the shared map
    let mut gauges = BACKEND_HEALTH_GAUGES.lock().unwrap();
    gauges.insert(backend_id.to_string(), health_value);

    // Emit the metric using the metrics crate.
    let backend_label = backend_id.to_string(); // Create the string before the macro call
    gauge!(PROX_BACKEND_HEALTH_STATUS, "backend" => backend_label).set(health_value);
}

// --- Helper functions for new metrics ---

pub fn increment_request_total(path: &str, method: &str, status: u16) {
    counter!(
        PROX_REQUESTS_TOTAL,
        "path" => path.to_string(),
        "method" => method.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

pub fn record_request_duration(path: &str, method: &str, duration: std::time::Duration) {
    histogram!(
        PROX_REQUEST_DURATION_SECONDS,
        "path" => path.to_string(),
        "method" => method.to_string()
    )
    .record(duration.as_secs_f64());
}

pub fn increment_backend_request_total(backend: &str, path: &str, method: &str, status: u16) {
    counter!(
        PROX_BACKEND_REQUESTS_TOTAL,
        "backend" => backend.to_string(),
        "path" => path.to_string(),
        "method" => method.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

pub fn record_backend_request_duration(
    backend: &str,
    path: &str,
    method: &str,
    duration: std::time::Duration,
) {
    histogram!(
        PROX_BACKEND_REQUEST_DURATION_SECONDS,
        "backend" => backend.to_string(),
        "path" => path.to_string(),
        "method" => method.to_string()
    )
    .record(duration.as_secs_f64());
}

// Helper struct for measuring duration easily using RAII
pub struct RequestTimer {
    start: Instant,
    path: String,
    method: String,
}

impl RequestTimer {
    pub fn new(path: String, method: String) -> Self {
        Self {
            start: Instant::now(),
            path,
            method,
        }
    }
}

impl Drop for RequestTimer {
    fn drop(&mut self) {
        record_request_duration(&self.path, &self.method, self.start.elapsed());
    }
}

pub struct BackendRequestTimer {
    start: Instant,
    backend: String,
    path: String,
    method: String,
}

impl BackendRequestTimer {
    pub fn new(backend: String, path: String, method: String) -> Self {
        Self {
            start: Instant::now(),
            backend,
            path,
            method,
        }
    }
}

impl Drop for BackendRequestTimer {
    fn drop(&mut self) {
        record_backend_request_duration(
            &self.backend,
            &self.path,
            &self.method,
            self.start.elapsed(),
        );
    }
}
