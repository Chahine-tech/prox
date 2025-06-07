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

# Stage 2: Final image
FROM debian:bookworm-slim

# Arguments for user and group
ARG APP_USER=proxuser
ARG APP_GROUP=proxgroup
ARG APP_UID=1001
ARG APP_GID=1001

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user and group
RUN groupadd -g ${APP_GID} ${APP_GROUP} \
    && useradd -u ${APP_UID} -g ${APP_GROUP} -s /bin/false -m ${APP_USER}

# Set working directory
WORKDIR /app

# Copy the compiled binary from the builder stage
COPY --from=builder /usr/src/prox/target/release/prox /usr/local/bin/prox

# Copy configuration and static files
COPY config.yaml ./config.yaml
COPY static ./static
COPY certs ./certs

# Ensure the non-root user can access necessary files/directories
# Create directories if they might not exist and set ownership
RUN mkdir -p /app/static /app/certs \
    && chown -R ${APP_USER}:${APP_GROUP} /app \
    && chmod -R 755 /app

# Switch to the non-root user
USER ${APP_USER}:${APP_GROUP}

# Expose the port the application listens on
EXPOSE 8080
EXPOSE 8443

# Command to run the application
ENTRYPOINT ["/usr/local/bin/prox"]
CMD ["--config", "config.yaml"]