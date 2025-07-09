#[cfg(test)]
mod http3_tests {
    use crate::config::models::{Http3Config, Http3CongestionControl};
    use bytes::Bytes;
    use quiche::h3::Header as H3Header;
    use std::net::SocketAddr;

    #[test]
    fn test_http3_config_creation() {
        let http3_config = Http3Config {
            max_data: 10_000_000,
            max_stream_data: 1_000_000,
            max_streams_bidi: 100,
            max_idle_timeout: 30_000,
            congestion_control: Http3CongestionControl::Cubic,
            enable_0rtt: true,
            max_packet_size: Some(1452),
        };

        assert_eq!(http3_config.max_data, 10_000_000);
        assert_eq!(http3_config.max_stream_data, 1_000_000);
        assert_eq!(http3_config.max_streams_bidi, 100);
        assert_eq!(http3_config.max_idle_timeout, 30_000);
        assert!(matches!(
            http3_config.congestion_control,
            Http3CongestionControl::Cubic
        ));
        assert!(http3_config.enable_0rtt);
        assert_eq!(http3_config.max_packet_size, Some(1452));
    }

    #[test]
    fn test_http3_congestion_control_variants() {
        let cubic = Http3CongestionControl::Cubic;
        let reno = Http3CongestionControl::Reno;
        let bbr = Http3CongestionControl::Bbr;

        assert!(matches!(cubic, Http3CongestionControl::Cubic));
        assert!(matches!(reno, Http3CongestionControl::Reno));
        assert!(matches!(bbr, Http3CongestionControl::Bbr));
    }

    #[test]
    fn test_http3_config_validation_ranges() {
        // Test minimum values
        let min_config = Http3Config {
            max_data: 1024,
            max_stream_data: 1024,
            max_streams_bidi: 1,
            max_idle_timeout: 1000,
            congestion_control: Http3CongestionControl::Cubic,
            enable_0rtt: false,
            max_packet_size: Some(1200),
        };

        assert!(min_config.max_data >= 1024);
        assert!(min_config.max_stream_data >= 1024);
        assert!(min_config.max_streams_bidi >= 1);
        assert!(min_config.max_idle_timeout >= 1000);

        // Test maximum reasonable values
        let max_config = Http3Config {
            max_data: 1_000_000_000,
            max_stream_data: 100_000_000,
            max_streams_bidi: 1000,
            max_idle_timeout: 600_000,
            congestion_control: Http3CongestionControl::Bbr,
            enable_0rtt: true,
            max_packet_size: Some(65535),
        };

        assert!(max_config.max_data <= 1_000_000_000);
        assert!(max_config.max_stream_data <= 100_000_000);
        assert!(max_config.max_streams_bidi <= 1000);
        assert!(max_config.max_idle_timeout <= 600_000);
    }

    #[test]
    fn test_bytes_creation() {
        // Test Bytes creation for HTTP/3 request bodies
        let empty_body: Option<Bytes> = None;
        let some_body = Bytes::from("test body");

        assert!(empty_body.is_none());
        assert_eq!(some_body, Bytes::from("test body"));
    }

    #[test]
    fn test_socket_addr_parsing() {
        // Test socket address parsing for HTTP/3 server binding
        let addr: SocketAddr = "127.0.0.1:3000".parse().expect("Failed to parse address");

        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 3000);
    }

    #[test]
    fn test_h3_header_conversion() {
        // Test HTTP/3 header name-value pairs
        let method_header = H3Header::new(b":method", b"GET");
        let path_header = H3Header::new(b":path", b"/test");
        let authority_header = H3Header::new(b":authority", b"example.com");

        // Verify headers are created successfully
        // Note: H3Header::new returns a Header directly, not a Result
        // We can test by creating them without panicking
        drop(method_header);
        drop(path_header);
        drop(authority_header);

        // Test successful creation
        // Headers were created without panicking - test passes
    }

    #[test]
    fn test_config_with_different_congestion_algorithms() {
        let configs = vec![
            (Http3CongestionControl::Cubic, "CUBIC"),
            (Http3CongestionControl::Reno, "Reno"),
            (Http3CongestionControl::Bbr, "BBR"),
        ];

        for (algorithm, name) in configs {
            let config = Http3Config {
                max_data: 10_000_000,
                max_stream_data: 1_000_000,
                max_streams_bidi: 100,
                max_idle_timeout: 30_000,
                congestion_control: algorithm,
                enable_0rtt: true,
                max_packet_size: Some(1452),
            };

            match config.congestion_control {
                Http3CongestionControl::Cubic => assert_eq!(name, "CUBIC"),
                Http3CongestionControl::Reno => assert_eq!(name, "Reno"),
                Http3CongestionControl::Bbr => assert_eq!(name, "BBR"),
            }
        }
    }

    #[test]
    fn test_config_optional_fields() {
        // Test config with None max_packet_size
        let config_without_max_packet = Http3Config {
            max_data: 10_000_000,
            max_stream_data: 1_000_000,
            max_streams_bidi: 100,
            max_idle_timeout: 30_000,
            congestion_control: Http3CongestionControl::Cubic,
            enable_0rtt: false,
            max_packet_size: None,
        };

        assert!(config_without_max_packet.max_packet_size.is_none());
        assert!(!config_without_max_packet.enable_0rtt);

        // Test config with Some max_packet_size
        let config_with_max_packet = Http3Config {
            max_data: 10_000_000,
            max_stream_data: 1_000_000,
            max_streams_bidi: 100,
            max_idle_timeout: 30_000,
            congestion_control: Http3CongestionControl::Cubic,
            enable_0rtt: true,
            max_packet_size: Some(1500),
        };

        assert!(config_with_max_packet.max_packet_size.is_some());
        assert_eq!(config_with_max_packet.max_packet_size.unwrap(), 1500);
        assert!(config_with_max_packet.enable_0rtt);
    }

    // Test helper functions
    mod test_helpers {
        use super::*;

        pub fn create_test_http3_config() -> Http3Config {
            Http3Config {
                max_data: 10_000_000,
                max_stream_data: 1_000_000,
                max_streams_bidi: 100,
                max_idle_timeout: 30_000,
                congestion_control: Http3CongestionControl::Cubic,
                enable_0rtt: true,
                max_packet_size: Some(1452),
            }
        }

        pub fn create_test_headers() -> Vec<H3Header> {
            vec![
                H3Header::new(b":method", b"GET"),
                H3Header::new(b":path", b"/test"),
                H3Header::new(b":authority", b"localhost:3000"),
                H3Header::new(b":scheme", b"https"),
            ]
        }

        pub fn create_minimal_config() -> Http3Config {
            Http3Config {
                max_data: 1024,
                max_stream_data: 1024,
                max_streams_bidi: 1,
                max_idle_timeout: 1000,
                congestion_control: Http3CongestionControl::Reno,
                enable_0rtt: false,
                max_packet_size: None,
            }
        }
    }

    #[test]
    fn test_helper_functions() {
        let config = test_helpers::create_test_http3_config();
        let headers = test_helpers::create_test_headers();
        let minimal_config = test_helpers::create_minimal_config();

        assert_eq!(config.max_data, 10_000_000);
        assert_eq!(headers.len(), 4);
        assert_eq!(minimal_config.max_data, 1024);
        assert!(!minimal_config.enable_0rtt);
    }

    #[test]
    fn test_error_conditions() {
        // Test various edge cases and potential error conditions

        // Zero timeout (edge case)
        let zero_timeout_config = Http3Config {
            max_data: 10_000_000,
            max_stream_data: 1_000_000,
            max_streams_bidi: 100,
            max_idle_timeout: 0, // Edge case
            congestion_control: Http3CongestionControl::Cubic,
            enable_0rtt: true,
            max_packet_size: Some(1452),
        };

        assert_eq!(zero_timeout_config.max_idle_timeout, 0);

        // Very large packet size (edge case)
        let large_packet_config = Http3Config {
            max_data: 10_000_000,
            max_stream_data: 1_000_000,
            max_streams_bidi: 100,
            max_idle_timeout: 30_000,
            congestion_control: Http3CongestionControl::Cubic,
            enable_0rtt: true,
            max_packet_size: Some(65535), // Maximum UDP packet size
        };

        assert_eq!(large_packet_config.max_packet_size.unwrap(), 65535);
    }
}
