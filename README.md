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

1. Generate self-signed certificates for HTTPS (or use your own):
   ```bash
   mkdir -p certs
   openssl req -x509 -newkey rsa:4096 -keyout certs/key.pem -out certs/cert.pem -days 365 -nodes -subj '/CN=localhost'
   ```

2. Configure your reverse proxy in `config.yaml`

3. Run the proxy: `cargo run`

## Configuration Example

```yaml
listen_addr: "127.0.0.1:3002"
# TLS configuration
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

# Test load balancing (run multiple times to see round-robin in action)
curl -k https://127.0.0.1:3000/balance/get
```

Note: The `-k` flag is used to skip certificate validation for self-signed certificates.

## License

[MIT License](LICENSE)