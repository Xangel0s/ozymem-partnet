# Ozymem-Partner

Ozymem-Partner es un motor de analisis arquitectonico poliglota y un servidor de engramas de memoria basado en grafos de conocimiento, disenado para el trabajo colaborativo en equipos de desarrollo. Utiliza una arquitectura unificada que conecta multiples terminales a un cerebro centralizado.

## Caracteristicas

- **Arquitectura Colaborativa**: La CLI delega la persistencia del mapa de dependencias, definicion de archivos y registro de lecciones a traves de APIs HTTP seguras.
- **Cerebro Compartido**: Las lecciones aprendidas y soluciones de errores aplicadas por un desarrollador estan disponibles de forma instantanea para el resto del equipo via MCP.
- **Multiplataforma Nativo**: Soporte para Windows (PowerShell), Linux y macOS (Bash).
- **Modo Offline/Local**: Conexion por defecto via Bolt a una base de datos Memgraph local.
- **Seguridad**: Autenticacion por token, rate limiting por IP, body size limits, credenciales no hardcoded.

## Requisitos

- Rust 1.75.0+ (cargo, rustc)
- Docker (para Memgraph local o despliegue en produccion)

## Instalacion Rapida

### Windows (PowerShell)
```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force
.\init-ozymem.ps1
```

### Linux / macOS (Bash)
```bash
chmod +x init-ozymem.sh
./init-ozymem.sh
```

## Variables de Entorno

| Variable | Requerida | Default | Descripcion |
|----------|-----------|---------|-------------|
| `MEMGRAPH_USER` | Si | - | Usuario de Memgraph |
| `MEMGRAPH_PASSWORD` | Si | - | Password de Memgraph |
| `MEMGRAPH_URI` | No | `127.0.0.1:7687` | URI de conexion a Memgraph |
| `MEMGRAPH_DATABASE` | No | `memgraph` | Nombre de la base de datos |
| `PORT` | No | `8080` | Puerto del servidor HTTP |
| `OZYMEM_SERVER_MODE` | No | `web` | Modo del servidor (`web` o `stdio`) |

Ver `.env.example` para la lista completa.

## Comandos CLI

| Comando | Descripcion |
|---------|-------------|
| `ozymem scan <dir>` | Escanea un directorio e indexa archivos |
| `ozymem status` | Muestra estado de watchers y metricas del grafo |
| `ozymem lessons --limit N` | Lista lecciones aprendidas |
| `ozymem tree <archivo> --depth N` | Arbol de dependencias |
| `ozymem trace <archivo>` | Analisis de impacto reverso |
| `ozymem watch <dir>` | Monitoreo continuo de cambios |
| `ozymem doctor` | Diagnostico del entorno |
| `ozymem gpr push/list/diff/merge` | Graph Pull Requests |
| `ozymem team create` | Gestion de usuarios |
| `ozymem session list/kick` | Gestion de sesiones |

## Docker

### Desarrollo
```bash
docker compose up -d
```

### Produccion
```bash
# Copiar .env.example a .env y configurar credenciales
cp .env.example .env
docker compose -f docker-compose.prod.yml up -d
```

## Arquitectura

```
crates/
  ozymem-core/     # Capa de base de datos (Memgraph) y logica central
  ozymem-parser/   # Parsers multi-lenguaje (Tree-sitter)
  ozymem-cli/      # CLI principal
  ozymem-server/   # Servidor MCP stdio y HTTP API
```

## Seguridad

- **Sin credenciales hardcoded**: Las variables `MEMGRAPH_USER` y `MEMGRAPH_PASSWORD` son obligatorias.
- **Rate limiting por IP**: 100 requests por 60 segundos por cliente.
- **Body size limit**: Maximo 10MB por request.
- **Tokens con salt**: SHA-256 con salt aleatorio, sin fallback legacy.
- **Health check**: Endpoint `/api/health` para monitoreo.

## Testing

```bash
cargo test --workspace     # Ejecutar todos los tests
cargo clippy --workspace   # Verificar calidad de codigo
```

## Licencia

MIT
