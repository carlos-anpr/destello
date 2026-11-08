//! Dispatcher de comandos: capturas, copia al portapapeles, ajustes.
//!
//! Las capturas directas solo copian. Guardar es exclusivamente "Guardar como"
//! desde el overlay; no hay guardado automático ni toasts.
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::HWND;

use crate::capture;
use crate::config::Config;
use crate::geom::{Point, Rect};
use crate::image_ops::RgbaImage;
use crate::output::{self, Action, DeliveryPlan};

/// Qué monitores capturar en la captura directa.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorSelection {
    /// Monitor bajo el cursor.
    Cursor,
    /// Todos (escritorio virtual).
    All,
    /// Índice 0-based en `capture::monitors()`.
    Index(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Overlay de selección de área.
    CaptureArea { delay: u32 },
    /// Captura directa de monitor(es).
    CaptureMonitors {
        selection: MonitorSelection,
        delay: u32,
        plan: DeliveryPlan,
    },
    /// Captura directa de la ventana activa.
    CaptureWindow { delay: u32, plan: DeliveryPlan },
    /// Repite el rectángulo de la última selección.
    RepeatLastArea,
    OpenSettings,
    Quit,
}

impl Command {
    pub fn delay(&self) -> u32 {
        match self {
            Self::CaptureArea { delay }
            | Self::CaptureMonitors { delay, .. }
            | Self::CaptureWindow { delay, .. } => *delay,
            _ => 0,
        }
    }

    pub fn without_delay(&self) -> Self {
        let mut cmd = self.clone();
        match &mut cmd {
            Self::CaptureArea { delay }
            | Self::CaptureMonitors { delay, .. }
            | Self::CaptureWindow { delay, .. } => *delay = 0,
            _ => {}
        }
        cmd
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Outcome {
    /// La configuración cambió (la UI debe refrescar su copia).
    pub config_changed: bool,
    pub quit: bool,
}

/// Resultado del overlay traducido a tipos propios (sin depender de src/ui
/// cuando se compila sin la feature `ui`).
enum OverlayOutcome {
    Cancelled,
    Done {
        image: RgbaImage,
        action: Action,
        area: Rect,
    },
    Pin {
        image: RgbaImage,
        area: Rect,
    },
}

/// Evita solapar dos capturas/ajustes modales (el hook sigue tragando ImpPnt).
static MODAL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Ejecuta un comando ya sin retardo (los retardos los programa main.rs).
pub fn dispatch(cmd: Command, cfg: &mut Config, owner: HWND) -> Outcome {
    let mut outcome = Outcome::default();

    match cmd {
        Command::CaptureArea { .. } => {
            let trigger = crate::hotkeys::take_area_trigger();
            let Some(_modal) = ModalGuard::enter() else {
                crate::logging::info("captura ignorada: ya hay una ventana modal abierta");
                return outcome;
            };
            let before = cfg.last_area;
            if let Err(e) = capture_area(cfg, owner, trigger) {
                crate::notify::error(&format!("No se pudo capturar: {e:#}"));
            }
            outcome.config_changed = cfg.last_area != before;
        }
        Command::CaptureMonitors {
            selection, plan, ..
        } => {
            let Some(_modal) = ModalGuard::enter() else {
                crate::logging::info("captura ignorada: ya hay una ventana modal abierta");
                return outcome;
            };
            if let Err(e) = capture_monitors(&selection, &plan, cfg) {
                crate::notify::error(&format!("No se pudo capturar: {e:#}"));
            }
        }
        Command::CaptureWindow { plan, .. } => {
            let Some(_modal) = ModalGuard::enter() else {
                crate::logging::info("captura ignorada: ya hay una ventana modal abierta");
                return outcome;
            };
            if let Err(e) = capture_window(&plan, cfg) {
                crate::notify::error(&format!("No se pudo capturar la ventana: {e:#}"));
            }
        }
        Command::RepeatLastArea => {
            let Some(_modal) = ModalGuard::enter() else {
                crate::logging::info("captura ignorada: ya hay una ventana modal abierta");
                return outcome;
            };
            if let Err(e) = repeat_last_area(cfg) {
                crate::notify::error(&format!("No se pudo repetir la última área: {e:#}"));
            }
        }
        Command::OpenSettings => {
            let Some(_modal) = ModalGuard::enter() else {
                crate::logging::info("ajustes ignorados: ya hay una ventana modal abierta");
                return outcome;
            };
            let changed = open_settings(cfg);
            outcome.config_changed = changed;
        }
        Command::Quit => outcome.quit = true,
    }

    outcome
}

/// La ventana fijada guarda desde su propia copia de la configuración. Antes
/// de volver a usar `last_save_dir` (overlay/ajustes) se refresca desde disco
/// para no sobrescribir con un valor viejo la carpeta recién elegida.
fn sync_last_save_dir(cfg: &mut Config) {
    let load = Config::load();
    if !load.created && !load.corrupt {
        cfg.last_save_dir = load.config.last_save_dir;
    }
}

fn open_settings(cfg: &mut Config) -> bool {
    sync_last_save_dir(cfg);
    #[cfg(feature = "ui")]
    {
        crate::hotkeys::suspend();
        let saved = crate::ui::settings::open(cfg);
        crate::hotkeys::resume();
        if !saved {
            return false;
        }
        cfg.normalize();
        if let Err(e) = cfg.save() {
            crate::notify::error(&format!("No se pudo guardar la configuración: {e:#}"));
        }
        if let Err(e) = crate::autostart::set(cfg.launch_at_startup) {
            crate::notify::error(&format!("No se pudo actualizar el arranque: {e:#}"));
        }
        for spec in crate::hotkeys::reload(cfg) {
            crate::notify::error(&format!("No se pudo registrar el atajo {spec}"));
        }
        true
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = cfg;
        crate::notify::error("Los ajustes necesitan la interfaz gráfica");
        false
    }
}

fn capture_area(cfg: &mut Config, _owner: HWND, trigger: Option<Instant>) -> Result<()> {
    sync_last_save_dir(cfg);
    let started = Instant::now();
    let shot = capture::capture_virtual_screen(cfg.include_cursor)?;
    log_capture("capture_virtual_screen", started, shot.width, shot.height);
    // La imagen ya está congelada; cerrar el flyout del shell permite que el
    // overlay reciba foco. Si no hay flyout visible no espera.
    capture::dismiss_shell_flyouts();
    if let Some(trigger) = trigger {
        let total = trigger.elapsed().as_secs_f64() * 1000.0;
        let message = format!("ImpPnt->run_overlay: {total:.1} ms (incluye captura)");
        if total > 150.0 {
            crate::logging::warn(&message);
        } else {
            crate::logging::info(&message);
        }
    }
    match run_overlay(shot, cfg)? {
        OverlayOutcome::Cancelled => Ok(()),
        OverlayOutcome::Done {
            image,
            action,
            area,
        } => {
            let mut persist = false;
            if area.width() > 0 && area.height() > 0 {
                cfg.last_area = Some([area.left, area.top, area.right, area.bottom]);
                persist = true;
            }
            if action == Action::SaveAs {
                // El overlay ya guardó con output::save_as_dialog y actualizó
                // cfg.last_save_dir; solo hay que persistir la configuración.
                persist = true;
            }
            if persist {
                if let Err(e) = cfg.save() {
                    crate::logging::warn(&format!("no se pudo guardar la configuración: {e:#}"));
                }
            }

            match action {
                // Copiar es silencioso, como Ctrl+C en cualquier app.
                Action::Copy => {
                    if let Err(e) = output::copy_to_clipboard(&image) {
                        crate::notify::error(&format!("No se pudo copiar la captura: {e:#}"));
                    }
                }
                // El archivo ya está escrito por el overlay.
                Action::SaveAs => crate::logging::info("captura guardada desde el overlay"),
            }
            Ok(())
        }
        OverlayOutcome::Pin { image, area } => show_pin(image, area),
    }
}

fn capture_monitors(
    selection: &MonitorSelection,
    plan: &DeliveryPlan,
    cfg: &Config,
) -> Result<()> {
    let rect = match selection {
        MonitorSelection::Cursor => {
            let cursor = cursor_position().unwrap_or(Point { x: 0, y: 0 });
            capture::monitor_at(cursor).rect
        }
        MonitorSelection::All => capture::virtual_screen_rect(),
        MonitorSelection::Index(index) => {
            let monitors = capture::monitors();
            monitors
                .get(*index)
                .ok_or_else(|| anyhow!("no existe el monitor {}", index + 1))?
                .rect
        }
    };
    deliver_rect(rect, plan, cfg)
}

fn capture_window(plan: &DeliveryPlan, cfg: &Config) -> Result<()> {
    let rect =
        capture::foreground_window_rect().ok_or_else(|| anyhow!("no hay una ventana activa"))?;
    if rect.width() <= 0 || rect.height() <= 0 {
        return Err(anyhow!("la ventana activa no tiene área visible"));
    }
    deliver_rect(rect, plan, cfg)
}

fn repeat_last_area(cfg: &Config) -> Result<()> {
    let area = cfg
        .last_area
        .ok_or_else(|| anyhow!("todavía no hay una última área seleccionada"))?;
    let rect = Rect::new(area[0], area[1], area[2], area[3])
        .intersect(capture::virtual_screen_rect());
    if rect.is_empty() {
        return Err(anyhow!("la última área no es válida o está fuera de pantalla"));
    }
    deliver_rect(rect, &DeliveryPlan::copy_only(), cfg)
}

fn deliver_rect(rect: Rect, plan: &DeliveryPlan, cfg: &Config) -> Result<()> {
    let started = Instant::now();
    let shot = capture::capture_rect(rect, cfg.include_cursor)?;
    log_capture("capture_rect", started, shot.width, shot.height);
    let image = RgbaImage::from_screenshot(&shot);
    output::deliver_plan(&image, plan, cfg)
}

fn cursor_position() -> Option<Point> {
    let mut point = windows::Win32::Foundation::POINT::default();
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) }
        .ok()
        .map(|_| Point {
            x: point.x,
            y: point.y,
        })
}

fn log_capture(label: &str, started: Instant, width: u32, height: u32) {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let message = format!("{label}: {ms:.1} ms ({width}x{height})");
    if ms > 40.0 {
        crate::logging::warn(&message);
    } else {
        crate::logging::info(&message);
    }
}

#[cfg(feature = "ui")]
fn run_overlay(shot: capture::Screenshot, cfg: &mut Config) -> Result<OverlayOutcome> {
    use crate::ui::OverlayResult;
    Ok(match crate::ui::run_overlay(shot, cfg) {
        OverlayResult::Cancelled => OverlayOutcome::Cancelled,
        OverlayResult::Done {
            image,
            action,
            area,
        } => OverlayOutcome::Done {
            image,
            action,
            area,
        },
        OverlayResult::Pin { image, area } => OverlayOutcome::Pin { image, area },
    })
}

#[cfg(not(feature = "ui"))]
fn run_overlay(_shot: capture::Screenshot, _cfg: &mut Config) -> Result<OverlayOutcome> {
    Err(anyhow!("la interfaz todavía no está disponible"))
}

#[cfg(feature = "ui")]
fn show_pin(image: RgbaImage, area: Rect) -> Result<()> {
    crate::ui::pin::show(image, area);
    Ok(())
}

#[cfg(not(feature = "ui"))]
fn show_pin(_image: RgbaImage, _area: Rect) -> Result<()> {
    Err(anyhow!("fijar en pantalla necesita la interfaz gráfica"))
}

struct ModalGuard;

impl ModalGuard {
    fn enter() -> Option<Self> {
        if MODAL_ACTIVE.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(Self)
        }
    }
}

impl Drop for ModalGuard {
    fn drop(&mut self) {
        MODAL_ACTIVE.store(false, Ordering::SeqCst);
    }
}
