# Prox: A Lightweight Reverse Proxy with Hexagonal Architecture

Prox is a lightweight reverse proxy built in Rust, implementing a hexagonal architecture (also known as ports and adapters architecture) for maintainability, testability, and flexibility.

## Features

- HTTP/HTTPS support with TLS
- Static file serving
- HTTP redirects
- Load balancing (round-robin and random strategies)
- Path Rewriting for proxy and load-balanced routes
- Health checking for backend services
- Rate limiting (by IP, header, or route-wide) with configurable limits and responses
- **Configuration Validation** with detailed error reporting and CLI validation command
- **Production-grade monitoring** with Prometheus metrics and Grafana dashboards
- Configurable via YAML
- Custom error handling with type safety
- Browser-like request headers for improved compatibility
- Request and Response Manipulation (Headers & Body) with conditional logic.

👉 **See also:** [Value-Adding Ideas and Implementation Status](docs/REVERSE_PROXY_VALUE_ADDITIONS.md)

## Architecture

Prox follows a hexagonal architecture pattern, which separates the application into three main areas:

1. **Core Domain** - Contains the business logic of the application
2. **Ports** - Interfaces that define how the core interacts with the outside world
3. **Adapters** - Implementations of the ports that connect to external systems

```
src/
├── lib.rs                # Library crate definition and re-exports
├── main.rs               # Application entry point 
├── config/               # Configuration handling
│   ├── loader.rs         # Configuration loading logic
│   ├── models.rs         # Configuration data structures with builder pattern
│   ├── validation.rs     # Configuration validation with detailed error reporting
│   └── mod.rs           
├── core/                 # Domain logic
│   ├── proxy.rs          # Core proxy service logic
│   ├── backend.rs        # Backend health tracking
│   ├── load_balancer.rs  # Load balancing strategies
│   ├── rate_limiter.rs   # Rate limiting logic
│   └── mod.rs
├── ports/                # Interfaces
│   ├── http_server.rs    # HTTP server interface
│   ├── http_client.rs    # HTTP client interface with type aliases
│   ├── file_system.rs    # File system interface
│   └── mod.rs
├── adapters/             # Implementations of the ports
│   ├── http/             # HTTP server implementation
│   │   ├── server.rs     # Hyper server implementation
│   │   └── mod.rs
│   ├── acme.rs           # ACME/Let's Encrypt certificate management
│   ├── http_handler.rs   # HTTP request handler
│   ├── http_client.rs    # HTTP client implementation
│   ├── file_system.rs    # Static file handling
│   ├── health_checker.rs # Health checking implementation
│   └── mod.rs
├── utils/                # Utility functions
│   ├── connection_tracker.rs # Connection tracking utilities
│   ├── graceful_shutdown.rs  # Graceful shutdown handling
│   ├── health_checker_utils.rs # Utilities for health checking
│   └── mod.rs
```

### Benefits of Hexagonal Architecture

1. **Testability**: Core domain logic can be tested independently of HTTP, file systems, etc.
2. **Flexibility**: Components can be replaced without affecting the rest of the system
3. **Separation of Concerns**: Clean boundaries between different parts of the application
4. **Domain Focus**: Business logic is isolated from technical details


## Usage

## Usage

### Configuration Validation

Before running the proxy server, you can validate your configuration file to catch any errors:

```bash
# Validate your configuration file
./prox validate --config config.yaml

# Or validate the default config.yaml file
./prox validate
```

**Example validation output (success):**
```bash
$ ./prox validate
🔍 Validating configuration file: config.yaml
✅ YAML parsing: OK
✅ Configuration validation: OK

📋 Configuration Summary:
   • Listen Address: 127.0.0.1:3000
   • Routes: 5
   • TLS Enabled: true
   • Health Checks: true

🎉 Configuration is valid and ready to use!
```

**Example validation output (with errors):**
```bash
$ ./prox validate
🔍 Validating configuration file: config.yaml
✅ YAML parsing: OK
❌ Configuration validation failed:
Found 2 validation error(s):
  1. Invalid URL in field 'route '/api' proxy target': not_a_url - Invalid URL format: relative URL without a base
  2. Invalid listen address: invalid_address - Must be in format 'IP:PORT' (e.g., '127.0.0.1:3000' or '0.0.0.0:8080')

💡 Common fixes:
   • Ensure all URLs start with http:// or https://
   • Check that file paths exist
   • Verify listen address format (e.g., '127.0.0.1:3000')
   • Ensure rate limit periods use valid units (s, m, h)
```

The validation feature checks:
- ✅ Listen address format
- ✅ Route configuration (proxy, load balance, static, redirect)
- ✅ URL validity for all targets
- ✅ Rate limiting configuration
- ✅ TLS certificate and ACME settings
- ✅ File existence for static routes and certificates
- ✅ Route conflict detection

### Starting the Server

### Option 1: Manual TLS Certificates

1. Generate self-signed certificates for HTTPS (or use your own):
   ```bash
   mkdir -p certs
   openssl req -x509 -newkey rsa:4096 -keyout certs/key.pem -out certs/cert.pem -days 365 -nodes -subj '/CN=localhost'
   ```

2. Configure your reverse proxy in `config.yaml` with manual certificates

3. Validate your configuration: `./prox validate --config config.yaml`

4. Run the proxy: `cargo run` or `./prox serve --config config.yaml`

### Option 2: Automatic TLS with Let's Encrypt (ACME)

1. Configure your reverse proxy in `config.yaml` with ACME settings

2. Validate your configuration: `./prox validate --config config.yaml`

3. Ensure your domain points to your server and port 443 is accessible

4. Run the proxy: `cargo run` or `./prox serve --config config.yaml`

**Note**: For ACME to work, your server must be publicly accessible on port 443, and the domains you specify must point to your server for HTTP-01 challenge validation.

## Configuration Examples

All configuration examples below can be validated using `./prox validate` before starting the server.

### Manual TLS Configuration

```yaml
listen_addr: "0.0.0.0:443"
# Manual TLS configuration
tls:
  cert_path: "./certs/cert.pem"
  key_path: "./certs/key.pem"

# Health check configuration
health_check:
  enabled: true
  interval_secs: 10
  timeout_secs: 5
  path: "/health"
  unhealthy_threshold: 3
  healthy_threshold: 2
```

### Automatic TLS with ACME/Let's Encrypt

```yaml
listen_addr: "0.0.0.0:443"
# ACME configuration for automatic certificate management
tls:
  acme:
    enabled: true
    domains:
      - "example.com"
      - "www.example.com"
    email: "admin@example.com"
    staging: false  # Set to true for testing
    storage_path: "./acme_storage"
    renewal_days_before_expiry: 30

# Health check configuration
health_check:
  enabled: true
  interval_secs: 10
  timeout_secs: 5
  path: "/health"
  unhealthy_threshold: 3
  healthy_threshold: 2

# Backend-specific health check paths
backend_health_paths:
  "https://httpbin.org": "/get"
  "https://postman-echo.com": "/get"

routes:
  "/":  # Root route that redirects to /static
    type: "redirect"
    target: "/static"
    status_code: 302
  "/static":
    type: "static"
    root: "./static"
  "/redirect":
    type: "redirect"
    target: "https://www.example.com"
    status_code: 302
  "/proxy":
    type: "proxy"
    target: "https://httpbin.org"
    path_rewrite: "/anything" # Example: /proxy/foo rewrites to /anything/foo
    rate_limit: # Example: Limit by IP, 10 requests per minute
      by: "ip"
      requests: 10
      period: "1m"
      # status_code: 429 # Optional: defaults to 429
      # message: "Too many requests from your IP. Please try again later." # Optional: defaults to "Too Many Requests"
      # algorithm: "token_bucket" # Optional: defaults to token_bucket (current default)
    request_headers:
      add:
        "X-My-Custom-Header": "MyValue"
        "X-Forwarded-By": "Prox"
        "X-Real-IP": "{client_ip}"
      remove: ["User-Agent", "Referer"]
    response_headers:
      add:
        "Server": "Prox"
      remove: ["X-Powered-By"]
    request_body: # Example: Modify the request body
      condition: # Only apply if the request path contains "/special"
        path_matches: "/special"
      set_text: "This is a modified request body."
    response_body: # Example: Modify the response body if it's a 404
      condition:
        # Assuming your service might return a specific header for identifiable errors
        # or you might match on status code if that becomes a condition option.
        # For now, let's imagine a header condition.
        has_header:
          name: "X-Error-Type"
          value_matches: "NotFound"
      set_json:
        error: "Resource not found"
        message: "The requested resource was not found on the server."
        status: 404
  "/api/v1": # Example for API versioning
    type: "proxy"
    target: "http://internal-service"
    path_rewrite: "/" # Example: /api/v1/users rewrites to /users
    rate_limit: # Example: Limit by a specific header X-API-Key, 5 requests per 30 seconds
      by: "header"
      header_name: "X-API-Key"
      requests: 5
      period: "30s"
      status_code: 403 # Custom status code
      message: "Rate limit exceeded for your API key." # Custom message
  "/balance":
    type: "load_balance"
    targets:
      - "https://httpbin.org"
      - "https://postman-echo.com" 
    strategy: "round_robin"
    path_rewrite: "/anything" # Example: /balance/bar rewrites to /anything/bar
    rate_limit: # Example: Route-wide limit, 1000 requests per hour
      by: "route"
      requests: 1000
      period: "1h"
```

## ACME Configuration Options

When using automatic TLS certificate management with ACME (Let's Encrypt), you can configure the following options:

- **`enabled`**: Set to `true` to enable ACME certificate management
- **`domains`**: List of domains to include in the certificate (first domain is the primary)
- **`email`**: Contact email for Let's Encrypt account registration
- **`staging`**: Set to `true` to use Let's Encrypt staging environment for testing (optional, defaults to `false`)
- **`ca_url`**: Custom ACME CA URL (optional, defaults to Let's Encrypt production)
- **`storage_path`**: Directory to store certificates and account data (optional, defaults to `./acme_storage`)
- **`renewal_days_before_expiry`**: Days before expiry to renew certificates (optional, defaults to 30)

### ACME Requirements

1. **Public accessibility**: Your server must be publicly accessible on port 443
2. **Domain DNS**: All domains in your configuration must point to your server's IP address
3. **HTTP-01 challenge**: Prox automatically handles HTTP-01 challenges by serving files from `./static/.well-known/acme-challenge/`

### ACME Certificate Renewal

- Certificates are automatically checked daily for renewal
- Renewal occurs when the certificate expires within the configured threshold (default: 30 days)
- The renewal process runs in the background without interrupting service

## Testing

You can test the proxy using curl:

```bash
# Test static content
curl -k https://127.0.0.1:3000/static

# Test redirection
curl -k -L https://127.0.0.1:3000/

# Test proxy
curl -k https://127.0.0.1:3000/proxy/get

# Test proxy with path rewriting
curl -k https://127.0.0.1:3000/proxy/test/path # Assuming /proxy has path_rewrite: "/anything"
# Expected: httpbin.org receives a request for /anything/test/path

# Test request header addition (X-My-Custom-Header: MyValue)
curl -k -H "X-My-Custom-Header: OriginalValue" https://127.0.0.1:3000/proxy/headers -v
# Expected: Prox adds/overwrites X-My-Custom-Header, removes User-Agent, adds X-Real-IP

# Test request body modification (POST to /proxy/special/anything)
# This will trigger the request_body.set_text action due to path_matches: "/special"
curl -k -X POST -d '{"original": "data"}' https://127.0.0.1:3000/proxy/special/post -H "Content-Type: application/json"
# Expected: httpbin.org receives "This is a modified request body."

# Test response header removal (X-Powered-By) and addition (Server: Prox)
curl -k -I https://127.0.0.1:3000/proxy/get
# Expected: X-Powered-By header (if present from httpbin) is removed, Server: Prox is added.

# Test load balancing (run multiple times to see round-robin in action)
curl -k https://127.0.0.1:3000/balance/get
```

Note: The `-k` flag is used to skip certificate validation for self-signed certificates.

## CLI Commands

Prox supports several CLI commands for different operations:

### Server Commands

```bash
# Start the proxy server (default behavior)
./prox serve --config config.yaml
./prox serve  # Uses default config.yaml

# Legacy format (still supported)
./prox --config config.yaml
```

### Configuration Validation

```bash
# Validate a specific configuration file
./prox validate --config config.yaml

# Validate the default config.yaml file
./prox validate

# Exit codes:
# 0 = Configuration is valid
# 1 = Configuration has errors or file not found
```

**Benefits of configuration validation:**
- 🔍 **Early Error Detection**: Catch configuration issues before deployment
- 🚀 **CI/CD Integration**: Validate configs in automated pipelines  
- 📝 **Self-Documenting**: Clear error messages explain requirements
- 🛡️ **Production Safety**: Prevent server startup with invalid configuration

## Monitoring & Observability

Prox includes built-in Prometheus metrics and can be easily monitored with a complete Grafana dashboard setup.

### Metrics Endpoint

Prox exposes metrics at `https://localhost:3000/metrics` including:
- Request rates and response times
- Active connections and backend health
- Error rates and status code distributions
- Rate limiting statistics

### Complete Monitoring Stack

Set up production-grade monitoring with Prometheus and Grafana:

📊 **[Complete Monitoring Stack Setup Guide](docs/MONITORING_STACK_SETUP.md)**

This guide includes:
- Docker Compose setup for Prometheus and Grafana
- Pre-configured dashboards and panels
- Alerting rules and best practices
- Troubleshooting and performance tuning
- Production deployment considerations

**Quick Start:**
```bash
# Start monitoring stack
docker-compose up -d

# Start Prox with metrics
cargo run

# Access Grafana dashboards
open http://localhost:3001  # admin/admin
```

### Available Metrics

Key metrics exposed by Prox:
- `prox_requests_total` - Total number of requests by endpoint, method, and status
- `prox_request_duration_seconds` - Request duration histogram
- `prox_active_connections` - Current active connections
- `prox_backend_health_status` - Backend server health status
- `prox_rate_limit_hits_total` - Rate limiting statistics

## License

[MIT License](LICENSE)