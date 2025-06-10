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
│   ├── http_handler.rs   # HTTP request handler
│   ├── http_client.rs    # HTTP client implementation
│   ├── file_system.rs    # Static file handling
│   ├── health_checker.rs # Health checking implementation
│   └── mod.rs
├── utils/                # Utility functions
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

### Option 1: Manual TLS Certificates

1. Generate self-signed certificates for HTTPS (or use your own):
   ```bash
   mkdir -p certs
   openssl req -x509 -newkey rsa:4096 -keyout certs/key.pem -out certs/cert.pem -days 365 -nodes -subj '/CN=localhost'
   ```

2. Configure your reverse proxy in `config.yaml` with manual certificates

3. Run the proxy: `cargo run`

### Option 2: Automatic TLS with Let's Encrypt (ACME)

1. Configure your reverse proxy in `config.yaml` with ACME settings

2. Ensure your domain points to your server and port 443 is accessible

3. Run the proxy: `cargo run`

**Note**: For ACME to work, your server must be publicly accessible on port 443, and the domains you specify must point to your server for HTTP-01 challenge validation.

## Configuration Examples

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

## License

[MIT License](LICENSE)