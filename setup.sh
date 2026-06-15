#!/bin/bash
# Ozymem Partner - Setup & Deploy
# Run: bash setup.sh

set -e

echo "=== Ozymem Partner Setup ==="

# Build ozymem-server image
echo "[1/3] Building ozymem-server image..."
docker build -t ozymem-server:latest .

# Create .env if not exists
if [ ! -f .env ]; then
    echo "[2/3] Creating .env file..."
    cat > .env << 'EOF'
MEMGRAPH_USER=Adminsito
MEMGRAPH_PASSWORD=Lunalopez2077@
MEMGRAPH_DATABASE=memgraph
EOF
    echo "  -> .env created. Edit it with your credentials."
else
    echo "[2/3] .env already exists."
fi

# Start all services
echo "[3/3] Starting services..."
docker compose -f docker-compose.prod.yml up -d

echo ""
echo "=== Done! ==="
echo "Server:      http://localhost:5857"
echo "Memgraph Lab: http://localhost:7474"
echo "Bolt:        bolt://localhost:7687"
