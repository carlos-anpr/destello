//! Errores visibles sin toasts: globo de la bandeja + línea en el log.
//!
//! `error` es thread-safe. Desde cualquier hilo encola el mensaje y hace
//! `PostMessage` a la ventana de control; el hilo de UI es el único que toca
//! `Shell_NotifyIconW`. Si todavía no hay ventana (arranque/CLI) solo queda el
//! log.
use std::sync::Mutex;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Mensaje privado que despierta a la ventana de control para mostrar el globo.
pub const WM_APP_NOTIFY: u32 = WM_APP + 5;

const MAX_QUEUED: usize = 8;

static QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Notifica un error real (fallo de captura, portapapeles ocupado, disco lleno…).
/// Nunca se usa para confirmar éxitos: copiar es silencioso.
pub fn error(message: &str) {
    crate::logging::error(message);

    let hwnd = crate::control_hwnd();
    if hwnd.0.is_null() {
        return;
    }

    {
        let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        if queue.len() >= MAX_QUEUED {
            queue.remove(0);
        }
        queue.push(message.to_string());
    }

    let posted = unsafe { PostMessageW(Some(hwnd), WM_APP_NOTIFY, WPARAM(0), LPARAM(0)) };
    if posted.is_err() {
        // La ventana pudo destruirse entre la comprobación y el PostMessage.
        // Mostrar aquí sería tocar Shell_NotifyIcon desde un hilo arbitrario,
        // así que el mensaje queda en el log (ya escrito arriba).
        crate::logging::warn("no se pudo encolar el globo de error");
        let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        queue.retain(|queued| queued != message);
    }
}

/// Vacía la cola y muestra los globos. Se llama desde el wndproc principal.
pub fn drain() {
    let messages: Vec<String> = {
        let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *queue)
    };
    if messages.is_empty() {
        return;
    }
    let hwnd = crate::control_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    for message in messages {
        if !crate::tray::balloon_error(hwnd, &message) {
            crate::logging::warn("no se pudo mostrar el globo de error");
        }
    }
}
