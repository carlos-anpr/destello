//! Parseo de argumentos de línea de comandos. Sin dependencias de UI.
use std::fmt;
use std::path::PathBuf;

use crate::app::{Command, MonitorSelection};
use crate::output::DeliveryPlan;

pub const HELP: &str = "\
Destello — capturador de pantalla para Windows

Uso:
  destello                        Arranca en la bandeja del sistema
  destello gui [--delay N]        Overlay de selección de área
  destello full [opciones]        Captura directa de monitor(es)
      --monitor N                 1-based; por defecto, el monitor del cursor
      --all                       Todos los monitores
      --delay N                   Segundos de espera (0-3600)
      --path RUTA                 Guarda en un archivo o carpeta (explícito)
      --clipboard                 Copia además al portapapeles
  destello window [--delay N]     Ventana activa
  destello --version | --help

Las capturas directas solo se copian: Ctrl+V en una carpeta crea el PNG.
--path es la única forma de guardar en disco. --no-save está obsoleto y se ignora.

Si Destello ya está en ejecución, la orden se le reenvía (WM_COPYDATA).
";

const NO_SAVE_WARNING: &str =
    "--no-save está obsoleto y se ignora: las capturas ya no se guardan automáticamente";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Arrancar (o no hacer nada si ya hay instancia) sin capturar.
    StartTray,
    Command(Command),
    /// Diagnóstico oculto: vuelca todas las ventanas top-level y su veredicto.
    DebugTargets { delay: u32 },
    Help,
    Version,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    pub message: String,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// Parsea una lista de argumentos (sin el nombre del ejecutable).
///
/// Descarta los avisos: el proceso que invoca debe usar
/// `parse_args_with_warnings` para imprimirlos antes de reenviar la orden.
pub fn parse_args(args: &[String]) -> Result<Parsed, CliError> {
    parse_args_with_warnings(args).map(|(parsed, _warnings)| parsed)
}

/// Parsea el payload de WM_COPYDATA: argumentos separados por '\n'.
///
/// La instancia residente no imprime avisos: ya los mostró quien invocó.
pub fn parse_line(line: &str) -> Result<Parsed, CliError> {
    let args: Vec<String> = line
        .split('\n')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    parse_args(&args)
}

pub(crate) fn parse_args_with_warnings(
    args: &[String],
) -> Result<(Parsed, Vec<String>), CliError> {
    if args.is_empty() {
        return Ok((Parsed::StartTray, Vec::new()));
    }
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "/?"))
    {
        return Ok((Parsed::Help, Vec::new()));
    }
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--version" | "-V" | "-v"))
    {
        return Ok((Parsed::Version, Vec::new()));
    }

    let command = args[0].to_ascii_lowercase();
    let rest = &args[1..];
    let parsed = match command.as_str() {
        "gui" | "area" | "capturar" => {
            let opts = parse_options(rest, &[Opt::Delay, Opt::NoSave])?;
            let warnings = warnings_for(&opts);
            return Ok((
                Parsed::Command(Command::CaptureArea {
                    delay: opts.delay,
                }),
                warnings,
            ));
        }
        "full" | "pantalla" => {
            let opts = parse_options(
                rest,
                &[
                    Opt::Delay,
                    Opt::Monitor,
                    Opt::All,
                    Opt::Path,
                    Opt::Clipboard,
                    Opt::NoSave,
                ],
            )?;
            if opts.all && opts.monitor.is_some() {
                return Err(CliError::new("--all y --monitor no se pueden combinar"));
            }
            let selection = if opts.all {
                MonitorSelection::All
            } else if let Some(index) = opts.monitor {
                MonitorSelection::Index(index)
            } else {
                MonitorSelection::Cursor
            };
            let warnings = warnings_for(&opts);
            let plan = OutputRequest {
                path: opts.path,
                clipboard: opts.clipboard,
            }
            .plan();
            return Ok((
                Parsed::Command(Command::CaptureMonitors {
                    selection,
                    delay: opts.delay,
                    plan,
                }),
                warnings,
            ));
        }
        "window" | "ventana" => {
            let opts = parse_options(rest, &[Opt::Delay, Opt::NoSave])?;
            let warnings = warnings_for(&opts);
            return Ok((
                Parsed::Command(Command::CaptureWindow {
                    delay: opts.delay,
                    plan: DeliveryPlan::copy_only(),
                }),
                warnings,
            ));
        }
        // Diagnóstico oculto (no aparece en --help).
        "debug-targets" | "debug_targets" => {
            let opts = parse_options(rest, &[Opt::Delay])?;
            return Ok((
                Parsed::DebugTargets { delay: opts.delay },
                Vec::new(),
            ));
        }
        "settings" | "ajustes" => {
            parse_options(rest, &[])?;
            Parsed::Command(Command::OpenSettings)
        }
        "quit" | "salir" | "exit" => {
            parse_options(rest, &[])?;
            Parsed::Command(Command::Quit)
        }
        "help" => Parsed::Help,
        "version" => Parsed::Version,
        other if other.starts_with('-') => {
            return Err(CliError::new(format!(
                "opción desconocida: {other} (prueba --help)"
            )))
        }
        other => {
            return Err(CliError::new(format!(
                "comando desconocido: {other} (prueba --help)"
            )))
        }
    };
    Ok((parsed, Vec::new()))
}

fn warnings_for(opts: &RawOptions) -> Vec<String> {
    if opts.no_save {
        vec![NO_SAVE_WARNING.to_string()]
    } else {
        Vec::new()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Opt {
    Delay,
    Monitor,
    All,
    Path,
    Clipboard,
    NoSave,
}

#[derive(Default)]
struct RawOptions {
    delay: u32,
    monitor: Option<usize>,
    all: bool,
    path: Option<PathBuf>,
    clipboard: bool,
    /// Aceptado por compatibilidad con scripts; no cambia el plan.
    no_save: bool,
}

fn parse_options(args: &[String], allowed: &[Opt]) -> Result<RawOptions, CliError> {
    let mut opts = RawOptions::default();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (arg.as_str(), None),
        };

        let kind = match name {
            "--delay" => Opt::Delay,
            "--monitor" => Opt::Monitor,
            "--all" => Opt::All,
            "--path" => Opt::Path,
            "--clipboard" => Opt::Clipboard,
            "--no-save" => Opt::NoSave,
            _ => {
                return Err(CliError::new(format!(
                    "opción desconocida: {arg} (prueba --help)"
                )))
            }
        };
        if !allowed.contains(&kind) {
            return Err(CliError::new(format!(
                "{name} no es válido para este comando"
            )));
        }

        match kind {
            Opt::Delay | Opt::Monitor | Opt::Path => {
                let value = match inline {
                    Some(value) => value,
                    None => {
                        index += 1;
                        args.get(index)
                            .cloned()
                            .ok_or_else(|| CliError::new(format!("falta el valor de {name}")))?
                    }
                };
                if value.is_empty() {
                    return Err(CliError::new(format!("{name} necesita un valor")));
                }
                match kind {
                    Opt::Delay => {
                        let delay: u32 = value.parse().map_err(|_| {
                            CliError::new(format!("retardo no válido: {value} (segundos)"))
                        })?;
                        if delay > 3600 {
                            return Err(CliError::new("el retardo máximo es 3600 s"));
                        }
                        opts.delay = delay;
                    }
                    Opt::Monitor => {
                        let number: usize = value
                            .parse()
                            .map_err(|_| CliError::new(format!("monitor no válido: {value}")))?;
                        if number == 0 {
                            return Err(CliError::new("los monitores empiezan en 1"));
                        }
                        opts.monitor = Some(number - 1);
                    }
                    Opt::Path => opts.path = Some(PathBuf::from(value)),
                    _ => unreachable!("las opciones con valor ya están cubiertas"),
                }
            }
            Opt::All => {
                if inline.is_some() {
                    return Err(CliError::new("--all no lleva valor"));
                }
                opts.all = true;
            }
            Opt::Clipboard => {
                if inline.is_some() {
                    return Err(CliError::new("--clipboard no lleva valor"));
                }
                opts.clipboard = true;
            }
            Opt::NoSave => {
                if inline.is_some() {
                    return Err(CliError::new("--no-save no lleva valor"));
                }
                opts.no_save = true;
            }
        }

        index += 1;
    }

    Ok(opts)
}

struct OutputRequest {
    path: Option<PathBuf>,
    clipboard: bool,
}

impl OutputRequest {
    /// Sin opciones: copiar (nunca guardar). `--path` guarda explícitamente y
    /// `--clipboard` añade la copia. `--no-save` no afecta al plan.
    fn plan(&self) -> DeliveryPlan {
        match &self.path {
            Some(target) if self.clipboard => DeliveryPlan::copy_and_save(target.clone()),
            Some(target) => DeliveryPlan::save_only(target.clone()),
            None => DeliveryPlan::copy_only(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn command(list: &[&str]) -> Command {
        match parse_args(&args(list)).expect("parse ok") {
            Parsed::Command(cmd) => cmd,
            other => panic!("se esperaba comando, llegó {other:?}"),
        }
    }

    fn parse_with_warnings(list: &[&str]) -> (Parsed, Vec<String>) {
        parse_args_with_warnings(&args(list)).expect("parse ok")
    }

    #[test]
    fn sin_argumentos_arranca_en_bandeja() {
        assert_eq!(parse_args(&[]).unwrap(), Parsed::StartTray);
    }

    #[test]
    fn ayuda_y_version() {
        assert_eq!(parse_args(&args(&["--help"])).unwrap(), Parsed::Help);
        assert_eq!(parse_args(&args(&["-h"])).unwrap(), Parsed::Help);
        assert_eq!(parse_args(&args(&["--version"])).unwrap(), Parsed::Version);
        assert_eq!(parse_args(&args(&["gui", "--help"])).unwrap(), Parsed::Help);
        assert!(HELP.contains("solo se copian"));
        assert!(!HELP.contains("--no-save                   No guardar"));
    }

    #[test]
    fn gui_con_retardo() {
        assert_eq!(command(&["gui"]), Command::CaptureArea { delay: 0 });
        assert_eq!(
            command(&["gui", "--delay", "3"]),
            Command::CaptureArea { delay: 3 }
        );
        assert_eq!(
            command(&["gui", "--delay=5"]),
            Command::CaptureArea { delay: 5 }
        );
    }

    #[test]
    fn ac6_full_por_defecto_solo_copia() {
        assert_eq!(
            command(&["full"]),
            Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            }
        );
    }

    #[test]
    fn full_monitor_y_todos() {
        assert_eq!(
            command(&["full", "--monitor", "2"]),
            Command::CaptureMonitors {
                selection: MonitorSelection::Index(1),
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            }
        );
        assert_eq!(
            command(&["full", "--all", "--delay", "5"]),
            Command::CaptureMonitors {
                selection: MonitorSelection::All,
                delay: 5,
                plan: DeliveryPlan::copy_only(),
            }
        );
    }

    #[test]
    fn ac6_full_path_solo_guarda() {
        assert_eq!(
            command(&["full", "--path", r"C:\Capturas"]),
            Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::save_only(PathBuf::from(r"C:\Capturas")),
            }
        );
    }

    #[test]
    fn full_clipboard_solo_copia() {
        assert_eq!(
            command(&["full", "--clipboard"]),
            Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            }
        );
    }

    #[test]
    fn full_path_y_clipboard() {
        assert_eq!(
            command(&["full", "--path", r"C:\a.png", "--clipboard"]),
            Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::copy_and_save(PathBuf::from(r"C:\a.png")),
            }
        );
    }

    #[test]
    fn no_save_se_acepta_se_ignora_y_avisa() {
        let (parsed, warnings) = parse_with_warnings(&["full", "--no-save"]);
        assert_eq!(
            parsed,
            Parsed::Command(Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            })
        );
        assert_eq!(warnings, vec![NO_SAVE_WARNING.to_string()]);

        // Con --path sigue guardando y el aviso se mantiene.
        let (parsed, warnings) = parse_with_warnings(&["full", "--path", r"C:\a.png", "--no-save"]);
        assert_eq!(
            parsed,
            Parsed::Command(Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::save_only(PathBuf::from(r"C:\a.png")),
            })
        );
        assert_eq!(warnings.len(), 1);

        // Aceptado también en gui/window para no romper scripts.
        let (_, warnings) = parse_with_warnings(&["gui", "--no-save"]);
        assert_eq!(warnings.len(), 1);
        let (_, warnings) = parse_with_warnings(&["window", "--no-save"]);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn errores_de_validacion() {
        assert!(parse_args(&args(&["full", "--monitor", "0"])).is_err());
        assert!(parse_args(&args(&["full", "--all", "--monitor", "1"])).is_err());
        assert!(parse_args(&args(&["gui", "--clipboard"])).is_err());
        assert!(parse_args(&args(&["full", "--delay"])).is_err());
        assert!(parse_args(&args(&["full", "--delay", "99999"])).is_err());
        assert!(parse_args(&args(&["full", "--wat"])).is_err());
        assert!(parse_args(&args(&["nope"])).is_err());
        assert!(parse_args(&args(&["carpeta"])).is_err());
    }

    #[test]
    fn window_y_comandos_auxiliares() {
        assert_eq!(
            command(&["window"]),
            Command::CaptureWindow {
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            }
        );
        assert_eq!(command(&["ajustes"]), Command::OpenSettings);
        assert_eq!(command(&["salir"]), Command::Quit);
    }

    #[test]
    fn debug_targets_oculto_acepta_retardo() {
        assert_eq!(
            parse_args(&args(&["debug-targets"])).unwrap(),
            Parsed::DebugTargets { delay: 0 }
        );
        assert_eq!(
            parse_args(&args(&["debug-targets", "--delay", "5"])).unwrap(),
            Parsed::DebugTargets { delay: 5 }
        );
        assert!(parse_args(&args(&["debug-targets", "--clipboard"])).is_err());
        assert!(!HELP.contains("debug-targets"), "el subcomando es oculto");
    }

    #[test]
    fn payload_de_wm_copydata() {
        assert_eq!(
            parse_line("gui\n--delay\n2").unwrap(),
            Parsed::Command(Command::CaptureArea { delay: 2 })
        );
        // La instancia residente acepta --no-save en silencio: el aviso ya lo
        // mostró el proceso que reenvió la orden.
        assert_eq!(
            parse_line("full\n--no-save").unwrap(),
            Parsed::Command(Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            })
        );
        assert_eq!(parse_line("").unwrap(), Parsed::StartTray);
    }

    #[test]
    fn opciones_no_se_comen_el_siguiente_flag() {
        // `--path --clipboard` tomaría "--clipboard" como ruta; es ambiguo pero
        // no debe confundirse con la bandera. Lo tratamos como ruta literal.
        let cmd = command(&["full", "--path", "--clipboard"]);
        assert_eq!(
            cmd,
            Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::save_only(PathBuf::from("--clipboard")),
            }
        );
    }
}
