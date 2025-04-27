# Prox: A Lightweight Reverse Proxy with Hexagonal Architecture

Prox is a lightweight reverse proxy built in Rust, implementing a hexagonal architecture (also known as ports and adapters architecture) for maintainability, testability, and flexibility.

## Features

- HTTP/HTTPS support with TLS
- Static file serving
- HTTP redirects
- Load balancing (round-robin and random strategies)
- Health checking for backend services
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
│   └── mod.rs
├── ports/                # Interfaces
│   ├── http_server.rs    # HTTP server interface
│   ├── http_client.rs    # HTTP client interface with type aliases
│   ├── file_system.rs    # File system interface
│   └── mod.rs
└── adapters/             # Implementations of the ports
    ├── http/             # HTTP server implementation
    │   ├── server.rs     # Hyper server implementation
    │   └── mod.rs
    ├── http_handler.rs   # HTTP request handler
    ├── http_client.rs    # HTTP client implementation
    ├── file_system.rs    # Static file handling
    ├── health_checker.rs # Health checking implementation
    └── mod.rs
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
  "/balance":
    type: "load_balance"
    targets:
      - "https://httpbin.org"
      - "https://postman-echo.com" 
    strategy: "round_robin"
```

## Testing

You can test the proxy using curl:

```bash
# Test static content
curl -k https://127.0.0.1:3002/static

# Test redirection
curl -k -L https://127.0.0.1:3002/

# Test proxy
curl -k https://127.0.0.1:3002/proxy/get

# Test load balancing (run multiple times to see round-robin in action)
curl -k https://127.0.0.1:3002/balance/get
```

Note: The `-k` flag is used to skip certificate validation for self-signed certificates.

## License

[MIT License](LICENSE)