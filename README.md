# Ozymem

Monorepo Rust para el motor de análisis y el servidor MCP de Ozymem.

## Estructura inicial

- `crates/ozymem-core`: acceso a Memgraph y utilidades del dominio
- `crates/ozymem-parser`: extracción sintáctica semántica
- `crates/ozymem-cli`: indexación local desde terminal
- `crates/ozymem-server`: servidor MCP local

## Arranque local

1. Levanta Memgraph y Memgraph Lab:

```bash
docker compose up -d
```

2. Prueba la conexión básica desde `ozymem-core`:

```bash
cargo run -p ozymem-core --bin ping-memgraph
```

3. Ajusta `MEMGRAPH_URI`, `MEMGRAPH_USER`, `MEMGRAPH_PASSWORD` y `MEMGRAPH_DATABASE` si cambias la configuración por defecto.


## CLI

Escanear un directorio con el recolector:

```bash
cargo run -p ozymem-cli -- scan --dir .
```

Para vaciar el grafo antes de reindexar:

```bash
cargo run -p ozymem-cli -- scan --dir . --reset
```

La CLI indexa archivos `.py`, `.go`, `.js`, `.ts`, `.tsx`, `.jsx` y `.sql`, y degrada el resto a `Unknown` con fallback textual sin ignorarlos.

## MCP Server

Levantar el servidor MCP local:

```bash
cargo run -p ozymem-server
```

El servidor expone dos tools de lectura por stdio:

- `file_context`: devuelve el lenguaje, la estrategia y las funciones indexadas de un archivo.
- `graph_summary`: devuelve un resumen global del grafo indexado.

### Wrapper de arranque para Windows

Si vas a conectarlo desde Cursor o Claude Desktop, usa el wrapper `start-ozymem.ps1` para:

1. levantar Memgraph con Docker,
2. esperar a que el puerto `7687` esté disponible,
3. ejecutar el servidor MCP ya compilado.

Ejemplo de configuración:

```json
{
  "command": "powershell.exe",
  "args": [
    "-NoLogo",
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    "C:\\Users\\Lenovo\\Documents\\ozymem\\start-ozymem.ps1"
  ]
}
```
