param(
    [string]$ProjectRoot = $PSScriptRoot,
    [string]$MemgraphHost = "127.0.0.1",
    [int]$MemgraphPort = 7687,
    [int]$MemgraphWaitSeconds = 60
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Test-TcpPortOpen {
    param(
        [Parameter(Mandatory = $true)][string]$Host,
        [Parameter(Mandatory = $true)][int]$Port,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)

    while ((Get-Date) -lt $deadline) {
        try {
            $client = [System.Net.Sockets.TcpClient]::new()
            $iar = $client.BeginConnect($Host, $Port, $null, $null)

            if ($iar.AsyncWaitHandle.WaitOne(1000, $false)) {
                $client.EndConnect($iar)
                $client.Close()
                return $true
            }

            $client.Close()
        }
        catch {
            if ($client) {
                $client.Close()
            }
        }

        Start-Sleep -Milliseconds 500
    }

    return $false
}

Set-Location $ProjectRoot

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    throw "Docker no está disponible en PATH."
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "Cargo no está disponible en PATH."
}

if (-not $env:MEMGRAPH_URI) {
    $env:MEMGRAPH_URI = "$MemgraphHost`:$MemgraphPort"
}

if (-not $env:MEMGRAPH_USER) {
    $env:MEMGRAPH_USER = "admin"
}

if (-not $env:MEMGRAPH_PASSWORD) {
    $env:MEMGRAPH_PASSWORD = "admin"
}

if (-not $env:MEMGRAPH_DATABASE) {
    $env:MEMGRAPH_DATABASE = "memgraph"
}

& docker compose up -d *>$null
if ($LASTEXITCODE -ne 0) {
    throw "No se pudo levantar docker compose."
}

if (-not (Test-TcpPortOpen -Host $MemgraphHost -Port $MemgraphPort -TimeoutSeconds $MemgraphWaitSeconds)) {
    throw "Memgraph no respondió en ${MemgraphHost}:${MemgraphPort} dentro de ${MemgraphWaitSeconds}s."
}

$serverExe = Join-Path $ProjectRoot "target\debug\ozymem-server.exe"
if (-not (Test-Path $serverExe)) {
    & cargo build -p ozymem-server --quiet 1>$null 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Falló la compilación de ozymem-server."
    }
}

& $serverExe
