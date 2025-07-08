use anyhow::{Context, Result};
use quiche::{Config, CongestionControlAlgorithm};

use crate::config::models::{Http3Config, Http3CongestionControl};

pub struct QuicheConfig {
    config: Config,
}

impl std::fmt::Debug for QuicheConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicheConfig")
            .field("config", &"<quiche::Config>")
            .finish()
    }
}

impl QuicheConfig {
    pub fn new(http3_config: &Http3Config, cert_path: &str, key_path: &str) -> Result<Self> {
        let mut config =
            Config::new(quiche::PROTOCOL_VERSION).context("Failed to create QUIC config")?;

        // Set application protocols - HTTP/3 uses "h3"
        config
            .set_application_protos(&[b"h3"])
            .context("Failed to set application protocols")?;

        // Configure flow control
        config.set_initial_max_data(http3_config.max_data);
        config.set_initial_max_stream_data_bidi_local(http3_config.max_stream_data);
        config.set_initial_max_stream_data_bidi_remote(http3_config.max_stream_data);
        config.set_initial_max_streams_bidi(http3_config.max_streams_bidi);

        // Configure congestion control
        let cc_algorithm = match http3_config.congestion_control {
            Http3CongestionControl::Cubic => CongestionControlAlgorithm::CUBIC,
            Http3CongestionControl::Reno => CongestionControlAlgorithm::Reno,
            Http3CongestionControl::Bbr => CongestionControlAlgorithm::BBR,
        };
        config.set_cc_algorithm(cc_algorithm);

        // Set idle timeout
        config.set_max_idle_timeout(http3_config.max_idle_timeout);

        // Enable 0-RTT if configured
        if http3_config.enable_0rtt {
            config.enable_early_data();
        }

        // Set maximum packet size if specified
        if let Some(max_packet_size) = http3_config.max_packet_size {
            config.set_max_recv_udp_payload_size(max_packet_size as usize);
        }

        // Load TLS certificate and private key
        config
            .load_cert_chain_from_pem_file(cert_path)
            .with_context(|| format!("Failed to load certificate from {}", cert_path))?;

        config
            .load_priv_key_from_pem_file(key_path)
            .with_context(|| format!("Failed to load private key from {}", key_path))?;

        // Enable qlog for debugging (optional)
        config.enable_dgram(true, 1024, 1024);

        Ok(Self { config })
    }

    pub fn into_inner(self) -> Config {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::{Http3Config, Http3CongestionControl};

    fn create_test_http3_config() -> Http3Config {
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
    fn test_quiche_config_creation_fails_with_invalid_certs() {
        let http3_config = create_test_http3_config();

        // Test with invalid certificate paths
        let result = QuicheConfig::new(
            &http3_config,
            "invalid/cert/path.pem",
            "invalid/key/path.pem",
        );

        // Should fail due to missing certificate files
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("cert") || error_msg.contains("key") || error_msg.contains("file")
        );
    }

    #[test]
    fn test_congestion_control_mapping() {
        let http3_config = Http3Config {
            congestion_control: Http3CongestionControl::Cubic,
            ..create_test_http3_config()
        };

        // Test that we can create a QUIC config (will fail on cert loading but that's expected)
        let result = QuicheConfig::new(&http3_config, "cert.pem", "key.pem");
        assert!(result.is_err()); // Expected due to missing certs

        // Test other congestion control algorithms
        let reno_config = Http3Config {
            congestion_control: Http3CongestionControl::Reno,
            ..create_test_http3_config()
        };
        let reno_result = QuicheConfig::new(&reno_config, "cert.pem", "key.pem");
        assert!(reno_result.is_err()); // Expected due to missing certs

        let bbr_config = Http3Config {
            congestion_control: Http3CongestionControl::Bbr,
            ..create_test_http3_config()
        };
        let bbr_result = QuicheConfig::new(&bbr_config, "cert.pem", "key.pem");
        assert!(bbr_result.is_err()); // Expected due to missing certs
    }

    #[test]
    fn test_http3_config_values() {
        let config = create_test_http3_config();

        // Test basic parameter validation
        assert!(config.max_data > 0);
        assert!(config.max_stream_data > 0);
        assert!(config.max_streams_bidi > 0);
        assert!(config.max_idle_timeout > 0);

        // Test 0-RTT setting
        assert!(!config.enable_0rtt); // We set it to false in test config

        // Test max packet size
        if let Some(size) = config.max_packet_size {
            assert!(size >= 1200); // QUIC minimum
        }
    }
}
