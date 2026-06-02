# init-ozymem.ps1
# Script de inicialización de Ozymem para PowerShell en Windows.
# Automatiza la verificación de Docker, inicio de Memgraph, instalación global de la CLI e indexación inicial.

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Write-Host "Iniciando proceso de preparacion de Ozymem..." -ForegroundColor Cyan

# 1. Verificar prerrequisitos
Write-Host "Verificando dependencias en el sistema..." -ForegroundColor Gray

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Write-Error "Docker no esta instalado o no se encuentra en el PATH del sistema."
    Exit 1
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Rust/Cargo no esta instalado o no se encuentra en el PATH del sistema."
    Exit 1
}

# Verificar si Docker Desktop / Daemon esta activo
try {
    & docker info --format '{{.ID}}' *>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Docker Daemon no esta respondiendo."
    }
} catch {
    Write-Warning "Docker Desktop no parece estar ejecutandose o el servicio de Docker esta detenido."
    Write-Host "Por favor, abre Docker Desktop e intenta ejecutar este script nuevamente." -ForegroundColor Yellow
    Exit 1
}

# 2. Levantar Memgraph (si no esta corriendo ya)
Write-Host "Verificando estado de Memgraph..." -ForegroundColor Gray
$containerName = "ozymem-memgraph"
$running = & docker ps --filter "name=$containerName" --filter "status=running" --format "{{.Names}}"

if (-not $running) {
    Write-Host "Memgraph no esta corriendo. Levantando contenedor a traves de docker compose..." -ForegroundColor Yellow
    & docker compose up -d memgraph memgraph-lab
    if ($LASTEXITCODE -ne 0) {
        Write-Error "No se pudo iniciar docker compose para Memgraph."
        Exit 1
    }
    Write-Host "Contenedores iniciados correctamente." -ForegroundColor Green
} else {
    Write-Host "Memgraph ya se encuentra en ejecucion." -ForegroundColor Green
}

# 3. Registrar ozymem globalmente
Write-Host "Instalando ozymem-cli globalmente via cargo install..." -ForegroundColor Gray
& cargo install --path crates/ozymem-cli --force
if ($LASTEXITCODE -ne 0) {
    Write-Error "Fallo la instalacion de ozymem-cli."
    Exit 1
}
Write-Host "CLI instalada correctamente. Comando 'ozymem' registrado globalmente." -ForegroundColor Green

# Recargar variables de entorno del PATH de cargo si es necesario
$CargoBinPath = Join-Path $env:USERPROFILE ".cargo\bin"
if ($env:PATH -notlike "*$CargoBinPath*") {
    $env:PATH = "$env:PATH;$CargoBinPath"
}

$ozymemExe = Join-Path $CargoBinPath "ozymem.exe"

# 4. Escaneo inicial del monorepo
Write-Host "Ejecutando escaneo inicial del monorepo..." -ForegroundColor Gray
& $ozymemExe scan .
if ($LASTEXITCODE -ne 0) {
    Write-Warning "El escaneo inicial devolvio un codigo de salida no exitoso. Asegúrate de que Memgraph este listo."
}

# 5. Mostrar status
Write-Host "Obteniendo estado del sistema..." -ForegroundColor Gray
& $ozymemExe status

Write-Host "Ozymem inicializado con exito." -ForegroundColor Green

