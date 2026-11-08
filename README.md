# Destello

Capturador de pantalla para Windows, rápido y sin distracciones: seleccionas un área, la anotas y la copias. **Ctrl+V en una carpeta del Explorador crea el archivo PNG**, como en Linux.

Escrito en Rust sobre Win32, Direct2D y DirectWrite. Un único `.exe` de ~1,3 MB, sin dependencias ni instalador con administrador.

## Descargar e instalar

1. Descarga `Destello-vX.Y.Z-windows-x64.zip` de la [última release](../../releases/latest).
2. Extrae el ZIP y haz doble clic en **`Instalar.cmd`**.

Se instala solo para tu usuario en `%LOCALAPPDATA%\Programs\Destello`, con acceso directo en el menú Inicio, arranque con Windows, el comando `destello` en el PATH y una entrada en *Configuración → Aplicaciones instaladas*, desde donde se desinstala.

Si prefieres no instalar nada, la release incluye también el `.exe` suelto: ejecútalo y aparece en la bandeja del sistema.

> El ejecutable no está firmado: la primera vez Windows SmartScreen puede avisar. Pulsa *Más información → Ejecutar de todas formas*. Cada release publica `SHA256SUMS.txt` para comprobar la descarga.

## Uso

| Atajo | Acción |
|---|---|
| `ImpPnt` | Seleccionar un área (también ventanas, menús y el menú Inicio abiertos) |
| `Ctrl+ImpPnt` | Pantalla completa del monitor del cursor |
| `Shift+ImpPnt` | Todos los monitores |
| `Ctrl+Shift+ImpPnt` | Ventana activa |
| `Ctrl+Alt+ImpPnt` | Repetir la última área |

Dentro de la selección:

- **Herramientas**: mover `V`, rectángulo `R`, elipse `E`, flecha `A`, línea `L`, lápiz `P`, marcador `M`, texto `T`, contador `N`, pixelar `B` y censurar `X`. Lo que dibujas queda recortado al área seleccionada.
- **Copiar** `Enter`: la imagen va al portapapeles como imagen *y* como archivo. Pégala en cualquier aplicación o con `Ctrl+V` en una carpeta.
- **Guardar como…** `Ctrl+S`: elige carpeta y nombre (recuerda la última carpeta).
- **Fijar** `Ctrl+P`: deja la captura flotando encima de todo.
- **Deshacer / rehacer** `Ctrl+Z` / `Ctrl+Y`. **Cerrar** `Esc`.

Destello nunca guarda nada por su cuenta: la captura solo vive en el portapapeles hasta que copias otra cosa, como cualquier texto copiado.

### Línea de comandos

```text
destello                        Arranca en la bandeja del sistema
destello gui [--delay N]        Selección de área
destello full [opciones]        Captura directa de monitor(es)
    --monitor N                 1-based; por defecto, el monitor del cursor
    --all                       Todos los monitores
    --delay N                   Segundos de espera (0-3600)
    --path RUTA                 Guarda en un archivo o carpeta
    --clipboard                 Copia además al portapapeles
destello window [--delay N]     Ventana activa
destello --version | --help
```

## Compilar desde el código

Requisitos: Windows 10/11 x64, [Rust](https://rustup.rs) estable y las *Build Tools* de Visual Studio (MSVC y Windows SDK).

```powershell
cargo test
cargo build --release
powershell -ExecutionPolicy Bypass -File installer\install.ps1   # instala el binario recién compilado
```

## CI/CD

- **Cada PR** pasa el pipeline central [`platform-pipeline`](https://github.com/carlos-anpr/platform-pipeline):
  - **Commits y criterios:** Conventional Commits, y cada criterio `AC-n` tiene su test (`fn acN_…`).
  - **Calidad:** clippy con `-D warnings`, duplicación y complejidad.
  - **Tests:** en Windows.
  - **Guardarraíles y procedencia.**
  - **Seguridad:** gitleaks, Semgrep, OSV-Scanner y zizmor.
- **Cada release publicada** (`vX.Y.Z`) lanza [`release.yml`](.github/workflows/release.yml): compila en Windows, repite lint y tests, comprueba que la versión del `.exe` coincide con la etiqueta, y adjunta el ZIP, el `.exe` y `SHA256SUMS.txt`.
