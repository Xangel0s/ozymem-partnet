# Multi-stage Dockerfile for ozymem-server
# Compatible with Coolify, Docker, and docker-compose

# Stage 1: Builder with dependency caching
FROM rust:1.87-slim AS builder

# Install system dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Create dummy source files for dependency caching
RUN mkdir -p crates/ozymem-core/src crates/ozymem-parser/src crates/ozymem-cli/src crates/ozymem-server/src \
    && echo "pub fn dummy() {}" > crates/ozymem-core/src/lib.rs \
    && echo "pub fn dummy() {}" > crates/ozymem-parser/src/lib.rs \
    && echo "fn main() {}" > crates/ozymem-cli/src/main.rs \
    && echo "fn main() {}" > crates/ozymem-server/src/main.rs

# Copy workspace Cargo.toml files for dependency resolution
COPY Cargo.toml ./
COPY crates/ozymem-core/Cargo.toml crates/ozymem-core/
COPY crates/ozymem-parser/Cargo.toml crates/ozymem-parser/
COPY crates/ozymem-cli/Cargo.toml crates/ozymem-cli/
COPY crates/ozymem-server/Cargo.toml crates/ozymem-server/

# Build dependencies only (this layer is cached)
RUN cargo build --release --bin ozymem-server 2>/dev/null || true

# Stage 2: Build actual application
FROM builder AS real-builder

# Copy real source code (overwrites dummy files)
COPY crates/ crates/

# Rebuild with actual source code
RUN cargo build --release --bin ozymem-server

# Stage 3: Runtime
FROM debian:bookworm-slim

# Install runtime dependencies including curl for healthcheck
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libgcc-s1 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN groupadd -r ozymem && useradd -r -g ozymem ozymem

WORKDIR /app

# Copy the binary from builder
COPY --from=real-builder /app/target/release/ozymem-server .

# Verify binary exists and is executable
RUN ls -la /app/ozymem-server

# Set ownership
RUN chown -R ozymem:ozymem /app

USER ozymem

# Environment variables with secure defaults
ENV PORT=8080
ENV OZYMEM_SERVER_MODE=web
ENV MEMGRAPH_URI=memgraph:7687
ENV MEMGRAPH_DATABASE=memgraph
ENV RUST_BACKTRACE=1

EXPOSE 8080

# Health check - increased start-period for slow cold starts
HEALTHCHECK --interval=30s --timeout=10s --start-period=30s --retries=3 \
    CMD curl -f http://localhost:${PORT}/api/ping || exit 1

ENV RUST_LOG=info

CMD echo "=== Ozymem starting ===" && echo "PORT=$PORT MODE=$OZYMEM_SERVER_MODE URI=$MEMGRAPH_URI" && exec ./ozymem-server --web
