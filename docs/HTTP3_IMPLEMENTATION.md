# HTTP/3 Integration Implementation

This document describes the HTTP/3 support implementation in the prox reverse proxy.

## Overview

The prox reverse proxy now supports HTTP/3 (HTTP over QUIC) alongside HTTP/1.1 and HTTP/2. This implementation provides:

- **Unified Server Architecture**: Single server supporting TCP (HTTP/1.1, HTTP/2) and UDP (HTTP/3) protocols
- **Automatic Protocol Detection**: Clients can discover HTTP/3 support via Alt-Svc headers
- **Configurable HTTP/3 Settings**: Comprehensive configuration options for QUIC parameters
- **TLS Integration**: Seamless certificate sharing between HTTP/2 and HTTP/3

## Architecture

### Components

1. **UnifiedServer**: Main server component that manages both HTTP and HTTP/3 servers
2. **Http3Server**: UDP-based server handling QUIC connections
3. **ConnectionManager**: Manages QUIC connection lifecycle and HTTP/3 streams
4. **Http3Handler**: Processes HTTP/3 requests and integrates with existing proxy logic
5. **QuicheConfig**: Configuration wrapper for QUIC settings

### Protocol Flow

```
Client ──TCP──► HTTP Server (HTTP/1.1, HTTP/2)
       │                     │
       └─UDP──► HTTP/3 Server │
                             │
                             ▼
                    Unified Request Handler
                             │
                             ▼
                    Backend Services
```

## Configuration

### Basic Setup

```yaml
# Enable HTTP/3 support
protocols:
  http2_enabled: true
  websocket_enabled: true
  http3_enabled: true  # Enable HTTP/3

# TLS is required for HTTP/3
tls:
  cert_path: "certs/cert.pem"
  key_path: "certs/key.pem"
```

### Advanced Configuration

```yaml
protocols:
  http3_enabled: true
  http3_config:
    max_data: 10000000           # 10MB max data per connection
    max_stream_data: 1000000     # 1MB max data per stream
    max_streams_bidi: 100        # Max 100 bidirectional streams
    max_idle_timeout: 30000      # 30 second idle timeout (ms)
    congestion_control: "cubic"  # CUBIC, reno, or bbr
    enable_0rtt: true           # Enable 0-RTT connection resumption
    max_packet_size: 1452       # Optional: max UDP packet size
```

### Key Parameters

| Parameter | Default | Range | Description |
|-----------|---------|-------|-------------|
| `max_data` | 10,000,000 | 1024 - 2^60 | Maximum data per connection (bytes) |
| `max_stream_data` | 1,000,000 | 1024 - 2^60 | Maximum data per stream (bytes) |
| `max_streams_bidi` | 100 | 1 - 2^60 | Maximum bidirectional streams |
| `max_idle_timeout` | 30,000 | 1000 - 600000 | Connection idle timeout (ms) |
| `congestion_control` | "cubic" | cubic, reno, bbr | Congestion control algorithm |
| `enable_0rtt` | true | true, false | Enable 0-RTT resumption |

## Features

- **Alt-Svc Header Support**: Automatic `Alt-Svc: h3=":443"; ma=3600` advertisement
- **Certificate Sharing**: Uses same TLS certificates as HTTP/2
- **0-RTT Connection Resumption**: Reduced latency for returning clients
- **Stream Multiplexing**: Efficient single UDP connection usage
- **Congestion Control**: Adaptive algorithms (CUBIC, Reno, BBR)
- **Connection Migration**: Seamless network changes (mobile scenarios)

## Testing

### Validation Command

```bash
cargo run -- validate --config config-http3-example.yaml
```

### Starting the Server

```bash
cargo run -- serve --config config-http3-example.yaml
```

Expected output:
```
Server listening on 127.0.0.1:3000 (TLS: true, HTTP/2: true, HTTP/3: true, WebSocket: true)
HTTP/3 server listening on UDP 127.0.0.1:3000
```

### Testing HTTP/3 Advertisement

```bash
curl -I --insecure https://127.0.0.1:3000/health
```

Expected response includes:
```
HTTP/2 502 
alt-svc: h3=":443"; ma=3600
```

### Client Testing

- **Chrome/Firefox**: Navigate to `https://127.0.0.1:3000` and check Network tab for "h3" protocol
- **curl**: `curl --http3 --insecure https://127.0.0.1:3000/health`
- **Chrome QUIC monitoring**: `chrome://net-internals/#quic`

## Monitoring and Troubleshooting

### Common Issues

1. **UDP Firewall Blocking**: Ensure UDP port is open for QUIC traffic
2. **Certificate Issues**: Verify TLS certificates are valid with proper ALPN
3. **Client Compatibility**: Check client HTTP/3 support and Alt-Svc header presence
4. **HTTP/0.9 Response Error**: Use HTTPS instead of HTTP for testing

### Debugging Tips

- Check server logs for HTTP/3 events: `[INFO] HTTP/3 server listening on UDP`
- Monitor QUIC connections in Chrome: `chrome://net-internals/#quic`
- Test with telnet for raw responses: `telnet 127.0.0.1 3000`
- Use `curl --http0.9` for debugging malformed responses

## Performance Tuning

### Network Requirements
- **Best for**: High-latency scenarios with >1% packet loss
- **Resource usage**: ~10-20% memory increase, ~2-5x CPU overhead

### Optimization Tips
- **Tune max_data**: Start with 10MB, adjust based on usage
- **Congestion control**: BBR for high-bandwidth, CUBIC for general use
- **Enable 0-RTT**: For performance-critical applications

## Example Production Configuration

```yaml
listen_addr: "0.0.0.0:443"
protocols:
  http2_enabled: true
  websocket_enabled: true
  http3_enabled: true
  http3_config:
    max_data: 50000000
    max_stream_data: 5000000
    max_streams_bidi: 200
    max_idle_timeout: 60000
    congestion_control: "bbr"
    enable_0rtt: true

tls:
  cert_path: "/etc/ssl/certs/server.crt"
  key_path: "/etc/ssl/private/server.key"

routes:
  "/api/": { type: "proxy", target: "http://backend:8080" }
  "/static/": { type: "static", root: "/var/www/static" }

health_check:
  enabled: true
  interval_secs: 30
  path: "/health"
```

## Unit Tests

The implementation includes **19 comprehensive unit tests** covering:

- **Server Tests**: QUIC header parsing, configuration validation
- **Configuration Tests**: Parameter ranges, congestion control algorithms  
- **Handler Tests**: HTTP/3 header conversion, request/response processing
- **Integration Tests**: Component interaction and error handling

Run tests with:
```bash
cargo test http3 --lib  # HTTP/3 specific tests
cargo test              # All tests
```
