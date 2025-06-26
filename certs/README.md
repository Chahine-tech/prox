# Certificates Directory

This directory contains SSL/TLS certificates for the Prox reverse proxy.

## Files

- `cert-placeholder.pem` - Placeholder certificate for CI/CD builds (committed to repo)
- `key-placeholder.pem` - Placeholder private key for CI/CD builds (committed to repo)
- `cert.pem` - Real certificate for local development (ignored by git)
- `key.pem` - Real private key for local development (ignored by git)

## Local Development

For local development with HTTPS, replace the placeholder files:

1. Generate a self-signed certificate:
   ```bash
   openssl req -x509 -newkey rsa:4096 -keyout certs/key.pem -out certs/cert.pem -days 365 -nodes -subj "/CN=localhost"
   ```

2. Or copy your existing certificates:
   ```bash
   cp your-cert.pem certs/cert.pem
   cp your-key.pem certs/key.pem
   ```

## Production Deployment

In production environments:

1. **Kubernetes**: Mount certificates as secrets or volumes
2. **Docker**: Mount certificates as volumes or use secrets management
3. **Cloud**: Use managed certificate services (e.g., AWS ACM, Let's Encrypt)

The placeholder certificates should never be used in production as they are not secure and are publicly available in the repository.
