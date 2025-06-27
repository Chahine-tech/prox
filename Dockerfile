# Stage 1: Builder - Use nightly for edition 2024 support
FROM rustlang/rust:nightly AS builder

# Set working directory
WORKDIR /usr/src/prox

# Install build dependencies (if any, e.g., for specific crates like openssl-sys)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy Cargo.toml and Cargo.lock
COPY Cargo.toml Cargo.lock ./

# Build dependencies first to leverage Docker cache
# Create a dummy lib.rs or main.rs to build only dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo +nightly build --release --locked

# Copy the rest of the source code
COPY src ./src

# Build the application
# Ensure to touch src/main.rs if it was a dummy to trigger rebuild of the main crate
RUN rm -f target/release/prox target/release/deps/prox* # Clean previous build artifacts
RUN cargo +nightly build --release --locked

# Stage 2: Final image - Using distroless for security
FROM gcr.io/distroless/cc-debian12

# Arguments for user and group (distroless already has nonroot user)
ARG APP_USER=nonroot
ARG APP_GROUP=nonroot

# Set working directory
WORKDIR /app

# Copy the compiled binary from the builder stage with executable permissions
COPY --from=builder --chmod=755 /usr/src/prox/target/release/prox ./prox

# Copy configuration and static files
COPY config.yaml ./config.yaml
COPY static ./static
COPY certs ./certs

# Switch to the non-root user (distroless already provides nonroot user)
USER nonroot:nonroot

# Expose the port the application listens on
EXPOSE 8080
EXPOSE 8443

# Command to run the application
ENTRYPOINT ["./prox"]
CMD ["--config", "config.yaml"]