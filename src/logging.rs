//! Log a `%LOCALAPPDATA%\Destello\destello.log` y hook de pánico.
//!
//! Nunca propaga errores: si no se puede escribir, la aplicación sigue.
use std::io::Write;
use std::sync::Mutex;

use crate::paths;

static LOCK: Mutex<()> = Mutex::new(());

pub fn info(message: &str) {
    write("INFO", message);
}

pub fn warn(message: &str) {
    write("WARN", message);
}

pub fn error(message: &str) {
    write("ERROR", message);
}

fn write(level: &str, message: &str) {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = paths::log_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Rotación simple: 2 MB.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 2_000_000 {
            let old = path.with_extension("log.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&path, &old);
        }
    }

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "{stamp} [{level}] {message}");
    }

    // En builds GUI stderr no va a ningún sitio; nunca debe provocar un panic.
    let _ = writeln!(std::io::stderr(), "[destello] [{level}] {message}");
}

/// Instala el hook que escribe el pánico en el log (con backtrace).
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "pánico sin mensaje".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<desconocido>".to_string());
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("main");
        let backtrace = std::backtrace::Backtrace::force_capture();
        write(
            "PANIC",
            &format!("{payload}\n  hilo: {name}\n  en: {location}\n{backtrace}"),
        );
    }));
}
