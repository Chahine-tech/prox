use regex::Regex;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;
use url::Url;

use crate::config::models::{AcmeConfig, RateLimitConfig, RouteConfig, ServerConfig, TlsConfig};

#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("Configuration validation failed: {message}")]
    ValidationFailed { message: String },

    #[error("Invalid field '{field}': {message}")]
    InvalidField { field: String, message: String },

    #[error("Missing required field: {field}")]
    MissingField { field: String },

    #[error("Invalid URL in field '{field}': {url} - {reason}")]
    InvalidUrl {
        field: String,
        url: String,
        reason: String,
    },

    #[error("Invalid listen address: {address} - {reason}")]
    InvalidListenAddress { address: String, reason: String },

    #[error("Invalid rate limit configuration for route '{route}': {message}")]
    InvalidRateLimit { route: String, message: String },

    #[error("Invalid TLS configuration: {message}")]
    InvalidTls { message: String },

    #[error("Invalid ACME configuration: {message}")]
    InvalidAcme { message: String },

    #[error("Route configuration conflict: {message}")]
    RouteConflict { message: String },

    #[error("File not found: {path}")]
    FileNotFound { path: String },
}

pub type ValidationResult<T> = Result<T, ValidationError>;

/// Configuration validator with detailed error reporting
pub struct ConfigValidator;

impl ConfigValidator {
    /// Validate a complete server configuration
    pub fn validate(config: &ServerConfig) -> ValidationResult<()> {
        let mut errors = Vec::new();

        // Validate listen address
        if let Err(e) = Self::validate_listen_address(&config.listen_addr) {
            errors.push(e);
        }

        // Validate routes
        if config.routes.is_empty() {
            errors.push(ValidationError::MissingField {
                field: "routes".to_string(),
            });
        } else {
            for (path, route_config) in &config.routes {
                if let Err(mut route_errors) = Self::validate_single_route(path, route_config) {
                    errors.append(&mut route_errors);
                }
            }
        }

        // Validate TLS configuration if present
        if let Some(tls_config) = &config.tls {
            if let Err(e) = Self::validate_tls_config(tls_config) {
                errors.push(e);
            }
        }

        // Check for route conflicts
        if let Err(conflict_error_list) = Self::check_route_conflicts(&config.routes) {
            errors.extend(conflict_error_list);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationError::ValidationFailed {
                message: Self::format_multiple_errors(errors),
            })
        }
    }

    /// Validate listen address format
    fn validate_listen_address(address: &str) -> ValidationResult<()> {
        if address.parse::<SocketAddr>().is_err() {
            return Err(ValidationError::InvalidListenAddress {
                address: address.to_string(),
                reason: "Must be in format 'IP:PORT' (e.g., '127.0.0.1:3000' or '0.0.0.0:8080')"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Validate all route configurations
    /// Validate a single route configuration
    fn validate_single_route(path: &str, config: &RouteConfig) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        // Validate path format
        if !path.starts_with('/') {
            errors.push(ValidationError::InvalidField {
                field: format!("route path: {path}"),
                message: "Route paths must start with '/'".to_string(),
            });
        }

        match config {
            RouteConfig::Proxy { target, .. } => {
                if let Err(e) = Self::validate_url(target, &format!("route '{path}' proxy target"))
                {
                    errors.push(e);
                }
            }
            RouteConfig::LoadBalance { targets, .. } => {
                if targets.is_empty() {
                    errors.push(ValidationError::InvalidField {
                        field: format!("route '{path}' load balance targets"),
                        message: "Load balance routes must have at least one target".to_string(),
                    });
                } else {
                    for (i, target) in targets.iter().enumerate() {
                        if let Err(e) = Self::validate_url(
                            target,
                            &format!("route '{path}' load balance target {i}"),
                        ) {
                            errors.push(e);
                        }
                    }
                }
            }
            RouteConfig::Static { root, .. } => {
                if !Path::new(root).exists() {
                    errors.push(ValidationError::FileNotFound { path: root.clone() });
                }
            }
            RouteConfig::Redirect {
                target,
                status_code,
                ..
            } => {
                // Validate redirect target (can be relative or absolute URL)
                if target.starts_with("http://") || target.starts_with("https://") {
                    if let Err(e) =
                        Self::validate_url(target, &format!("route '{path}' redirect target"))
                    {
                        errors.push(e);
                    }
                }

                // Validate status code
                if let Some(code) = status_code {
                    if !Self::is_valid_redirect_status_code(*code) {
                        errors.push(ValidationError::InvalidField {
                            field: format!("route '{path}' redirect status_code"),
                            message: format!("Status code {code} is not a valid redirect code. Use 301, 302, 307, or 308"),
                        });
                    }
                } else {
                    // Default status code should be valid
                    // This is OK, we'll use a default 302 in the actual implementation
                }
            }
        }

        // Validate rate limiting if configured
        let rate_limit = match config {
            RouteConfig::Proxy { rate_limit, .. } => rate_limit,
            RouteConfig::LoadBalance { rate_limit, .. } => rate_limit,
            RouteConfig::Static { rate_limit, .. } => rate_limit,
            RouteConfig::Redirect { rate_limit, .. } => rate_limit,
        };

        if let Some(rate_limit) = rate_limit {
            if let Err(e) = Self::validate_rate_limit(path, rate_limit) {
                errors.push(e);
            }
        }

        // Validate path rewrite regex if present
        let path_rewrite = match config {
            RouteConfig::Proxy { path_rewrite, .. } => path_rewrite,
            RouteConfig::LoadBalance { path_rewrite, .. } => path_rewrite,
            RouteConfig::Static { .. } => &None,
            RouteConfig::Redirect { .. } => &None,
        };

        if let Some(path_rewrite) = path_rewrite {
            if let Err(e) = Self::validate_path_rewrite(path, path_rewrite) {
                errors.push(e);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Validate URL format
    fn validate_url(url_str: &str, context: &str) -> ValidationResult<()> {
        match Url::parse(url_str) {
            Ok(url) => {
                if url.scheme() != "http" && url.scheme() != "https" {
                    return Err(ValidationError::InvalidUrl {
                        field: context.to_string(),
                        url: url_str.to_string(),
                        reason: "URL must use http:// or https:// scheme".to_string(),
                    });
                }

                if url.host().is_none() {
                    return Err(ValidationError::InvalidUrl {
                        field: context.to_string(),
                        url: url_str.to_string(),
                        reason: "URL must have a valid host".to_string(),
                    });
                }

                Ok(())
            }
            Err(e) => Err(ValidationError::InvalidUrl {
                field: context.to_string(),
                url: url_str.to_string(),
                reason: format!("Invalid URL format: {e}"),
            }),
        }
    }

    /// Validate rate limit configuration
    fn validate_rate_limit(route_path: &str, config: &RateLimitConfig) -> ValidationResult<()> {
        // Validate period format
        if let Err(e) = Self::parse_period(&config.period) {
            return Err(ValidationError::InvalidRateLimit {
                route: route_path.to_string(),
                message: format!(
                    "Invalid period format '{}': {}. Use formats like '1s', '1m', '1h'",
                    config.period, e
                ),
            });
        }

        // Validate requests count
        if config.requests == 0 {
            return Err(ValidationError::InvalidRateLimit {
                route: route_path.to_string(),
                message: "Request count must be greater than 0".to_string(),
            });
        }

        // Validate status code
        if config.status_code < 400 || config.status_code > 599 {
            return Err(ValidationError::InvalidRateLimit {
                route: route_path.to_string(),
                message: format!(
                    "Status code {} is not valid for rate limiting. Use 4xx or 5xx codes",
                    config.status_code
                ),
            });
        }

        // Validate header name if rate limiting by header
        if let crate::config::models::RateLimitBy::Header = config.by {
            if let Some(header_name) = &config.header_name {
                if header_name.is_empty() {
                    return Err(ValidationError::InvalidRateLimit {
                        route: route_path.to_string(),
                        message: "header_name cannot be empty when rate limiting by header"
                            .to_string(),
                    });
                }

                // Validate header name format
                if header_name.parse::<hyper::header::HeaderName>().is_err() {
                    return Err(ValidationError::InvalidRateLimit {
                        route: route_path.to_string(),
                        message: format!(
                            "Invalid header name '{header_name}'. Header names must be valid HTTP header names"
                        ),
                    });
                }
            } else {
                return Err(ValidationError::InvalidRateLimit {
                    route: route_path.to_string(),
                    message: "header_name is required when rate limiting by header".to_string(),
                });
            }
        }

        Ok(())
    }

    /// Validate TLS configuration
    fn validate_tls_config(config: &TlsConfig) -> ValidationResult<()> {
        match (&config.cert_path, &config.key_path, &config.acme) {
            (Some(cert_path), Some(key_path), None) => {
                // Manual certificate configuration
                if !Path::new(cert_path).exists() {
                    return Err(ValidationError::InvalidTls {
                        message: format!("Certificate file not found: {cert_path}"),
                    });
                }

                if !Path::new(key_path).exists() {
                    return Err(ValidationError::InvalidTls {
                        message: format!("Private key file not found: {key_path}"),
                    });
                }
            }
            (None, None, Some(acme_config)) => {
                // ACME configuration
                Self::validate_acme_config(acme_config)?;
            }
            (Some(_), Some(_), Some(_)) => {
                return Err(ValidationError::InvalidTls {
                    message: "Cannot specify both manual certificates (cert_path/key_path) and ACME configuration".to_string(),
                });
            }
            _ => {
                return Err(ValidationError::InvalidTls {
                    message: "TLS configuration must specify either manual certificates (cert_path + key_path) or ACME configuration".to_string(),
                });
            }
        }

        Ok(())
    }

    /// Validate ACME configuration
    fn validate_acme_config(config: &AcmeConfig) -> ValidationResult<()> {
        if !config.enabled {
            return Ok(()); // Skip validation if ACME is disabled
        }

        if config.domains.is_empty() {
            return Err(ValidationError::InvalidAcme {
                message: "At least one domain must be specified for ACME".to_string(),
            });
        }

        // Validate email format
        if !Self::is_valid_email(&config.email) {
            return Err(ValidationError::InvalidAcme {
                message: format!("Invalid email address: {}", config.email),
            });
        }

        // Validate domains
        for domain in &config.domains {
            if !Self::is_valid_domain(domain) {
                return Err(ValidationError::InvalidAcme {
                    message: format!("Invalid domain name: {domain}"),
                });
            }
        }

        // Validate renewal days
        if let Some(days) = config.renewal_days_before_expiry {
            if days == 0 || days > 89 {
                return Err(ValidationError::InvalidAcme {
                    message: format!(
                        "renewal_days_before_expiry must be between 1 and 89, got: {days}"
                    ),
                });
            }
        }

        Ok(())
    }

    /// Check for route conflicts (overlapping paths)
    fn check_route_conflicts(
        routes: &HashMap<String, RouteConfig>,
    ) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        let route_paths: Vec<&String> = routes.keys().collect();

        for (i, path1) in route_paths.iter().enumerate() {
            for path2 in route_paths.iter().skip(i + 1) {
                if Self::routes_conflict(path1, path2) {
                    errors.push(ValidationError::RouteConflict {
                        message: format!("Routes '{path1}' and '{path2}' have conflicting paths"),
                    });
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Check if two route paths conflict
    fn routes_conflict(path1: &str, path2: &str) -> bool {
        // Exact match
        if path1 == path2 {
            return true;
        }

        // Normalize paths (remove trailing slashes, but keep root "/")
        let path1_norm = if path1 == "/" {
            "/"
        } else {
            path1.trim_end_matches('/')
        };

        let path2_norm = if path2 == "/" {
            "/"
        } else {
            path2.trim_end_matches('/')
        };

        if path1_norm == path2_norm {
            return true;
        }

        // Special case: root path "/" doesn't conflict with specific paths like "/api"
        if path1_norm == "/" || path2_norm == "/" {
            return false;
        }

        // Check if one is a prefix of the other with a path separator
        // e.g., "/api" conflicts with "/api/v1" but not with "/apiv2"
        let longer = if path1_norm.len() > path2_norm.len() {
            path1_norm
        } else {
            path2_norm
        };
        let shorter = if path1_norm.len() <= path2_norm.len() {
            path1_norm
        } else {
            path2_norm
        };

        longer.starts_with(shorter)
            && (longer.len() == shorter.len() || longer.chars().nth(shorter.len()) == Some('/'))
    }

    /// Validate path rewrite pattern
    fn validate_path_rewrite(route_path: &str, path_rewrite: &str) -> ValidationResult<()> {
        // For now, we'll do basic validation. In the future, we could validate regex patterns
        if path_rewrite.is_empty() {
            return Err(ValidationError::InvalidField {
                field: format!("route '{route_path}' path_rewrite"),
                message: "Path rewrite cannot be empty".to_string(),
            });
        }

        // Validate that it starts with /
        if !path_rewrite.starts_with('/') {
            return Err(ValidationError::InvalidField {
                field: format!("route '{route_path}' path_rewrite"),
                message: "Path rewrite must start with '/'".to_string(),
            });
        }

        Ok(())
    }

    /// Parse period string (e.g., "1s", "5m", "1h") into Duration
    fn parse_period(period: &str) -> Result<Duration, String> {
        let period = period.trim();

        if period.is_empty() {
            return Err("Period cannot be empty".to_string());
        }

        let (number_part, unit_part) =
            if let Some(pos) = period.chars().position(|c| c.is_alphabetic()) {
                period.split_at(pos)
            } else {
                return Err("Period must include a unit (s, m, h)".to_string());
            };

        let number: u64 = number_part
            .parse()
            .map_err(|_| format!("Invalid number: {number_part}"))?;

        let duration = match unit_part.to_lowercase().as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => Duration::from_secs(number),
            "m" | "min" | "mins" | "minute" | "minutes" => Duration::from_secs(number * 60),
            "h" | "hr" | "hrs" | "hour" | "hours" => Duration::from_secs(number * 3600),
            _ => {
                return Err(format!(
                    "Invalid time unit: {unit_part}. Use 's', 'm', or 'h'"
                ));
            }
        };

        if duration.as_secs() == 0 {
            return Err("Period duration must be greater than 0".to_string());
        }

        Ok(duration)
    }

    /// Check if status code is valid for redirects
    fn is_valid_redirect_status_code(code: u16) -> bool {
        matches!(code, 301 | 302 | 307 | 308)
    }

    /// Basic email validation
    fn is_valid_email(email: &str) -> bool {
        let email_regex = Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$").unwrap();
        email_regex.is_match(email)
    }

    /// Basic domain name validation
    fn is_valid_domain(domain: &str) -> bool {
        let domain_regex = Regex::new(r"^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$").unwrap();
        domain_regex.is_match(domain) && domain.len() <= 253
    }

    /// Format multiple validation errors into a single message
    fn format_multiple_errors(errors: Vec<ValidationError>) -> String {
        let mut message = format!("Found {} validation error(s):\n", errors.len());
        for (i, error) in errors.iter().enumerate() {
            message.push_str(&format!("  {}. {}\n", i + 1, error));
        }
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::*;
    use std::collections::HashMap;

    fn create_valid_config() -> ServerConfig {
        let mut routes = HashMap::new();
        routes.insert(
            "/test".to_string(),
            RouteConfig::Proxy {
                target: "https://example.com".to_string(),
                path_rewrite: None,
                rate_limit: None,
                request_headers: None,
                response_headers: None,
                request_body: None,
                response_body: None,
            },
        );

        ServerConfig {
            listen_addr: "127.0.0.1:3000".to_string(),
            routes,
            tls: None,
            health_check: Default::default(),
            backend_health_paths: HashMap::new(),
        }
    }

    #[test]
    fn test_valid_config() {
        let config = create_valid_config();
        assert!(ConfigValidator::validate(&config).is_ok());
    }

    #[test]
    fn test_invalid_listen_address() {
        let mut config = create_valid_config();
        config.listen_addr = "invalid_address".to_string();

        let result = ConfigValidator::validate(&config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid listen address")
        );
    }

    #[test]
    fn test_missing_routes() {
        let mut config = create_valid_config();
        config.routes.clear();

        let result = ConfigValidator::validate(&config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing required field: routes")
        );
    }

    #[test]
    fn test_invalid_proxy_url() {
        let mut config = create_valid_config();
        config.routes.insert(
            "/test".to_string(),
            RouteConfig::Proxy {
                target: "not_a_url".to_string(),
                path_rewrite: None,
                rate_limit: None,
                request_headers: None,
                response_headers: None,
                request_body: None,
                response_body: None,
            },
        );

        let result = ConfigValidator::validate(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid URL"));
    }

    #[test]
    fn test_parse_period() {
        assert!(ConfigValidator::parse_period("30s").is_ok());
        assert!(ConfigValidator::parse_period("5m").is_ok());
        assert!(ConfigValidator::parse_period("1h").is_ok());
        assert!(ConfigValidator::parse_period("invalid").is_err());
        assert!(ConfigValidator::parse_period("").is_err());
    }

    #[test]
    fn test_email_validation() {
        assert!(ConfigValidator::is_valid_email("test@example.com"));
        assert!(ConfigValidator::is_valid_email("user.name@domain.co.uk"));
        assert!(!ConfigValidator::is_valid_email("invalid_email"));
        assert!(!ConfigValidator::is_valid_email("@domain.com"));
    }

    #[test]
    fn test_domain_validation() {
        assert!(ConfigValidator::is_valid_domain("example.com"));
        assert!(ConfigValidator::is_valid_domain("sub.example.com"));
        assert!(ConfigValidator::is_valid_domain("test-domain.co.uk"));
        assert!(!ConfigValidator::is_valid_domain(""));
        assert!(!ConfigValidator::is_valid_domain(".example.com"));
        assert!(!ConfigValidator::is_valid_domain("example..com"));
    }
}
