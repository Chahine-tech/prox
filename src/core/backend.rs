use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use crate::config::HealthStatus;

// Constants for health status to replace magic numbers
const HEALTH_STATUS_UNHEALTHY: u8 = 0;
const HEALTH_STATUS_HEALTHY: u8 = 1;

#[derive(Debug)]
pub struct BackendHealth {
    status: AtomicU8, // Uses HEALTH_STATUS_* constants
    pub consecutive_successes: AtomicU32,
    pub consecutive_failures: AtomicU32,
}

impl BackendHealth {
    pub fn new(_target: String) -> Self {
        Self {
            status: AtomicU8::new(HEALTH_STATUS_HEALTHY), // Start as healthy
            consecutive_successes: AtomicU32::new(0),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    pub fn status(&self) -> HealthStatus {
        // Use Acquire ordering for better correctness when reading status
        if self.status.load(Ordering::Acquire) == HEALTH_STATUS_HEALTHY {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        }
    }

    pub fn mark_healthy(&self) {
        // Use Release ordering for updates to ensure visibility to other threads
        self.status.store(HEALTH_STATUS_HEALTHY, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Release);
    }

    pub fn mark_unhealthy(&self) {
        // Use Release ordering for updates to ensure visibility to other threads
        self.status.store(HEALTH_STATUS_UNHEALTHY, Ordering::Release);
        self.consecutive_successes.store(0, Ordering::Release);
    }
}