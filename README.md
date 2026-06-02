# Ozymem

Ozymem es un motor de análisis arquitectónico políglota y un servidor de engramas de memoria basado en grafos de conocimiento. Utiliza Memgraph como base de datos para almacenar y correlacionar relaciones de dependencia, estructuras de funciones, y lecciones aprendidas durante los procesos de desarrollo y refactorización.

## Requisitos previos

Para poder ejecutar e instalar Ozymem localmente se requiere contar con:

- Rust (cargo, rustc versión estable reciente)
- Docker y Docker Desktop (con soporte para `docker compose`)

## Instalación rápida (Windows)

En la raíz del monorepo, abre PowerShell y ejecuta el script de inicialización:

```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force
.\init-ozymem.ps1
```

Este script automatizará las siguientes tareas:
1. Comprobar que Docker y Cargo estén disponibles y que Docker Desktop esté en ejecución.
2. Iniciar el contenedor de Memgraph (junto con Memgraph Lab) si no están activos.
3. Compilar e instalar la herramienta `ozymem` globalmente en el sistema.
4. Indexar el propio monorepo por primera vez.
5. Imprimir el estado actual del sistema Ozymem.

## Uso de la CLI

Una vez instalado, tienes a tu disposición el comando global `ozymem`. A continuación se detallan los subcomandos principales:

### Ver estado
Para comprobar la conectividad con la base de datos de grafos y las métricas básicas de entidades indexadas:
```bash
ozymem status
```

### Indexar código
Para indexar de forma recursiva un directorio y registrar sus archivos, funciones y dependencias internas:
```bash
ozymem scan <directorio>
```
Si deseas vaciar el grafo por completo antes del análisis, puedes agregar la bandera `--reset`:
```bash
ozymem scan <directorio> --reset
```

### Consultar lecciones históricas
Para visualizar las lecciones guardadas a partir de errores y soluciones registrados en el sistema:
```bash
ozymem lessons --limit 10
```

### Visualizar el árbol de dependencias
Para representar la jerarquía y las funciones asociadas de un archivo específico de manera visual en forma de árbol:
```bash
ozymem tree <ruta_al_archivo> --depth 2
```

## Servidor MCP

Para iniciar el servidor del Protocolo de Contexto de Modelos (MCP) y permitir la comunicación directa con IDEs (como Cursor o Claude Desktop):

```bash
cargo run -p ozymem-server
```

Alternativamente, en entornos Windows, se puede usar el wrapper preconfigurado:
```powershell
.\start-ozymem.ps1
```
