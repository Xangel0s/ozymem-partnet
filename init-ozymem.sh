#!/bin/bash
# init-ozymem.sh
# Script de inicialización de Ozymem para Bash en Linux y macOS.
# Automatiza la verificación de Docker, inicio de Memgraph, instalación global de la CLI e indexación inicial.

set -e

echo -e "\033[0;36mIniciando proceso de preparación de Ozymem...\033[0m"

# 1. Verificar prerrequisitos
echo "Verificando dependencias en el sistema..."

# Cargar el path de Cargo si no está en la sesión actual
export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v docker &> /dev/null; then
    echo -e "\033[0;31mError: Docker no está instalado o no se encuentra en el PATH del sistema.\033[0m" >&2
    exit 1
fi

if ! command -v cargo &> /dev/null; then
    echo -e "\033[0;31mError: Rust/Cargo no está instalado o no se encuentra en el PATH del sistema.\033[0m" >&2
    exit 1
fi

# Verificar si Docker Daemon está activo
if ! docker info &> /dev/null; then
    echo -e "\033[0;33mAdvertencia: El daemon de Docker no parece estar ejecutándose.\033[0m"
    echo "Por favor, abre Docker Desktop o inicia el servicio de Docker e intenta ejecutar este script nuevamente."
    exit 1
fi

# 2. Levantar Memgraph (si no está corriendo ya)
echo "Verificando estado de Memgraph..."
containerName="ozymem-memgraph"
running=$(docker ps --filter "name=$containerName" --filter "status=running" --format "{{.Names}}")

if [ -z "$running" ]; then
    echo -e "\033[0;33mMemgraph no está corriendo. Levantando contenedor a través de docker compose...\033[0m"
    docker compose up -d memgraph memgraph-lab
    echo -e "\033[0;32mContenedores iniciados correctamente.\033[0m"
else
    echo -e "\033[0;32mMemgraph ya se encuentra en ejecución.\033[0m"
fi

# 3. Registrar ozymem globalmente
echo "Instalando ozymem-cli globalmente vía cargo install..."
cargo install --path crates/ozymem-cli --force

echo -e "\033[0;32mCLI instalada correctamente. Comando 'ozymem' registrado globalmente.\033[0m"

# 4. Escaneo inicial del monorepo
echo "Ejecutando escaneo inicial del monorepo..."
if ! ozymem scan . --force; then
    echo -e "\033[0;33mAdvertencia: El escaneo inicial devolvió un código de salida no exitoso. Asegúrate de que Memgraph esté listo.\033[0m"
fi

# 5. Mostrar status
echo "Obteniendo estado del sistema..."
ozymem status || true

echo -e "\033[0;32mOzymem inicializado con éxito.\033[0m"
