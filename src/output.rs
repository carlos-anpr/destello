//! Nombres de archivo, "Guardar como" y el archivo de respaldo del portapapeles.
//!
//! Ya no existe el guardado automático: `Action::SaveAs` es la única vía para
//! escribir un archivo elegido por el usuario. El PNG de `%TEMP%` solo existe
//! para que Ctrl+V en una carpeta cree un archivo (CF_HDROP).
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::config::{Config, ImageFormat};
use crate::image_ops::{self, RgbaImage};
use crate::paths;

/// Acción elegida en el overlay para una captura.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    SaveAs,
}

/// Qué hacer con una captura directa (CLI, bandeja, atajos).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryPlan {
    pub copy: bool,
    /// `--path`: guardado explícito pedido por el usuario.
    pub target: Option<PathBuf>,
}

impl Default for DeliveryPlan {
    fn default() -> Self {
        Self::copy_only()
    }
}

impl DeliveryPlan {
    pub fn copy_only() -> Self {
        Self {
            copy: true,
            target: None,
        }
    }

    pub fn save_only(target: PathBuf) -> Self {
        Self {
            copy: false,
            target: Some(target),
        }
    }

    pub fn copy_and_save(target: PathBuf) -> Self {
        Self {
            copy: true,
            target: Some(target),
        }
    }
}

/// Nombre fijo del archivo que Explorer pega como `.png`.
const BACKUP_PATTERN: &str = "Captura {fecha} {hora}";
const CLEANUP_MAX_ATTEMPTS: usize = 5;
const CLEANUP_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Si el portapapeles está ocupado al inspeccionarlo (Win+V, Ditto…), se
/// reintenta desde el timer de la ventana de control sin bloquear el hilo.
pub const CLIPBOARD_RETRY_TIMER_ID: usize = 0xDE57;
const CLIPBOARD_RETRY_DELAY_MS: u32 = 300;
const CLIPBOARD_RETRY_ATTEMPTS: u32 = 5;

/// Ruta del único archivo de respaldo vivo (si lo hay).
static CURRENT_BACKUP: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Clone, Debug)]
struct ClipboardRetry {
    path: PathBuf,
    remaining: u32,
}

static CURRENT_RETRY: Mutex<Option<ClipboardRetry>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryDecision {
    Keep,
    Delete,
    Retry,
}

/// Máquina de estados pura del reintento de inspección del portapapeles.
/// `remaining` son las comprobaciones que aún puede hacer el timer.
fn retry_step(found: Option<bool>, remaining: u32) -> (RetryDecision, u32) {
    match found {
        Some(true) => (RetryDecision::Keep, 0),
        Some(false) => (RetryDecision::Delete, 0),
        None if remaining > 1 => (RetryDecision::Retry, remaining - 1),
        None => (RetryDecision::Keep, 0),
    }
}

/// Guarda con `--path`: carpeta (nombre generado) o archivo exacto.
pub fn save_to_target(img: &RgbaImage, target: &Path, cfg: &Config) -> Result<PathBuf> {
    let extension = target
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let looks_like_file = target.is_file() || matches!(extension.as_str(), "png" | "jpg" | "jpeg");
    if !looks_like_file {
        std::fs::create_dir_all(target)
            .with_context(|| format!("no se pudo crear {}", target.display()))?;
        let stem = format_filename(&cfg.filename_pattern, Local::now());
        let path = unique_path(target, &stem, cfg.format.extension());
        save_image_to(img, &path, cfg)?;
        return Ok(path);
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("no se pudo crear {}", parent.display()))?;
        }
    }
    let path = if target.extension().is_none() {
        target.with_extension(cfg.format.extension())
    } else {
        target.to_path_buf()
    };
    save_image_to(img, &path, cfg)?;
    Ok(path)
}

/// Escribe la imagen deduciendo el formato por la extensión (.jpg/.jpeg → JPEG).
pub fn save_image_to(img: &RgbaImage, path: &Path, cfg: &Config) -> Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "jpg" || ext == "jpeg" {
        let bytes = image_ops::encode_jpeg(img, cfg.jpeg_quality)?;
        std::fs::write(path, bytes)
            .with_context(|| format!("no se pudo escribir {}", path.display()))?;
    } else {
        let bytes = image_ops::encode_png(img)?;
        std::fs::write(path, bytes)
            .with_context(|| format!("no se pudo escribir {}", path.display()))?;
    }
    Ok(())
}

/// Entrega explícita: guardar en `--path` y/o copiar. Nunca guarda por defecto.
pub fn deliver_plan(img: &RgbaImage, plan: &DeliveryPlan, cfg: &Config) -> Result<()> {
    if img.is_empty() {
        return Err(anyhow!("la captura está vacía"));
    }
    if !plan.copy && plan.target.is_none() {
        return Err(anyhow!("no hay nada que copiar ni guardar"));
    }

    if let Some(target) = &plan.target {
        let path = save_to_target(img, target, cfg)?;
        crate::logging::info(&format!("captura guardada en {}", path.display()));
    }
    if plan.copy {
        copy_to_clipboard(img)?;
    }
    Ok(())
}

/// Copia la imagen y su archivo de respaldo. Silencioso: sin avisos de éxito.
pub fn copy_to_clipboard(img: &RgbaImage) -> Result<()> {
    let path = clipboard_file(img)?;
    if let Err(e) = crate::clipboard::copy_image(img, &path) {
        // Si el portapapeles quedó referenciando el archivo, no lo borramos.
        if clipboard_contains(&path) == Some(false) {
            forget_current(&path);
            let _ = std::fs::remove_file(&path);
        }
        return Err(e.context("no se pudo copiar la captura al portapapeles"));
    }
    if let Some(dir) = path.parent() {
        for stale in sweep_backups(dir, &path) {
            schedule_backup_cleanup(stale);
        }
    }
    crate::logging::info("captura copiada al portapapeles");
    Ok(())
}

/// Compatibilidad con la ventana fijada: la acción `Copy` copia y nada más.
/// `SaveAs` necesita `&mut Config` y un dueño real, así que se hace con
/// `save_as_dialog` desde el overlay o desde la propia ventana fijada.
pub fn deliver(
    img: RgbaImage,
    action: Action,
    _cfg: &Config,
    _owner: windows::Win32::Foundation::HWND,
) -> Result<()> {
    match action {
        Action::Copy => copy_to_clipboard(&img),
        Action::SaveAs => Err(anyhow!(
            "Guardar como debe abrirse desde el overlay con save_as_dialog"
        )),
    }
}

/// PNG en `%TEMP%\Destello\portapapeles\Captura AAAA-MM-DD HH.MM.SS.png`.
/// Deja un único archivo vivo (el de la última copia) y lo registra.
pub fn clipboard_file(img: &RgbaImage) -> Result<PathBuf> {
    clipboard_file_in(&paths::clipboard_cache_dir(), img, Local::now())
}

pub(crate) fn clipboard_file_in(
    dir: &Path,
    img: &RgbaImage,
    when: DateTime<Local>,
) -> Result<PathBuf> {
    if img.is_empty() {
        return Err(anyhow!("no se puede copiar una imagen vacía"));
    }
    std::fs::create_dir_all(dir)
        .with_context(|| format!("no se pudo crear {}", dir.display()))?;

    let stem = format_filename(BACKUP_PATTERN, when);
    let path = unique_path(dir, &stem, "png");
    let bytes = image_ops::encode_png(img)?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("no se pudo escribir {}", path.display()))?;
    set_current(Some(path.clone()));
    // El barrido del archivo anterior lo hace copy_to_clipboard después de
    // publicar el nuevo: si el portapapeles está ocupado, el pegado viejo
    // sigue funcionando.
    Ok(path)
}

/// Limpieza de arranque: conserva lo que el CF_HDROP actual siga referenciando
/// y borra los restos de `%TEMP%` y de la carpeta antigua de v1.
pub fn cleanup_startup() {
    let referenced = match crate::clipboard::try_file_paths() {
        Some(paths) => paths,
        None => {
            crate::logging::warn(
                "no se pudo inspeccionar el portapapeles al arrancar; se conservan los respaldos",
            );
            return;
        }
    };
    let temp = paths::clipboard_cache_dir();
    let legacy = paths::legacy_clipboard_cache_dir();
    let removed = cleanup_startup_in(&temp, &legacy, &referenced);

    let keep = referenced
        .iter()
        .find(|path| is_within(path, &temp) || is_within(path, &legacy))
        .cloned();
    set_current(keep);

    if removed > 0 {
        crate::logging::info(&format!(
            "limpieza de respaldos del portapapeles: {removed} archivos borrados"
        ));
    }
}

fn cleanup_startup_in(temp: &Path, legacy: &Path, referenced: &[PathBuf]) -> usize {
    let mut removed = cleanup_dir_except(temp, referenced);
    if !same_path(temp, legacy) {
        removed += cleanup_dir_except(legacy, referenced);
        // Si quedó un archivo referenciado, remove_dir falla y se conserva.
        let _ = std::fs::remove_dir(legacy);
    }
    removed
}

fn cleanup_dir_except(dir: &Path, keep: &[PathBuf]) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.iter().any(|kept| same_path(kept, &path)) {
            continue;
        }
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if result.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// `WM_CLIPBOARDUPDATE`: si el portapapeles ya no referencia nuestro archivo,
/// se borra con reintentos (nunca en el hilo de UI). Si está ocupado por otro
/// listener (Win+V, Ditto…), se reprograma la comprobación con un timer.
pub fn handle_clipboard_update(hwnd: HWND) {
    let Some(path) = current_backup() else {
        stop_clipboard_retry(hwnd);
        return;
    };
    match clipboard_contains(&path) {
        Some(true) => {
            // Todavía se puede pegar: no tocar y cancelar reintentos pendientes.
            cancel_clipboard_retry(hwnd, &path);
        }
        Some(false) => {
            cancel_clipboard_retry(hwnd, &path);
            forget_current(&path);
            schedule_backup_cleanup(path);
        }
        None => {
            let owner = crate::clipboard::owner_hwnd();
            let detail = if owner == hwnd { "propio" } else { "ajeno" };
            crate::logging::info(&format!(
                "portapapeles ocupado ({detail}); se revisa de nuevo en {} ms",
                CLIPBOARD_RETRY_DELAY_MS
            ));
            start_clipboard_retry(hwnd, path);
        }
    }
}

/// `WM_TIMER` del reintento de inspección. Cada tick comprueba de nuevo el
/// CF_HDROP sin bloquear el hilo; tras agotar los intentos conserva el archivo.
pub fn handle_clipboard_retry(hwnd: HWND) {
    let snapshot = {
        let retry = CURRENT_RETRY.lock().unwrap_or_else(|e| e.into_inner());
        retry.as_ref().map(|state| state.path.clone())
    };
    let Some(path) = snapshot else {
        unsafe {
            let _ = KillTimer(Some(hwnd), CLIPBOARD_RETRY_TIMER_ID);
        }
        return;
    };

    let found = clipboard_contains(&path);

    let mut retry = CURRENT_RETRY.lock().unwrap_or_else(|e| e.into_inner());
    let decision = match retry.as_mut() {
        None => {
            unsafe {
                let _ = KillTimer(Some(hwnd), CLIPBOARD_RETRY_TIMER_ID);
            }
            return;
        }
        Some(state) => {
            if !same_path(&state.path, &path) {
                // Otro reintento lo sustituyó; su timer sigue activo.
                return;
            }
            let (decision, remaining) = retry_step(found, state.remaining);
            if decision == RetryDecision::Retry {
                state.remaining = remaining;
            }
            decision
        }
    };

    match decision {
        RetryDecision::Retry => {}
        RetryDecision::Keep => {
            *retry = None;
            drop(retry);
            unsafe {
                let _ = KillTimer(Some(hwnd), CLIPBOARD_RETRY_TIMER_ID);
            }
            if found == Some(true) {
                crate::logging::info("el respaldo sigue en el portapapeles");
            } else {
                crate::logging::info(
                    "portapapeles ocupado tras 5 comprobaciones; se conserva el respaldo",
                );
            }
        }
        RetryDecision::Delete => {
            *retry = None;
            drop(retry);
            unsafe {
                let _ = KillTimer(Some(hwnd), CLIPBOARD_RETRY_TIMER_ID);
            }
            forget_current(&path);
            schedule_backup_cleanup(path);
        }
    }
}

/// Cancela el timer de reintento (al salir o al dejar de tener respaldo).
pub fn stop_clipboard_retry(hwnd: HWND) {
    *CURRENT_RETRY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    unsafe {
        let _ = KillTimer(Some(hwnd), CLIPBOARD_RETRY_TIMER_ID);
    }
}

fn start_clipboard_retry(hwnd: HWND, path: PathBuf) {
    {
        let mut retry = CURRENT_RETRY.lock().unwrap_or_else(|e| e.into_inner());
        *retry = Some(ClipboardRetry {
            path,
            remaining: CLIPBOARD_RETRY_ATTEMPTS,
        });
    }
    let id = unsafe {
        SetTimer(
            Some(hwnd),
            CLIPBOARD_RETRY_TIMER_ID,
            CLIPBOARD_RETRY_DELAY_MS,
            None,
        )
    };
    if id == 0 {
        crate::logging::warn("no se pudo programar la revisión del portapapeles");
        *CURRENT_RETRY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn cancel_clipboard_retry(hwnd: HWND, path: &Path) {
    let mut retry = CURRENT_RETRY.lock().unwrap_or_else(|e| e.into_inner());
    let matches = retry
        .as_ref()
        .is_some_and(|state| same_path(&state.path, path));
    if matches {
        *retry = None;
        drop(retry);
        unsafe {
            let _ = KillTimer(Some(hwnd), CLIPBOARD_RETRY_TIMER_ID);
        }
    }
}

/// Al salir: si seguimos siendo dueños del portapapeles, el archivo se conserva
/// para que el usuario pueda pegar; se limpiará en el próximo arranque.
pub fn on_exit(hwnd: windows::Win32::Foundation::HWND) {
    let Some(path) = current_backup() else {
        return;
    };
    if crate::clipboard::owner_hwnd() == hwnd {
        return;
    }
    match clipboard_contains(&path) {
        Some(false) => {}
        // Referenciado o imposible de comprobar: conservar. El próximo arranque
        // hará la limpieza con más información.
        Some(true) | None => return,
    }
    forget_current(&path);
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            crate::logging::warn(&format!(
                "no se pudo borrar el respaldo {} al salir: {e}",
                path.display()
            ));
        }
    }
}

/// "Guardar como…" con IFileSaveDialog, encima del overlay. Devuelve None si
/// se cancela. Recuerda la última carpeta en `cfg.last_save_dir`.
pub fn save_as_dialog(
    img: &RgbaImage,
    cfg: &mut Config,
    owner: windows::Win32::Foundation::HWND,
) -> Result<Option<PathBuf>> {
    match unsafe { show_save_dialog(cfg, owner) }? {
        Some(path) => {
            save_image_to(img, &path, cfg)?;
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    cfg.last_save_dir = Some(parent.to_path_buf());
                }
            }
            Ok(Some(path))
        }
        None => Ok(None),
    }
}

unsafe fn show_save_dialog(
    cfg: &Config,
    owner: windows::Win32::Foundation::HWND,
) -> Result<Option<PathBuf>> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, CLSCTX_ALL};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
        FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, SIGDN_FILESYSPATH,
    };
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

    let filters = [
        COMDLG_FILTERSPEC {
            pszName: w!("Imagen PNG (*.png)"),
            pszSpec: w!("*.png"),
        },
        COMDLG_FILTERSPEC {
            pszName: w!("Imagen JPEG (*.jpg)"),
            pszSpec: w!("*.jpg;*.jpeg"),
        },
    ];

    let dialog: IFileSaveDialog =
        CoCreateInstance(&FileSaveDialog, None::<&windows::core::IUnknown>, CLSCTX_ALL)
            .context("no se pudo crear el diálogo de guardado")?;

    dialog.SetTitle(w!("Guardar captura"))?;
    dialog.SetFileTypes(&filters)?;
    let type_index = if cfg.format == ImageFormat::Jpg { 2 } else { 1 };
    dialog.SetFileTypeIndex(type_index)?;
    dialog.SetOptions(FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM)?;
    let ext = wide(cfg.format.extension());
    dialog.SetDefaultExtension(PCWSTR(ext.as_ptr()))?;

    let stem = wide(&format_filename(&cfg.filename_pattern, Local::now()));
    dialog.SetFileName(PCWSTR(stem.as_ptr()))?;

    // Última carpeta usada; si ya no existe, Imágenes.
    let folder = cfg
        .last_save_dir
        .as_ref()
        .filter(|dir| dir.is_dir())
        .cloned()
        .unwrap_or_else(paths::pictures_dir);
    let folder_w = wide(folder.to_string_lossy().as_ref());
    if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
        PCWSTR(folder_w.as_ptr()),
        None::<&IBindCtx>,
    ) {
        let _ = dialog.SetFolder(&item);
    }

    let owner = if owner.0.is_null() { None } else { Some(owner) };
    if let Some(owner) = owner {
        // El diálogo es modal al overlay; reforzar el orden Z/foco.
        let _ = SetForegroundWindow(owner);
    }
    match dialog.Show(owner) {
        Ok(()) => {}
        Err(e) if is_cancel(&e) => return Ok(None),
        Err(e) => return Err(anyhow!("diálogo de guardado: {e}")),
    }

    let item = dialog.GetResult().context("diálogo: resultado")?;
    let name = item
        .GetDisplayName(SIGDN_FILESYSPATH)
        .context("diálogo: ruta del resultado")?;
    let text = name
        .to_string()
        .map_err(|_| anyhow!("ruta de guardado no válida"));
    CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));

    let mut path = PathBuf::from(text?);
    if path.extension().is_none() {
        // Si el usuario cambió el filtro a JPG, ese es el formato esperado.
        let selected = dialog.GetFileTypeIndex().unwrap_or(type_index);
        path.set_extension(if selected == 2 { "jpg" } else { "png" });
    }
    Ok(Some(path))
}

fn is_cancel(err: &windows::core::Error) -> bool {
    // HRESULT_FROM_WIN32(ERROR_CANCELLED)
    err.code().0 as u32 == 0x8007_04C7
}

/// Aplica el patrón con la fecha/hora indicada y limpia caracteres inválidos.
pub fn format_filename(pattern: &str, when: DateTime<Local>) -> String {
    let date = when.format("%Y-%m-%d").to_string();
    let time = when.format("%H.%M.%S").to_string();
    let mut out = pattern
        .replace("{fecha_hora}", &format!("{date} {time}"))
        .replace("{fecha}", &date)
        .replace("{hora}", &time)
        .replace("{date}", &date)
        .replace("{time}", &time);

    out = out
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '-'
            } else {
                c
            }
        })
        .collect();

    let out = out.trim().trim_end_matches(['.', ' ']).to_string();
    if out.is_empty() {
        return "Captura".to_string();
    }
    // Nombres razonables para el sistema de archivos.
    out.chars().take(120).collect()
}

/// Primer nombre libre: "Captura.png", "Captura (2).png", …
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..10_000u32 {
        let candidate = dir.join(format!("{stem} ({n}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem} ({}).{ext}", 10_000u32))
}

#[allow(dead_code)] // la vista previa de Ajustes la usa desde la UI
pub fn preview_filename(pattern: &str, format: ImageFormat) -> String {
    format!(
        "{}.{}",
        format_filename(pattern, Local::now()),
        format.extension()
    )
}

fn set_current(path: Option<PathBuf>) {
    *CURRENT_BACKUP.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

fn current_backup() -> Option<PathBuf> {
    CURRENT_BACKUP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn forget_current(path: &Path) {
    let mut current = CURRENT_BACKUP.lock().unwrap_or_else(|e| e.into_inner());
    if current.as_deref().is_some_and(|known| same_path(known, path)) {
        *current = None;
    }
}

/// ¿El portapapeles contiene todavía `path`? None = no se pudo inspeccionar.
fn clipboard_contains(path: &Path) -> Option<bool> {
    crate::clipboard::try_file_paths()
        .map(|paths| paths.iter().any(|candidate| same_path(candidate, path)))
}

/// Borra los demás archivos de la carpeta y devuelve los que estaban bloqueados.
fn sweep_backups(dir: &Path, keep: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut blocked = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if same_path(&path, keep) || !path.is_file() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => blocked.push(path),
        }
    }
    blocked
}

/// Reintenta borrar un respaldo sin bloquear el hilo de UI.
fn schedule_backup_cleanup(path: PathBuf) {
    std::thread::spawn(move || {
        if current_backup()
            .as_deref()
            .is_some_and(|current| same_path(current, &path))
        {
            return;
        }
        if clipboard_contains(&path) == Some(true) {
            return;
        }
        if let Err(e) = remove_with_retries(&path, CLEANUP_MAX_ATTEMPTS, CLEANUP_RETRY_DELAY) {
            crate::logging::warn(&format!(
                "no se pudo borrar el respaldo {} tras {CLEANUP_MAX_ATTEMPTS} intentos: {e}",
                path.display()
            ));
        }
    });
}

fn remove_with_retries(path: &Path, attempts: usize, delay: Duration) -> std::io::Result<()> {
    let mut last = std::io::Error::other("sin intentos");
    for attempt in 0..attempts.max(1) {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last = e;
                if attempt + 1 < attempts.max(1) {
                    std::thread::sleep(delay);
                }
            }
        }
    }
    Err(last)
}

fn path_key(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}

fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

fn is_within(path: &Path, dir: &Path) -> bool {
    let mut prefix = path_key(dir);
    if !prefix.ends_with('\\') {
        prefix.push('\\');
    }
    path_key(path).starts_with(&prefix)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn img(w: u32, h: u32) -> RgbaImage {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[(x * 7) as u8, (y * 11) as u8, 128, 255]);
            }
        }
        RgbaImage {
            width: w,
            height: h,
            data,
        }
    }

    fn at(second: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 8, 14, 30, second).unwrap()
    }

    /// Simula una copia completa: escribe el respaldo y barre los anteriores.
    fn write_backup(dir: &Path, image: &RgbaImage, when: DateTime<Local>) -> PathBuf {
        let path = clipboard_file_in(dir, image, when).unwrap();
        let _ = sweep_backups(dir, &path);
        path
    }

    #[test]
    fn patron_fecha_hora() {
        let when = Local.with_ymd_and_hms(2026, 11, 7, 14, 30, 12).unwrap();
        assert_eq!(
            format_filename("Captura {fecha} {hora}", when),
            "Captura 2026-11-07 14.30.12"
        );
        assert_eq!(format_filename("{fecha_hora}", when), "2026-11-07 14.30.12");
    }

    #[test]
    fn patron_limpia_caracteres_invalidos() {
        let when = Local.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(format_filename("a/b:c*?\"<>|", when), "a-b-c------");
        assert_eq!(format_filename("   ", when), "Captura");
        assert_eq!(format_filename("fin.", when), "fin");
    }

    #[test]
    fn colisiones_anaden_sufijo() {
        let dir = tempfile::tempdir().unwrap();
        let first = unique_path(dir.path(), "Captura", "png");
        assert_eq!(first.file_name().unwrap(), "Captura.png");
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(dir.path(), "Captura", "png");
        assert_eq!(second.file_name().unwrap(), "Captura (2).png");
        std::fs::write(&second, b"x").unwrap();
        let third = unique_path(dir.path(), "Captura", "png");
        assert_eq!(third.file_name().unwrap(), "Captura (3).png");
    }

    #[test]
    fn guarda_png_y_jpg_legibles() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let image = img(8, 5);

        let png = dir.path().join("a.png");
        save_image_to(&image, &png, &cfg).unwrap();
        let decoded = image::open(&png).unwrap().to_rgba8();
        assert_eq!(decoded.width(), 8);
        assert_eq!(decoded.height(), 5);

        let jpg = dir.path().join("b.jpg");
        save_image_to(&image, &jpg, &cfg).unwrap();
        let decoded = image::open(&jpg).unwrap().to_rgba8();
        assert_eq!(decoded.width(), 8);
        assert_eq!(decoded.height(), 5);
    }

    #[test]
    fn target_carpeta_o_archivo() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let image = img(3, 3);

        let folder = dir.path().join("carpeta");
        let saved = save_to_target(&image, &folder, &cfg).unwrap();
        assert!(saved.starts_with(&folder));
        assert!(saved.is_file());

        let file = dir.path().join("exacto.png");
        let saved = save_to_target(&image, &file, &cfg).unwrap();
        assert_eq!(saved, file);
        assert!(file.is_file());
    }

    #[test]
    fn respaldo_nombre_fijo_y_png_valido() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_backup(dir.path(), &img(4, 3), at(12));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "Captura 2026-11-08 14.30.12.png"
        );
        let decoded = image::open(&path).unwrap().to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (4, 3));
        let count = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, 1, "solo debe quedar el archivo vivo");
    }

    #[test]
    fn ac2_respaldo_reemplaza_el_anterior_y_sufija_mismo_segundo() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_backup(dir.path(), &img(2, 2), at(12));
        let second = write_backup(dir.path(), &img(2, 2), at(12));

        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "Captura 2026-11-08 14.30.12 (2).png"
        );
        assert!(!first.exists(), "el archivo anterior debe borrarse");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        let third = write_backup(dir.path(), &img(2, 2), at(13));
        assert_eq!(
            third.file_name().unwrap().to_string_lossy(),
            "Captura 2026-11-08 14.30.13.png"
        );
        assert!(!second.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn arranque_conserva_el_referenciado_y_borra_restos() {
        let base = tempfile::tempdir().unwrap();
        let temp = base.path().join("temp").join("portapapeles");
        let legacy = base.path().join("local").join("portapapeles");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();

        let stale = temp.join("viejo.png");
        let keep = temp.join("Captura actual.png");
        let old = legacy.join("resto-v1.png");
        std::fs::write(&stale, b"x").unwrap();
        std::fs::write(&keep, b"x").unwrap();
        std::fs::write(&old, b"x").unwrap();

        let removed = cleanup_startup_in(&temp, &legacy, std::slice::from_ref(&keep));
        assert_eq!(removed, 2);
        assert!(keep.exists(), "el archivo pegado debe conservarse");
        assert!(!stale.exists());
        assert!(!legacy.exists(), "la carpeta antigua se elimina si queda vacía");
    }

    #[test]
    fn ac2_arranque_sin_referencias_vacia_ambas_carpetas() {
        let base = tempfile::tempdir().unwrap();
        let temp = base.path().join("temp");
        let legacy = base.path().join("legacy");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(temp.join("a.png"), b"x").unwrap();
        std::fs::write(legacy.join("b.png"), b"x").unwrap();

        let removed = cleanup_startup_in(&temp, &legacy, &[]);
        assert_eq!(removed, 2);
        assert!(!temp.join("a.png").exists());
        assert!(!legacy.exists());
        assert!(temp.exists(), "la carpeta nueva se recrea al copiar");
    }

    #[test]
    fn arranque_conserva_un_referenciado_de_la_carpeta_antigua() {
        let base = tempfile::tempdir().unwrap();
        let temp = base.path().join("temp");
        let legacy = base.path().join("legacy");
        std::fs::create_dir_all(&legacy).unwrap();
        let keep = legacy.join("de-v1.png");
        std::fs::write(&keep, b"x").unwrap();

        let removed = cleanup_startup_in(&temp, &legacy, std::slice::from_ref(&keep));
        assert_eq!(removed, 0);
        assert!(keep.exists());
        assert!(legacy.exists(), "no se borra una carpeta con algo pegado");
    }

    #[test]
    fn rutas_comparadas_sin_distinguir_mayusculas() {
        assert!(same_path(Path::new("C:\\Temp\\A.png"), Path::new("c:\\temp\\a.png")));
        assert!(is_within(Path::new("C:\\Temp\\Destello\\a.png"), Path::new("C:\\Temp\\Destello")));
        assert!(!is_within(Path::new("C:\\Temp\\Destello2\\a.png"), Path::new("C:\\Temp\\Destello")));
    }

    #[test]
    fn reintento_de_inspeccion_hasta_cinco_veces() {
        assert_eq!(retry_step(Some(true), 5), (RetryDecision::Keep, 0));
        assert_eq!(retry_step(Some(false), 5), (RetryDecision::Delete, 0));

        let mut remaining = CLIPBOARD_RETRY_ATTEMPTS;
        for attempt in 1..CLIPBOARD_RETRY_ATTEMPTS {
            let (decision, next) = retry_step(None, remaining);
            assert_eq!(decision, RetryDecision::Retry);
            assert_eq!(next, CLIPBOARD_RETRY_ATTEMPTS - attempt);
            remaining = next;
        }
        // Quinta comprobación sin poder inspeccionar: se conserva el archivo.
        assert_eq!(retry_step(None, remaining), (RetryDecision::Keep, 0));
    }

    #[test]
    fn borrado_reintenta_si_el_archivo_esta_bloqueado() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bloqueado.png");
        std::fs::write(&path, b"x").unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        let err = remove_with_retries(&path, 2, Duration::from_millis(1)).unwrap_err();
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(path.exists(), "no debe perderse si sigue bloqueado");

        drop(lock);
        remove_with_retries(&path, 1, Duration::ZERO).unwrap();
        assert!(!path.exists());
    }
}
