# Multi-stage Dockerfile for ozymem-server
# Stage 1: Builder with dependency caching
FROM rust:1.75-slim as builder

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
FROM builder as real-builder

# Copy real source code
COPY crates/ crates/

# Remove dummy files and rebuild
RUN rm -f crates/ozymem-core/src/lib.rs crates/ozymem-parser/src/lib.rs crates/ozymem-cli/src/main.rs crates/ozymem-server/src/main.rs \
    && cargo build --release --bin ozymem-server

# Stage 3: Runtime
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN groupadd -r ozymem && useradd -r -g ozymem ozymem

WORKDIR /app

# Copy the binary from builder
COPY --from=real-builder /app/target/release/ozymem-server .

# Set ownership
RUN chown -R ozymem:ozymem /app

USER ozymem

# Environment variables with secure defaults
ENV PORT=8080
ENV OZYMEM_SERVER_MODE=web
ENV MEMGRAPH_URI=memgraph:7687
ENV MEMGRAPH_DATABASE=memgraph

EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:${PORT}/api/health || exit 1

ENTRYPOINT ["./ozymem-server", "--web"]
