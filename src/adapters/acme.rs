use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use acme_lib::{Directory, DirectoryUrl, create_p384_key, persist::FilePersist};
use anyhow::{Context, Result, anyhow};
use openssl::x509::X509;
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
            format!(
                "Failed to create ACME storage directory: {:?}",
                storage_path
            )
        })?;

        Ok(Self {
            config,
            storage_path,
        })
    }

    /// Get the ACME directory URL based on configuration
    fn get_directory_url(&self) -> DirectoryUrl {
        if let Some(ref ca_url) = self.config.ca_url {
            DirectoryUrl::Other(ca_url)
        } else if self.config.staging.unwrap_or(false) {
            DirectoryUrl::LetsEncryptStaging
        } else {
            DirectoryUrl::LetsEncrypt
        }
    }

    /// Get certificate paths for a domain
    fn get_cert_paths(&self, domain: &str) -> (PathBuf, PathBuf) {
        let cert_path = self.storage_path.join(format!("{}.crt", domain));
        let key_path = self.storage_path.join(format!("{}.key", domain));
        (cert_path, key_path)
    }

    /// Check if certificate exists and is valid
    pub fn check_certificate(&self, domain: &str) -> Option<CertificateInfo> {
        let (cert_path, key_path) = self.get_cert_paths(domain);

        if !cert_path.exists() || !key_path.exists() {
            return None;
        }

        // Check certificate expiration
        match fs::read(&cert_path) {
            Ok(cert_data) => {
                match X509::from_pem(&cert_data) {
                    Ok(cert) => {
                        let not_after = cert.not_after();
                        // Convert ASN1Time to SystemTime
                        let expires_at = SystemTime::UNIX_EPOCH
                            + Duration::from_secs(
                                not_after
                                    .diff(&openssl::asn1::Asn1Time::from_unix(0).unwrap())
                                    .unwrap()
                                    .days as u64
                                    * 24
                                    * 60
                                    * 60,
                            );

                        let renewal_threshold_days =
                            self.config.renewal_days_before_expiry.unwrap_or(30);

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
                    Err(e) => {
                        warn!("Failed to parse certificate for domain {}: {}", domain, e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to read certificate for domain {}: {}", domain, e);
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

        // Setup file persistence for ACME account
        let persist = FilePersist::new(&self.storage_path);

        // Create directory and account
        let dir = Directory::from_url(persist, self.get_directory_url())?;
        let acc = dir.account(&self.config.email)?;

        // Create order for domains
        let mut ord_new = acc.new_order(primary_domain, &[])?;

        // Go through all authorizations
        let ord_csr = loop {
            if let Some(ord_csr) = ord_new.confirm_validations() {
                break ord_csr;
            }

            let auths = ord_new.authorizations()?;

            for auth in auths {
                let chall = auth.http_challenge();
                let token = chall.http_token();
                let proof = chall.http_proof();

                info!(
                    "Setting up HTTP challenge for domain: {}",
                    auth.domain_name()
                );
                info!("Token: {}, Proof: {}", token, proof);

                // Create challenge directory and file
                let well_known_path = Path::new("./static/.well-known/acme-challenge");
                fs::create_dir_all(well_known_path)
                    .with_context(|| "Failed to create .well-known directory")?;

                let challenge_file = well_known_path.join(token);
                fs::write(&challenge_file, proof)
                    .with_context(|| "Failed to write challenge file")?;

                info!("Created challenge file: {:?}", challenge_file);

                // Validate challenge
                chall.validate(5000)?;

                // Clean up challenge file
                let _ = fs::remove_file(&challenge_file);
                info!("Challenge validated for domain: {}", auth.domain_name());
            }

            ord_new.refresh()?;
        };

        // Generate private key and certificate signing request
        let pkey_pri = create_p384_key();
        let ord_cert = ord_csr.finalize_pkey(pkey_pri, 5000)?;
        let cert = ord_cert.download_and_save_cert()?;

        // Save certificate and private key to our custom paths
        let (cert_path, key_path) = self.get_cert_paths(primary_domain);

        fs::write(&cert_path, cert.certificate()).with_context(|| "Failed to save certificate")?;
        fs::write(&key_path, cert.private_key()).with_context(|| "Failed to save private key")?;

        info!(
            "Certificate saved for domain: {} at {:?}",
            primary_domain, cert_path
        );

        // Parse certificate to get expiration time
        let cert_x509 = X509::from_pem(cert.certificate().as_bytes())
            .with_context(|| "Failed to parse saved certificate")?;
        let not_after = cert_x509.not_after();
        let expires_at = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                not_after
                    .diff(&openssl::asn1::Asn1Time::from_unix(0).unwrap())
                    .unwrap()
                    .days as u64
                    * 24
                    * 60
                    * 60,
            );

        Ok(CertificateInfo {
            cert_path: cert_path.to_string_lossy().to_string(),
            key_path: key_path.to_string_lossy().to_string(),
            expires_at,
        })
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
            if let Some(cert_info) = self.check_certificate(domain) {
                if cert_info.is_expired() {
                    return true;
                }
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
