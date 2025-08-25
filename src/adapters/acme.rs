use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
};
use rcgen::CertificateParams;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::models::AcmeConfig;

pub struct AcmeService {
    config: AcmeConfig,
    storage_path: PathBuf,
}

#[derive(Debug)]
pub struct CertificateInfo {
    pub cert_path: String,
    pub key_path: String,
    pub expires_at: SystemTime,
}

impl AcmeService {
    pub fn new(config: AcmeConfig) -> Result<Self> {
        let storage_path = PathBuf::from(
            config
                .storage_path
                .as_ref()
                .unwrap_or(&"./acme_storage".to_string()),
        );

        // Create storage directory if it doesn't exist
        fs::create_dir_all(&storage_path).with_context(|| {
            format!("Failed to create ACME storage directory: {storage_path:?}")
        })?;

        Ok(Self {
            config,
            storage_path,
        })
    }

    /// Get the ACME directory URL based on configuration
    fn get_directory_url(&self) -> &'static str {
        if let Some(ref _ca_url) = self.config.ca_url {
            // For custom URLs, we'll need to handle this differently
            // For now, fall back to Let's Encrypt
            if self.config.staging.unwrap_or(false) {
                instant_acme::LetsEncrypt::Staging.url()
            } else {
                instant_acme::LetsEncrypt::Production.url()
            }
        } else if self.config.staging.unwrap_or(false) {
            instant_acme::LetsEncrypt::Staging.url()
        } else {
            instant_acme::LetsEncrypt::Production.url()
        }
    }

    /// Get certificate paths for a domain
    fn get_cert_paths(&self, domain: &str) -> (PathBuf, PathBuf) {
        let cert_path = self.storage_path.join(format!("{domain}.crt"));
        let key_path = self.storage_path.join(format!("{domain}.key"));
        (cert_path, key_path)
    }

    /// Check if certificate exists and is valid
    pub fn check_certificate(&self, domain: &str) -> Option<CertificateInfo> {
        let (cert_path, key_path) = self.get_cert_paths(domain);

        if !cert_path.exists() || !key_path.exists() {
            return None;
        }

        // For now, we'll use a simple file modification time check
        // In a production system, you'd want to parse the certificate and check expiration
        match fs::metadata(&cert_path) {
            Ok(metadata) => {
                if let Ok(modified) = metadata.modified() {
                    let renewal_threshold_days =
                        self.config.renewal_days_before_expiry.unwrap_or(30);

                    // Assume certificates are valid for 90 days (Let's Encrypt default)
                    let expires_at = modified + Duration::from_secs(90 * 24 * 60 * 60);

                    if SystemTime::now()
                        + Duration::from_secs(renewal_threshold_days * 24 * 60 * 60)
                        < expires_at
                    {
                        info!("Valid certificate found for domain: {}", domain);
                        let cert_info = CertificateInfo {
                            cert_path: cert_path.to_string_lossy().to_string(),
                            key_path: key_path.to_string_lossy().to_string(),
                            expires_at,
                        };
                        cert_info.log_info();
                        return Some(cert_info);
                    } else {
                        info!(
                            "Certificate for domain {} expires soon, needs renewal",
                            domain
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to read certificate metadata for domain {}: {}",
                    domain, e
                );
            }
        }

        None
    }

    /// Request a new certificate for the given domains
    pub async fn request_certificate(&self, domains: &[String]) -> Result<CertificateInfo> {
        if domains.is_empty() {
            return Err(anyhow!("No domains specified for certificate request"));
        }

        let primary_domain = &domains[0];
        info!("Requesting certificate for domains: {:?}", domains);

        // Create account
        let directory_url = self.get_directory_url();
        let (account, _credentials) = Account::create(
            &NewAccount {
                contact: &[&format!("mailto:{email}", email = self.config.email)],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory_url,
            None,
        )
        .await
        .context("Failed to create ACME account")?;

        // Create identifiers for all domains
        let identifiers: Vec<Identifier> = domains
            .iter()
            .map(|domain| Identifier::Dns(domain.clone()))
            .collect();

        // Create order
        let mut order = account
            .new_order(&NewOrder {
                identifiers: &identifiers,
            })
            .await
            .context("Failed to create new order")?;

        // Process authorizations
        let authorizations = order
            .authorizations()
            .await
            .context("Failed to get authorizations")?;

        for authorization in authorizations {
            if authorization.status == AuthorizationStatus::Valid {
                info!(
                    "Authorization already valid for: {:?}",
                    authorization.identifier
                );
                continue;
            }

            // Find HTTP-01 challenge
            let challenge = authorization
                .challenges
                .iter()
                .find(|c| c.r#type == ChallengeType::Http01)
                .ok_or_else(|| anyhow!("No HTTP-01 challenge found"))?;

            let token = &challenge.token;
            let key_authorization = order.key_authorization(challenge);

            info!(
                "Setting up HTTP challenge for domain: {:?}",
                authorization.identifier
            );
            info!("Token: {}", token);

            // Create challenge directory and file
            let well_known_path = Path::new("./static/.well-known/acme-challenge");
            fs::create_dir_all(well_known_path)
                .with_context(|| "Failed to create .well-known directory")?;

            let challenge_file = well_known_path.join(token);
            fs::write(&challenge_file, key_authorization.as_str())
                .with_context(|| "Failed to write challenge file")?;

            info!("Created challenge file: {:?}", challenge_file);

            // Validate challenge
            order
                .set_challenge_ready(&challenge.url)
                .await
                .context("Failed to validate challenge")?;

            // Wait for validation
            let mut attempts = 0;
            loop {
                sleep(Duration::from_secs(2)).await;
                attempts += 1;

                let updated_authorizations = order
                    .authorizations()
                    .await
                    .context("Failed to get updated authorizations")?;

                let updated_auth = updated_authorizations
                    .iter()
                    .find(|auth| auth.identifier == authorization.identifier)
                    .ok_or_else(|| {
                        anyhow!(
                            "Authorization not found for domain: {:?}",
                            authorization.identifier
                        )
                    })?;

                match updated_auth.status {
                    AuthorizationStatus::Valid => {
                        info!(
                            "Challenge validated for domain: {:?}",
                            authorization.identifier
                        );
                        break;
                    }
                    AuthorizationStatus::Invalid => {
                        return Err(anyhow!(
                            "Challenge validation failed for domain: {:?}",
                            authorization.identifier
                        ));
                    }
                    AuthorizationStatus::Pending => {
                        if attempts > 30 {
                            return Err(anyhow!(
                                "Challenge validation timeout for domain: {:?}",
                                authorization.identifier
                            ));
                        }
                        continue;
                    }
                    _ => {
                        if attempts > 30 {
                            return Err(anyhow!(
                                "Challenge validation timeout for domain: {:?}",
                                authorization.identifier
                            ));
                        }
                        continue;
                    }
                }
            }

            // Clean up challenge file
            let _ = fs::remove_file(&challenge_file);
        }

        // Generate CSR using rcgen 0.13 API
        let params = CertificateParams::new(domains)?;
        let key_pair = rcgen::KeyPair::generate()?;
        let csr_obj = params.serialize_request(&key_pair)?;
        let csr = csr_obj.der();

        // Finalize order
        order
            .finalize(csr)
            .await
            .context("Failed to finalize order")?;

        // Wait for certificate
        let mut attempts = 0;
        loop {
            sleep(Duration::from_secs(2)).await;
            attempts += 1;

            let state = order.state();
            match state.status {
                OrderStatus::Valid => {
                    if let Some(cert_chain) = order
                        .certificate()
                        .await
                        .context("Failed to download certificate")?
                    {
                        // Save certificate and private key
                        let (cert_path, key_path) = self.get_cert_paths(primary_domain);

                        fs::write(&cert_path, &cert_chain)
                            .with_context(|| "Failed to save certificate")?;
                        fs::write(&key_path, key_pair.serialize_pem())
                            .with_context(|| "Failed to save private key")?;

                        info!(
                            "Certificate saved for domain: {} at {:?}",
                            primary_domain, cert_path
                        );

                        // Calculate expiration time (Let's Encrypt certificates are valid for 90 days)
                        let expires_at = SystemTime::now() + Duration::from_secs(90 * 24 * 60 * 60);

                        return Ok(CertificateInfo {
                            cert_path: cert_path.to_string_lossy().to_string(),
                            key_path: key_path.to_string_lossy().to_string(),
                            expires_at,
                        });
                    } else {
                        return Err(anyhow!("Order is valid but no certificate available"));
                    }
                }
                OrderStatus::Invalid => {
                    return Err(anyhow!("Order became invalid"));
                }
                OrderStatus::Pending | OrderStatus::Processing => {
                    if attempts > 30 {
                        return Err(anyhow!("Certificate issuance timeout"));
                    }
                    continue;
                }
                OrderStatus::Ready => {
                    if attempts > 30 {
                        return Err(anyhow!("Certificate issuance timeout"));
                    }
                    continue;
                }
            }
        }
    }

    /// Get certificate for the configured domains, requesting new one if needed
    pub async fn get_certificate(&self) -> Result<CertificateInfo> {
        if self.config.domains.is_empty() {
            return Err(anyhow!("No domains configured for ACME"));
        }

        // Check for any expired certificates first
        if self.has_expired_certificate() {
            warn!("Found expired certificates! Certificate status for all domains:");
            for (domain, cert_info) in self.get_certificate_status() {
                match cert_info {
                    Some(info) => {
                        if info.is_expired() {
                            error!("Domain '{}' has EXPIRED certificate!", domain);
                        } else {
                            info!(
                                "Domain '{}' certificate expires in {} days",
                                domain,
                                info.days_until_expiry()
                            );
                        }
                    }
                    None => warn!("Domain '{}' has no certificate", domain),
                }
            }
        }

        let primary_domain = &self.config.domains[0];

        // Check if we have a valid certificate
        if let Some(cert_info) = self.check_certificate(primary_domain) {
            cert_info.log_info();
            return Ok(cert_info);
        }

        // Request new certificate
        info!(
            "No valid certificate found for domain: {}, requesting new certificate",
            primary_domain
        );
        let cert_info = self.request_certificate(&self.config.domains).await?;
        cert_info.log_info();
        Ok(cert_info)
    }

    /// Start a background task to monitor and renew certificates
    pub fn start_renewal_task(&self) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();

        tokio::spawn(async move {
            let service = match AcmeService::new(config) {
                Ok(service) => service,
                Err(e) => {
                    error!("Failed to create ACME service for renewal task: {}", e);
                    return;
                }
            };

            let check_interval = Duration::from_secs(24 * 60 * 60); // Check daily

            loop {
                sleep(check_interval).await;

                info!("Checking certificate renewal status");

                // Log status for all certificates
                let cert_statuses = service.get_certificate_status();
                info!(
                    "Certificate status summary for {} domains:",
                    cert_statuses.len()
                );

                for (domain, cert_info) in &cert_statuses {
                    match cert_info {
                        Some(info) => {
                            let days_left = info.days_until_expiry();
                            if info.is_expired() {
                                error!(
                                    "Domain '{}' certificate EXPIRED {} days ago!",
                                    domain, -days_left
                                );
                            } else if days_left < 30 {
                                warn!(
                                    "Domain '{}' certificate expires in {} days",
                                    domain, days_left
                                );
                            } else {
                                info!(
                                    "Domain '{}' certificate valid for {} days",
                                    domain, days_left
                                );
                            }
                        }
                        None => warn!("Domain '{}' has no certificate", domain),
                    }
                }

                // Check if we need to renew any certificates
                let needs_renewal = service.has_expired_certificate()
                    || cert_statuses.iter().any(|(_, cert_info)| {
                        if let Some(info) = cert_info {
                            let renewal_days =
                                service.config.renewal_days_before_expiry.unwrap_or(30);
                            info.expires_within_days(renewal_days)
                        } else {
                            true // No certificate means we need one
                        }
                    });

                if needs_renewal {
                    info!("Certificate renewal required");

                    match service.request_certificate(&service.config.domains).await {
                        Ok(cert_info) => {
                            info!("Successfully renewed/obtained certificate");
                            cert_info.log_info();
                        }
                        Err(e) => {
                            error!("Failed to renew/obtain certificate: {}", e);
                        }
                    }
                } else {
                    info!("All certificates are valid and don't need renewal yet");
                }
            }
        })
    }

    /// Check if any of the configured domains has an expired certificate
    pub fn has_expired_certificate(&self) -> bool {
        for domain in &self.config.domains {
            if matches!(self.check_certificate(domain), Some(cert_info) if cert_info.is_expired()) {
                return true;
            }
        }
        false
    }

    /// Get certificate status for all configured domains
    pub fn get_certificate_status(&self) -> Vec<(String, Option<CertificateInfo>)> {
        self.config
            .domains
            .iter()
            .map(|domain| {
                let cert_info = self.check_certificate(domain);
                (domain.clone(), cert_info)
            })
            .collect()
    }
}

impl CertificateInfo {
    /// Check if the certificate is expired
    #[allow(dead_code)]
    pub fn is_expired(&self) -> bool {
        SystemTime::now() >= self.expires_at
    }

    /// Check if the certificate will expire within the given number of days
    pub fn expires_within_days(&self, days: u64) -> bool {
        let threshold = SystemTime::now() + Duration::from_secs(days * 24 * 60 * 60);
        threshold >= self.expires_at
    }

    /// Get the number of days until expiration (negative if expired)
    pub fn days_until_expiry(&self) -> i64 {
        match self.expires_at.duration_since(SystemTime::now()) {
            Ok(duration) => (duration.as_secs() / (24 * 60 * 60)) as i64,
            Err(_) => {
                // Certificate is expired
                match SystemTime::now().duration_since(self.expires_at) {
                    Ok(duration) => -((duration.as_secs() / (24 * 60 * 60)) as i64),
                    Err(_) => 0,
                }
            }
        }
    }

    /// Log certificate information
    pub fn log_info(&self) {
        let days_until_expiry = self.days_until_expiry();
        if days_until_expiry < 0 {
            error!(
                "Certificate EXPIRED {} days ago! cert={}",
                -days_until_expiry, self.cert_path
            );
        } else if days_until_expiry < 30 {
            warn!(
                "Certificate expires in {} days. cert={}",
                days_until_expiry, self.cert_path
            );
        } else {
            info!(
                "Certificate valid for {} days. cert={}",
                days_until_expiry, self.cert_path
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn create_test_acme_config() -> AcmeConfig {
        AcmeConfig {
            enabled: true,
            domains: vec![
                "test.example.com".to_string(),
                "www.test.example.com".to_string(),
            ],
            email: "test@example.com".to_string(),
            ca_url: None,
            staging: Some(true),
            storage_path: None, // Will be set by individual tests
            renewal_days_before_expiry: Some(30),
        }
    }

    #[test]
    fn test_acme_service_new() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let mut config = create_test_acme_config();
        config.storage_path = Some(temp_dir.path().to_string_lossy().to_string());

        let service = AcmeService::new(config).expect("Failed to create ACME service");

        assert_eq!(service.config.domains.len(), 2);
        assert_eq!(service.config.email, "test@example.com");
        assert!(service.storage_path.exists());
    }

    #[test]
    fn test_get_directory_url() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let mut config = create_test_acme_config();
        config.storage_path = Some(temp_dir.path().to_string_lossy().to_string());

        // Test staging URL
        config.staging = Some(true);
        let service = AcmeService::new(config.clone()).expect("Failed to create ACME service");
        assert!(service.get_directory_url().contains("staging"));

        // Test production URL
        config.staging = Some(false);
        let service = AcmeService::new(config.clone()).expect("Failed to create ACME service");
        assert!(!service.get_directory_url().contains("staging"));
    }

    #[test]
    fn test_certificate_info_is_expired() {
        // Test with expired certificate - using a time far in the past
        let expired_cert = CertificateInfo {
            cert_path: "/test/cert.pem".to_string(),
            key_path: "/test/key.pem".to_string(),
            expires_at: SystemTime::now() - Duration::from_secs(86400), // 1 day ago
        };
        assert!(expired_cert.is_expired());

        // Test with valid certificate
        let valid_cert = CertificateInfo {
            cert_path: "/test/cert.pem".to_string(),
            key_path: "/test/key.pem".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(86400), // 1 day in the future
        };
        assert!(!valid_cert.is_expired());
    }

    #[test]
    fn test_certificate_info_expires_within_days() {
        let cert = CertificateInfo {
            cert_path: "/test/cert.pem".to_string(),
            key_path: "/test/key.pem".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(15 * 24 * 60 * 60), // 15 days
        };

        assert!(cert.expires_within_days(30)); // Expires within 30 days
        assert!(cert.expires_within_days(15)); // Expires within 15 days
        assert!(!cert.expires_within_days(10)); // Does not expire within 10 days
    }

    #[test]
    fn test_certificate_info_days_until_expiry() {
        // Test certificate expiring in the future
        let future_cert = CertificateInfo {
            cert_path: "/test/cert.pem".to_string(),
            key_path: "/test/key.pem".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(10 * 24 * 60 * 60), // 10 days
        };
        let days = future_cert.days_until_expiry();
        assert!((9..=10).contains(&days)); // Allow for small timing differences

        // Test expired certificate
        let expired_cert = CertificateInfo {
            cert_path: "/test/cert.pem".to_string(),
            key_path: "/test/key.pem".to_string(),
            expires_at: SystemTime::now() - Duration::from_secs(5 * 24 * 60 * 60), // 5 days ago
        };
        let days = expired_cert.days_until_expiry();
        assert!((-5..=-4).contains(&days)); // Negative for expired certs
    }

    #[tokio::test]
    async fn test_get_certificate_no_domains() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let mut config = create_test_acme_config();
        config.domains = vec![]; // No domains
        config.storage_path = Some(temp_dir.path().to_string_lossy().to_string());

        let service = AcmeService::new(config).expect("Failed to create ACME service");
        let result = service.get_certificate().await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No domains configured for ACME")
        );
    }
}
