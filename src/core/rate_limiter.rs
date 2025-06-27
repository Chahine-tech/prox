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
    pub fn check_route(&self) -> Result<(), Box<AxumResponse>> {
        if self.limiter.check().is_err() {
            let response = (self.status_code, self.message.clone()).into_response();
            Err(Box::new(response))
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
    fn check_keyed(&self, key: &K) -> Result<(), Box<AxumResponse>> {
        if self.limiter.check_key(key).is_err() {
            let response = (self.status_code, self.message.clone()).into_response();
            Err(Box::new(response))
        } else {
            Ok(())
        }
    }
}

// Specific check method for IP-based limiters
impl IpLimiter {
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), Box<AxumResponse>> {
        self.check_keyed(&ip) // Delegates to the generic keyed check
    }
}

// Specific check method for header-based limiters
impl HeaderLimiter {
    pub fn check_header_value(&self, value: &str) -> Result<(), Box<AxumResponse>> {
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

        let quota_requests = NonZeroU32::new(config.requests as u32)
            .ok_or_else(|| "Rate limit 'requests' must be greater than 0".to_string())?;

        // Configure Quota based on the algorithm.
        // For TokenBucket and SlidingWindow (using GCRA), we allow bursts up to the number of requests.
        // For FixedWindow, burst is typically 1 to strictly enforce the window, or could be `quota_requests`
        // if we want to allow all requests at the beginning of the window.
        // Governor's core algorithm is GCRA, which behaves like a token bucket or leaky bucket.
        // We'll map our enum variants to Quota configurations.
        let quota = match config.algorithm {
            RateLimitAlgorithm::TokenBucket => {
                // TokenBucket allows bursts up to the number of requests over the specified period.
                // Uses governor's GCRA, which behaves like a token bucket.
                Quota::with_period(period_duration)
                    .ok_or_else(|| {
                        format!("Invalid period duration for TokenBucket: {period_duration:?}")
                    })?
                    .allow_burst(quota_requests)
            }
            RateLimitAlgorithm::SlidingWindow => {
                // SlidingWindow, using governor's GCRA, allows a number of requests within any
                // sliding time window of the specified period. GCRA is inherently a sliding window algorithm.
                // This configuration allows bursts up to the number of requests.
                Quota::with_period(period_duration)
                    .ok_or_else(|| {
                        format!("Invalid period duration for SlidingWindow: {period_duration:?}")
                    })?
                    .allow_burst(quota_requests)
            }
            RateLimitAlgorithm::FixedWindow => {
                // FixedWindow, as implemented with governor, allows `requests` per `period_duration`.
                // This specific configuration allows all `requests` to be consumed at the start of any
                // period (i.e., burst capacity equals the total requests for the window).
                // This is a common interpretation of "N requests per fixed period P".
                //
                // For a "stricter" fixed window (e.g., smoothed rate without large bursts, or
                // a counter that resets sharply at window boundaries), a different Quota setup
                // (like a rate-based quota with a small burst) or a different rate-limiting
                // library/mechanism might be necessary, as governor's core is GCRA.
                Quota::with_period(period_duration)
                    .ok_or_else(|| {
                        format!("Invalid period duration for FixedWindow: {period_duration:?}")
                    })?
                    .allow_burst(quota_requests)
            }
        };

        let status_code = StatusCode::from_u16(config.status_code)
            .map_err(|_| format!("Invalid status code: {}", config.status_code))?;

        tracing::info!(
            "Creating rate limiter: by={:?}, algorithm={:?}, requests={}, period={}, status_code={}, on_missing_key={:?}",
            config.by,
            config.algorithm,
            config.requests,
            config.period,
            config.status_code,
            config.on_missing_key
        );

        match config.by {
            RateLimitBy::Route => {
                let limiter = Arc::new(LimiterWrapper {
                    limiter: RateLimiter::direct(quota),
                    status_code,
                    message: config.message.clone(),
                    on_missing_key: config.on_missing_key,
                });
                Ok(RouteRateLimiter::Route(limiter))
            }
            RateLimitBy::Ip => {
                let limiter = Arc::new(LimiterWrapper {
                    limiter: RateLimiter::keyed(quota),
                    status_code,
                    message: config.message.clone(),
                    on_missing_key: config.on_missing_key,
                });
                Ok(RouteRateLimiter::Ip(limiter))
            }
            RateLimitBy::Header => {
                let header_name_str = config
                    .header_name
                    .as_ref()
                    .ok_or_else(|| "header_name is required for RateLimitBy::Header".to_string())?;
                let header_name = HeaderName::from_bytes(header_name_str.as_bytes())
                    .map_err(|e| format!("Invalid header_name '{header_name_str}': {e}"))?;
                let limiter = Arc::new(LimiterWrapper {
                    limiter: RateLimiter::keyed(quota),
                    status_code,
                    message: config.message.clone(),
                    on_missing_key: config.on_missing_key,
                });
                Ok(RouteRateLimiter::Header {
                    limiter,
                    header_name,
                })
            }
        }
    }

    /// Checks if a request is allowed based on the configured rate limiting rules.
    /// Returns `Ok(())` if allowed, or `Err(AxumResponse)` if rate-limited.
    pub fn check<B>(
        &self,
        req: &Request<B>,
        connect_info: Option<&ConnectInfo<SocketAddr>>,
    ) -> Result<(), Box<AxumResponse>> {
        match self {
            RouteRateLimiter::Route(limiter) => {
                tracing::trace!("Checking route-specific rate limit");
                limiter.check_route().inspect_err(|_e| {
                    tracing::warn!("Route rate limit exceeded");
                })
            }
            RouteRateLimiter::Ip(limiter) => {
                if let Some(ConnectInfo(addr)) = connect_info {
                    let ip = addr.ip();
                    tracing::trace!("Checking IP-based rate limit for IP: {}", ip);
                    limiter.check_ip(ip).inspect_err(|_e| {
                        tracing::warn!("IP rate limit exceeded for {}: {}", ip, limiter.message);
                    })
                } else {
                    tracing::warn!("IP rate limiting configured, but ConnectInfo not available.");
                    // Handle missing IP based on policy
                    match limiter.on_missing_key {
                        MissingKeyPolicy::Allow => Ok(()),
                        MissingKeyPolicy::Deny => {
                            tracing::warn!(
                                "Denying request due to missing IP for IP-based rate limiting and Deny policy."
                            );
                            Err(Box::new(
                                (limiter.status_code, limiter.message.clone()).into_response(),
                            ))
                        }
                    }
                }
            }
            RouteRateLimiter::Header {
                limiter,
                header_name,
            } => {
                tracing::trace!(
                    "Checking header-based rate limit for header: {}",
                    header_name
                );
                if let Some(value) = req.headers().get(header_name) {
                    if let Ok(value_str) = value.to_str() {
                        limiter.check_header_value(value_str).inspect_err(|_e| {
                            tracing::warn!(
                                "Header rate limit exceeded for header \'{}\', value \'{}\': {}",
                                header_name,
                                value_str,
                                limiter.message
                            );
                        })
                    } else {
                        tracing::warn!(
                            "Header \'{}\' value is not valid UTF-8. Applying on_missing_key policy.",
                            header_name
                        );
                        // Handle non-UTF-8 header value based on policy
                        match limiter.on_missing_key {
                            MissingKeyPolicy::Allow => Ok(()),
                            MissingKeyPolicy::Deny => {
                                tracing::warn!(
                                    "Denying request due to non-UTF-8 header value for {} and Deny policy.",
                                    header_name
                                );
                                Err(Box::new(
                                    (limiter.status_code, limiter.message.clone()).into_response(),
                                ))
                            }
                        }
                    }
                } else {
                    tracing::debug!(
                        "Header \'{}\' not found for rate limiting. Applying on_missing_key policy: {:?}",
                        header_name,
                        limiter.on_missing_key
                    );
                    // Handle missing header based on policy
                    match limiter.on_missing_key {
                        MissingKeyPolicy::Allow => Ok(()),
                        MissingKeyPolicy::Deny => {
                            tracing::warn!(
                                "Denying request due to missing header {} and Deny policy.",
                                header_name
                            );
                            Err(Box::new(
                                (limiter.status_code, limiter.message.clone()).into_response(),
                            ))
                        }
                    }
                }
            }
        }
    }
}
