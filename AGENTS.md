# Ozymem Developer Agent Guidelines

## Arquitectura y Monorepo

- `crates/ozymem-core`: Capa de base de datos Memgraph, IAM, sesiones, GPR, utilidades del sistema de archivos.
- `crates/ozymem-parser`: Parsers de codigo fuente (Python, Go, Rust, JS/TS) usando Tree-sitter con fallback heuristico.
- `crates/ozymem-cli`: CLI principal con ~20 subcomandos, watcher, WAL, MCP client.
- `crates/ozymem-server`: Servidor dual-mode (MCP stdio / Axum HTTP API) con autenticacion.

## Principios y Convenciones

- **SOLID, DRY, KISS**: Acoplamiento minimo, logica reutilizable, sin sobredisenio.
- **Git**: Commits convencionales (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`).
- **Testing**: Cobertura >80% en funcionalidades nuevas. Siempre ejecutar:
  ```bash
  cargo test --workspace
  cargo clippy --workspace -- -D warnings
  ```
- **Cambios**: Mostrar diff antes de editar archivos existentes.

## Seguridad

- **Nunca hardcodear credenciales**: Usar variables de entorno `MEMGRAPH_USER` y `MEMGRAPH_PASSWORD`.
- **Queries parametrizadas**: Siempre usar `.param()` en Cypher, nunca interpolacion de strings.
- **Tokens**: SIEMPRE con salt (formato `salt:hash`). No implementar fallback sin salt.
- **Rate limiting**: El servidor aplica rate limiting por IP (100 req/60s).

## Estructura de Tests

- Unit tests: Dentro de `#[cfg(test)] mod tests` en cada modulo.
- Integration tests: En `crates/<name>/tests/integration_test.rs`.
- Tests de parser: Cubrir Python, Go, Rust, JavaScript con casos reales.

## CI/CD

El pipeline `.github/workflows/ci.yml` ejecuta:
1. `cargo fmt --check` - Formateo
2. `cargo clippy --workspace -- -D warnings` - Lint
3. `cargo audit` - Seguridad de dependencias
4. `cargo build/test` en ubuntu, windows, macos
5. `cargo tarpaulin` - Coverage
6. Docker build (solo en main)

## Infraestructura Docker

- `Dockerfile`: Multi-stage build con dependency caching y usuario no-root.
- `docker-compose.yml`: Desarrollo con Memgraph + Lab.
- `docker-compose.prod.yml`: Produccion con health checks, restart, resource limits.

## Comandos Utiles

```bash
cargo check --workspace          # Verificar compilacion
cargo test --workspace           # Ejecutar tests
cargo clippy --workspace -- -D warnings  # Lint estricto
cargo fmt --check                # Verificar formateo
cargo audit                      # Auditoria de seguridad
```
