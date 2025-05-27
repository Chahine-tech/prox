// Standard library imports
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::response::{IntoResponse, Response as AxumResponse};
use http::{HeaderName, Request, StatusCode};
use humantime;
use tracing;

use governor::clock::DefaultClock;
use governor::state::keyed::DashMapStateStore;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};

use crate::config::models::{MissingKeyPolicy, RateLimitAlgorithm, RateLimitBy, RateLimitConfig};

// --- LimiterWrapper Definition ---
// LimiterWrapper holds a RateLimiter instance and the response details for when the limit is exceeded.
// RL is the specific type of governor::RateLimiter.
#[derive(Clone)]
pub struct LimiterWrapper<RL> {
    pub limiter: RL,
    pub status_code: StatusCode,
    pub message: String,
    pub on_missing_key: MissingKeyPolicy, // Added field
}

// --- Type Aliases for specific RateLimiter configurations ---
pub type DirectRateLimiterImpl = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;
pub type KeyedRateLimiterImpl<K> = RateLimiter<K, DashMapStateStore<K>, DefaultClock>;

// --- Type Aliases for specific LimiterWrappers ---
// These wrap the RateLimiter implementations with custom error responses.
pub type RouteSpecificLimiter = LimiterWrapper<DirectRateLimiterImpl>;
pub type IpLimiter = LimiterWrapper<KeyedRateLimiterImpl<IpAddr>>;
pub type HeaderLimiter = LimiterWrapper<KeyedRateLimiterImpl<String>>;

// --- LimiterWrapper Implementations ---

// Implementation for non-keyed (direct) limiters
impl LimiterWrapper<DirectRateLimiterImpl> {
    pub fn check_route(&self) -> Result<(), AxumResponse> {
        if self.limiter.check().is_err() {
            let response = (self.status_code, self.message.clone()).into_response();
            Err(response)
        } else {
            Ok(())
        }
    }
}

// Generic implementation for keyed limiters
impl<K> LimiterWrapper<KeyedRateLimiterImpl<K>>
where
    K: Clone + Hash + Eq + Send + Sync + 'static, // Key constraints for DashMapStateStore
{
    // Generic check method for keyed limiters
    fn check_keyed(&self, key: &K) -> Result<(), AxumResponse> {
        if self.limiter.check_key(key).is_err() {
            let response = (self.status_code, self.message.clone()).into_response();
            Err(response)
        } else {
            Ok(())
        }
    }
}

// Specific check method for IP-based limiters
impl IpLimiter {
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), AxumResponse> {
        self.check_keyed(&ip) // Delegates to the generic keyed check
    }
}

// Specific check method for header-based limiters
impl HeaderLimiter {
    pub fn check_header_value(&self, value: &str) -> Result<(), AxumResponse> {
        // The key for DashMapStateStore<String> is String, so convert &str to String
        self.check_keyed(&value.to_string())
    }
}

// --- RouteRateLimiter Enum ---
// This enum dispatches to the correct type of limiter based on configuration.
// It holds an Arc to the LimiterWrapper, allowing shared state for the same route.
#[derive(Clone)]
pub enum RouteRateLimiter {
    Route(Arc<RouteSpecificLimiter>),
    Ip(Arc<IpLimiter>),
    Header {
        limiter: Arc<HeaderLimiter>,
        header_name: HeaderName, // Store HeaderName for extraction in check method
    },
}

impl RouteRateLimiter {
    /// Creates a new `RouteRateLimiter` based on the provided `RateLimitConfig`.
    pub fn new(config: &RateLimitConfig) -> Result<Self, String> {
        let period_duration = humantime::parse_duration(&config.period)
            .map_err(|e| format!("Invalid period string '{}': {}", config.period, e))?;

        let quota = Quota::with_period(period_duration)
            .ok_or_else(|| format!("Invalid period duration for quota: {:?}", period_duration))?
            .allow_burst(
                NonZeroU32::new(config.requests as u32)
                    .unwrap_or_else(|| NonZeroU32::new(1).unwrap()), // Ensure burst is at least 1
            );

        let status_code =
            StatusCode::from_u16(config.status_code).unwrap_or(StatusCode::TOO_MANY_REQUESTS); // Default if u16 is invalid

        // Algorithm handling: Currently, only TokenBucket (default governor behavior) is supported.
        // Future algorithms might require different Quota setups or even different limiter types.
        match config.algorithm {
            Some(RateLimitAlgorithm::TokenBucket) | None => {
                // TokenBucket is achieved via Quota::allow_burst, which is already configured.
            } // Add other algorithm arms here if/when supported
              // e.g., Some(RateLimitAlgorithm::SlidingWindow) => return Err("SlidingWindow not yet implemented".to_string()),
        }

        match config.by {
            RateLimitBy::Route => {
                // For non-keyed limiters, RateLimiter::direct uses InMemoryState by default.
                let limiter = DirectRateLimiterImpl::direct(quota);
                Ok(RouteRateLimiter::Route(Arc::new(RouteSpecificLimiter {
                    limiter,
                    status_code,
                    message: config.message.clone(),
                    on_missing_key: config.on_missing_key, // Pass policy
                })))
            }
            RateLimitBy::Ip => {
                // For keyed limiters with DashMapStateStore.
                let store = DashMapStateStore::<IpAddr>::default();
                let clock = DefaultClock::default(); // Create clock instance
                let limiter = KeyedRateLimiterImpl::<IpAddr>::new(quota, store, clock); // Pass clock by value
                Ok(RouteRateLimiter::Ip(Arc::new(IpLimiter {
                    limiter,
                    status_code,
                    message: config.message.clone(),
                    on_missing_key: config.on_missing_key, // Pass policy
                })))
            }
            RateLimitBy::Header => {
                let header_name_str = config.header_name.as_deref().ok_or_else(|| {
                    "`header_name` must be specified for header-based rate limiting".to_string()
                })?;
                let header_name = HeaderName::from_bytes(header_name_str.as_bytes())
                    .map_err(|e| format!("Invalid header name '{}': {}", header_name_str, e))?;

                let store = DashMapStateStore::<String>::default();
                let clock = DefaultClock::default(); // Create clock instance
                let limiter = KeyedRateLimiterImpl::<String>::new(quota, store, clock); // Pass clock by value
                Ok(RouteRateLimiter::Header {
                    limiter: Arc::new(HeaderLimiter {
                        limiter,
                        status_code,
                        message: config.message.clone(),
                        on_missing_key: config.on_missing_key, // Pass policy
                    }),
                    header_name,
                })
            }
        }
    }

    /// Checks if a request is allowed based on the configured rate limiting rules.
    /// Returns `Ok(())` if allowed, or `Err(AxumResponse)` if rate-limited.
    pub fn check<B>(
        // B is the request body type, typically axum::body::Body
        &self,
        req: &Request<B>,
        connect_info: Option<&ConnectInfo<SocketAddr>>, // Passed from the handler
    ) -> Result<(), AxumResponse> {
        match self {
            RouteRateLimiter::Route(limiter) => limiter.check_route(),
            RouteRateLimiter::Ip(limiter) => {
                if let Some(ConnectInfo(addr)) = connect_info {
                    limiter.check_ip(addr.ip())
                } else {
                    match limiter.on_missing_key {
                        MissingKeyPolicy::Allow => {
                            tracing::warn!(
                                "Could not determine client IP for IP-based rate limiting. Allowing request due to policy."
                            );
                            Ok(())
                        }
                        MissingKeyPolicy::Deny => {
                            tracing::warn!(
                                "Could not determine client IP for IP-based rate limiting. Denying request due to policy."
                            );
                            Err((
                                StatusCode::BAD_REQUEST,
                                "Cannot determine rate limiting key",
                            )
                                .into_response())
                        }
                    }
                }
            }
            RouteRateLimiter::Header {
                limiter,
                header_name,
            } => {
                if let Some(value) = req.headers().get(header_name).and_then(|v| v.to_str().ok()) {
                    limiter.check_header_value(value)
                } else {
                    match limiter.on_missing_key {
                        MissingKeyPolicy::Allow => {
                            tracing::debug!(
                                "Header '{}' not found for rate limiting. Allowing request due to policy.",
                                header_name
                            );
                            Ok(())
                        }
                        MissingKeyPolicy::Deny => {
                            tracing::debug!(
                                "Header '{}' not found for rate limiting. Denying request due to policy.",
                                header_name
                            );
                            Err((
                                StatusCode::BAD_REQUEST,
                                "Required header for rate limiting not found",
                            )
                                .into_response())
                        }
                    }
                }
            }
        }
    }
}
