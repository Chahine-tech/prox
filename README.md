# Prox: A Lightweight Reverse Proxy with Hexagonal Architecture

Prox is a lightweight reverse proxy built in Rust, implementing a hexagonal architecture (also known as ports and adapters architecture) for maintainability, testability, and flexibility.

## Features

- HTTP/HTTPS support with TLS
- Static file serving
- HTTP redirects
- Load balancing (round-robin and random strategies)
- Health checking for backend services
- Configurable via YAML

## Architecture

Prox follows a hexagonal architecture pattern, which separates the application into three main areas:

1. **Core Domain** - Contains the business logic of the application
2. **Ports** - Interfaces that define how the core interacts with the outside world
3. **Adapters** - Implementations of the ports that connect to external systems

```
src/
├── main.rs               # Application entry point 
├── config/               # Configuration handling
│   ├── loader.rs         # Configuration loading logic
│   ├── models.rs         # Configuration data structures
│   └── mod.rs           
├── core/                 # Domain logic
│   ├── proxy.rs          # Core proxy service logic
│   ├── backend.rs        # Backend health tracking
│   └── mod.rs
├── ports/                # Interfaces
│   ├── http_server.rs    # HTTP server interface
│   ├── http_client.rs    # HTTP client interface
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

1. Configure your reverse proxy in `config.yaml`
2. Run the proxy: `cargo run`

## Configuration Example

```yaml
listen_addr: "127.0.0.1:8080"
routes:
  "/static":
    type: "static"
    root: "./static"
  "/api":
    type: "proxy"
    target: "http://localhost:3000"
  "/app":
    type: "load_balance"
    strategy: "round_robin"
    targets:
      - "http://localhost:3001"
      - "http://localhost:3002"
  "/old":
    type: "redirect"
    target: "/new"
    status_code: 301
health_check:
  enabled: true
  interval_secs: 10
  timeout_secs: 2
  path: "/health"
  unhealthy_threshold: 3
  healthy_threshold: 2
backend_health_paths:
  "http://localhost:3001": "/custom-health"
```

## License

[MIT License](LICENSE)