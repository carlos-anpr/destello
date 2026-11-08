#![cfg_attr(not(test), windows_subsystem = "windows")]
//! Destello — arranque, DPI, instancia única, CLI, ventana de control y bucle.
mod app;
mod autostart;
mod capture;
mod cli;
mod clipboard;
mod config;
mod geom;
mod hotkeys;
mod image_ops;
mod logging;
mod notify;
mod output;
mod paths;
mod tray;
mod ui;

use std::io::Write;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
    ERROR_ALREADY_EXISTS,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{
    AttachConsole, FreeConsole, GetStdHandle, WriteConsoleW, ATTACH_PARENT_PROCESS,
    STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, RemoveClipboardFormatListener, COPYDATASTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwareness, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    PROCESS_PER_MONITOR_DPI_AWARE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
    LoadCursorW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SendMessageTimeoutW, SetProcessDPIAware, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, IDC_ARROW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MSG,
    SMTO_ABORTIFHUNG, WM_APP, WM_CLIPBOARDUPDATE, WM_CLOSE, WM_CONTEXTMENU, WM_COPYDATA,
    WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_QUIT, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};

use app::Command;
use cli::Parsed;
use config::Config;

const COPYDATA_MAGIC: usize = 0x4453_544C; // "DSTL"
const WM_APP_DEFERRED: u32 = WM_APP + 3;

static CONTROL_HWND: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static NEXT_PENDING_ID: AtomicU64 = AtomicU64::new(1);
static PENDING: Mutex<Vec<(u64, Command)>> = Mutex::new(Vec::new());

thread_local! {
    static STATE: std::cell::RefCell<Option<AppState>> = const { std::cell::RefCell::new(None) };
}

struct AppState {
    cfg: Config,
    tray: tray::Tray,
    _hotkeys: hotkeys::Hotkeys,
}

fn main() {
    logging::install_panic_hook();
    match entry() {
        Ok(()) => {}
        Err(e) => {
            logging::error(&format!("fallo de arranque: {e:#}"));
            message_box(
                &format!("Destello no pudo iniciar:\n\n{e:#}"),
                MB_OK | MB_ICONERROR,
            );
            std::process::exit(1);
        }
    }
}

fn entry() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (parsed, warnings) = match cli::parse_args_with_warnings(&args) {
        Ok(parsed) => parsed,
        Err(e) => {
            // Error de uso: lo imprime siempre quien invocó, antes de reenviar.
            emit_error(&format!("destello: {e}\n\n{}", cli::HELP));
            std::process::exit(2);
        }
    };

    // Avisos de compatibilidad (--no-save) en el terminal del invocador, no en
    // la instancia residente que recibe la orden por WM_COPYDATA.
    for warning in &warnings {
        emit_warning(&format!("destello: aviso: {warning}\n"));
    }

    match &parsed {
        Parsed::DebugTargets { delay } => {
            if *delay > 0 {
                std::thread::sleep(Duration::from_secs(*delay as u64));
            }
            // UI Automation necesita COM también en el diagnóstico.
            let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            let report = capture::debug_targets_report();
            if com.is_ok() {
                unsafe {
                    CoUninitialize();
                }
            }
            let path = paths::local_dir().join("targets_debug.txt");
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, report.as_bytes()) {
                Ok(()) => logging::info(&format!(
                    "debug-targets escrito en {}",
                    path.display()
                )),
                Err(e) => logging::error(&format!(
                    "no se pudo escribir {}: {e}",
                    path.display()
                )),
            }
            if !write_console(ConsoleStream::Out, &report) {
                logging::warn("debug-targets: no hay consola disponible");
            }
            return Ok(());
        }
        Parsed::Help => {
            emit_stdout(cli::HELP);
            return Ok(());
        }
        Parsed::Version => {
            emit_stdout(&format!("Destello {}\n", env!("CARGO_PKG_VERSION")));
            return Ok(());
        }
        _ => {}
    }

    let (_instance, already_running) = single_instance()?;
    if already_running {
        if matches!(&parsed, Parsed::StartTray) {
            // "destello" con otra instancia viva: no hace nada (SPEC).
            return Ok(());
        }
        if let Parsed::Command(_) = &parsed {
            forward_command(&args)?;
        }
        return Ok(());
    }

    init_process();
    let result = run_app(parsed);
    unsafe {
        CoUninitialize();
    }
    result
}

fn init_process() {
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err()
            && SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE).is_err()
        {
            let _ = SetProcessDPIAware();
        }
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if !hr.is_ok() {
            logging::warn(&format!("CoInitializeEx devolvió {hr:?}"));
        }
    }
    // Contrato con la UI: precalentar D2D/DWrite/WIC justo tras inicializar COM,
    // en el hilo de control, antes de atender capturas.
    #[cfg(feature = "ui")]
    crate::ui::warm_up();
}

fn run_app(parsed: Parsed) -> Result<()> {
    let hinstance = unsafe { GetModuleHandleW(None)? };
    let hinstance = HINSTANCE(hinstance.0);

    // Menús nativos en oscuro si el sistema está en modo oscuro.
    tray::apply_theme();

    unsafe {
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let window_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: cursor,
            lpszClassName: w!("Destello.Control"),
            ..Default::default()
        };
        if RegisterClassExW(&window_class) == 0 {
            return Err(anyhow!("no se pudo registrar la ventana de control"));
        }
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("Destello.Control"),
            w!("Destello"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )?
    };
    CONTROL_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    clipboard::set_owner(hwnd);
    if let Err(e) = unsafe { AddClipboardFormatListener(hwnd) } {
        logging::warn(&format!(
            "no se pudo escuchar los cambios del portapapeles: {e}"
        ));
    }
    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        Ordering::SeqCst,
    );

    // Conserva el respaldo que siga en el portapapeles y borra restos v1.
    output::cleanup_startup();

    let load = Config::load();
    let mut cfg = load.config;

    if load.created || load.corrupt || load.normalized {
        if let Err(e) = cfg.save() {
            logging::error(&format!("no se pudo guardar la config inicial: {e:#}"));
        }
    }

    // Desactivar Recortes con ImpPnt la primera vez.
    let mut snipping_changed = false;
    if !cfg.snipping_fix_applied {
        match hotkeys::disable_snipping() {
            Ok(changed) => {
                snipping_changed = changed;
                cfg.snipping_fix_applied = true;
                if let Err(e) = cfg.save() {
                    logging::warn(&format!("no se pudo guardar snipping_fix_applied: {e:#}"));
                }
            }
            Err(e) => logging::warn(&format!("no se pudo ajustar Recortes: {e:#}")),
        }
    }

    if let Err(e) = autostart::sync(cfg.launch_at_startup) {
        logging::warn(&format!("no se pudo sincronizar el arranque: {e:#}"));
    }

    let tray = tray::Tray::install(hwnd)
        .map_err(|e| anyhow!("no se pudo crear el icono de la bandeja: {e:#}"))?;

    let hotkeys = hotkeys::install(hwnd, &cfg);
    if let Some(error) = &hotkeys.error {
        notify::error(error);
    }
    let hotkey_errors = hotkeys::reload(&cfg);
    if !hotkey_errors.is_empty() {
        let message = format!("Atajos no registrados: {}", hotkey_errors.join(", "));
        notify::error(&message);
    }

    STATE.with(|state| {
        state.borrow_mut().replace(AppState {
            cfg: cfg.clone(),
            tray,
            _hotkeys: hotkeys,
        });
    });

    if load.corrupt {
        logging::warn("la configuración estaba dañada; se usaron valores por defecto");
    }
    if snipping_changed {
        logging::info("Recortes con ImpPnt desactivado");
    }

    if let Parsed::Command(command) = parsed {
        dispatch(command);
    }

    message_loop();

    // Conservar el respaldo si seguimos siendo dueños del portapapeles.
    output::on_exit(hwnd);
    output::stop_clipboard_retry(hwnd);
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
    }

    // Retirar icono/hook de forma ordenada.
    STATE.with(|state| {
        state.borrow_mut().take();
    });
    clipboard::set_owner(HWND::default());
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    CONTROL_HWND.store(0, Ordering::SeqCst);
    Ok(())
}

fn message_loop() {
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == 0 || result.0 == -1 || message.message == WM_QUIT {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        tray::WM_APP_TRAY => {
            handle_tray_event(wparam, lparam);
            LRESULT(0)
        }
        hotkeys::WM_APP_KEYHOOK => {
            if let Some(action) = hotkeys::action_from_wparam(wparam.0) {
                dispatch(action.command());
            }
            LRESULT(0)
        }
        WM_APP_DEFERRED => {
            let id = wparam.0 as u64;
            let command = {
                let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
                let position = pending.iter().position(|(pending_id, _)| *pending_id == id);
                position.map(|position| pending.remove(position).1)
            };
            if let Some(command) = command {
                dispatch(command);
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            if let Some(command) = hotkeys::command_for_hotkey(wparam.0 as i32) {
                dispatch(command);
            }
            LRESULT(0)
        }
        notify::WM_APP_NOTIFY => {
            notify::drain();
            LRESULT(0)
        }
        WM_CLIPBOARDUPDATE => {
            output::handle_clipboard_update(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == output::CLIPBOARD_RETRY_TIMER_ID => {
            output::handle_clipboard_retry(hwnd);
            LRESULT(0)
        }
        WM_COPYDATA => handle_copy_data(hwnd, lparam),
        task if task == TASKBAR_CREATED.load(Ordering::SeqCst) && task != 0 => {
            STATE.with(|state| {
                if let Some(state) = state.borrow_mut().as_mut() {
                    state.tray.re_add();
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            STATE.with(|state| {
                state.borrow_mut().take();
            });
            CONTROL_HWND.store(0, Ordering::SeqCst);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn handle_tray_event(wparam: WPARAM, lparam: LPARAM) {
    let event = (lparam.0 as u32) & 0xFFFF;
    let _ = wparam;
    match event {
        WM_LBUTTONUP => dispatch(Command::CaptureArea { delay: 0 }),
        WM_RBUTTONUP | WM_CONTEXTMENU => show_tray_menu(),
        _ => {}
    }
}

fn show_tray_menu() {
    let hwnd = control_hwnd();
    let Some(cfg) = snapshot_cfg() else {
        return;
    };
    let Some(id) = tray::show_menu(hwnd, &cfg) else {
        return;
    };
    match tray::action_for(id) {
        Some(tray::TrayAction::Run(command)) => dispatch(command),
        Some(tray::TrayAction::ToggleAutostart) => toggle_autostart(),
        None => {}
    }
}

fn toggle_autostart() {
    let Some(mut cfg) = snapshot_cfg() else {
        return;
    };
    cfg.launch_at_startup = !cfg.launch_at_startup;
    match autostart::set(cfg.launch_at_startup) {
        Ok(()) => {
            if let Err(e) = cfg.save() {
                logging::error(&format!("no se pudo guardar el arranque: {e:#}"));
            }
            logging::info(if cfg.launch_at_startup {
                "Destello arrancará con Windows"
            } else {
                "Destello ya no arrancará con Windows"
            });
            store_cfg(cfg.clone());
        }
        Err(e) => {
            notify::error(&format!("No se pudo cambiar el arranque: {e:#}"));
        }
    }
}

unsafe fn handle_copy_data(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    let data = &*(lparam.0 as *const COPYDATASTRUCT);
    if data.dwData != COPYDATA_MAGIC || data.lpData.is_null() || data.cbData == 0 {
        return LRESULT(0);
    }
    let bytes = std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize);
    let text = String::from_utf8_lossy(bytes);
    match cli::parse_line(text.trim_end_matches('\0')) {
        Ok(Parsed::Command(command)) => {
            // Responder rápido al emisor; la captura corre en el bucle de mensajes.
            submit(command, hwnd);
            LRESULT(1)
        }
        Ok(_) => LRESULT(1),
        Err(e) => {
            logging::error(&format!("orden recibida no válida: {e}"));
            LRESULT(1)
        }
    }
}

fn dispatch(command: Command) {
    if command.delay() > 0 {
        schedule(command);
        return;
    }

    let Some(mut cfg) = snapshot_cfg() else {
        return;
    };
    let owner = control_hwnd();
    let outcome = app::dispatch(command, &mut cfg, owner);
    if outcome.config_changed {
        store_cfg(cfg);
    }
    if outcome.quit {
        unsafe {
            PostQuitMessage(0);
        }
    }
}

fn schedule(command: Command) {
    let delay = command.delay().min(3600);
    if delay > 0 {
        logging::info(&format!("captura programada en {delay} s"));
    }
    let immediate = command.without_delay();
    let id = NEXT_PENDING_ID.fetch_add(1, Ordering::SeqCst);
    PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, immediate));

    let hwnd_raw = CONTROL_HWND.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(delay as u64));
        let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
        if !hwnd.0.is_null() {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_APP_DEFERRED, WPARAM(id as usize), LPARAM(0));
            }
        }
    });
}

/// Encola un comando para ejecutarlo en el siguiente ciclo del bucle.
fn submit(command: Command, hwnd: HWND) {
    if command.delay() > 0 {
        schedule(command);
        return;
    }
    let id = NEXT_PENDING_ID.fetch_add(1, Ordering::SeqCst);
    PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, command));
    if hwnd.0.is_null()
        || unsafe {
            PostMessageW(Some(hwnd), WM_APP_DEFERRED, WPARAM(id as usize), LPARAM(0))
        }
        .is_err()
    {
        PENDING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(pending_id, _)| *pending_id != id);
        logging::error("no se pudo encolar el comando recibido");
    }
}

fn snapshot_cfg() -> Option<Config> {
    STATE.with(|state| state.borrow().as_ref().map(|state| state.cfg.clone()))
}

fn store_cfg(cfg: Config) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.cfg = cfg;
        }
    });
}

pub(crate) fn control_hwnd() -> HWND {
    HWND(CONTROL_HWND.load(Ordering::SeqCst) as *mut core::ffi::c_void)
}

struct SingleInstance {
    _handle: HANDLE,
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self._handle);
        }
    }
}

fn single_instance() -> Result<(SingleInstance, bool)> {
    let handle =
        unsafe { CreateMutexW(None, false, w!("Local\\Destello.SingleInstance")) }
            .map_err(|e| anyhow!("no se pudo crear el mutex de instancia única: {e}"))?;
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    Ok((SingleInstance { _handle: handle }, already))
}

fn forward_command(args: &[String]) -> Result<()> {
    let payload = args.join("\n");
    let bytes = payload.as_bytes();
    let data = COPYDATASTRUCT {
        dwData: COPYDATA_MAGIC,
        cbData: bytes.len() as u32,
        lpData: bytes.as_ptr() as *mut core::ffi::c_void,
    };

    let mut hwnd = HWND::default();
    for _ in 0..40 {
        match unsafe { FindWindowW(w!("Destello.Control"), PCWSTR::null()) } {
            Ok(found) if !found.0.is_null() => {
                hwnd = found;
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    if hwnd.0.is_null() {
        return Err(anyhow!(
            "ya hay una instancia de Destello, pero su ventana no responde"
        ));
    }

    let mut result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&data as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            3000,
            Some(&mut result),
        )
    };
    if sent.0 == 0 {
        return Err(anyhow!("la instancia existente no aceptó la orden"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ConsoleStream {
    Out,
    Err,
}

/// Escribe Unicode en el terminal del proceso invocador. `WriteConsoleW`
/// evita depender de la code page; si la salida está redirigida (archivo o
/// tubería) se escriben bytes UTF-8. Devuelve false si no hay salida posible.
fn write_console(stream: ConsoleStream, text: &str) -> bool {
    // En builds GUI no hay consola propia; si nos lanzaron desde una, usarla.
    // Se desengancha al terminar para no quedar atados a esa consola: si el
    // usuario cierra el terminal, la instancia residente no debe terminar.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    let written = write_console_attached(stream, text);
    if attached {
        unsafe {
            let _ = FreeConsole();
        }
    }
    written
}

fn write_console_attached(stream: ConsoleStream, text: &str) -> bool {
    let handle = unsafe {
        GetStdHandle(match stream {
            ConsoleStream::Out => STD_OUTPUT_HANDLE,
            ConsoleStream::Err => STD_ERROR_HANDLE,
        })
    };
    if let Ok(handle) = handle {
        let units: Vec<u16> = text.encode_utf16().collect();
        if unsafe { WriteConsoleW(handle, &units, None, None) }.is_ok() {
            return true;
        }
    }

    match stream {
        ConsoleStream::Out => {
            let mut out = std::io::stdout();
            out.write_all(text.as_bytes())
                .and_then(|_| out.flush())
                .is_ok()
        }
        ConsoleStream::Err => {
            let mut err = std::io::stderr();
            err.write_all(text.as_bytes())
                .and_then(|_| err.flush())
                .is_ok()
        }
    }
}

fn emit_stdout(text: &str) {
    if !write_console(ConsoleStream::Out, text) {
        message_box(text, MB_OK | MB_ICONINFORMATION);
    }
}

fn emit_error(text: &str) {
    // Los errores de uso van a stderr; si no hay, a stdout; si tampoco, diálogo.
    if !write_console(ConsoleStream::Err, text) && !write_console(ConsoleStream::Out, text) {
        message_box(text, MB_OK | MB_ICONERROR);
    }
}

fn emit_warning(text: &str) {
    // Un aviso nunca abre un diálogo modal: si stderr no existe, prueba stdout.
    if !write_console(ConsoleStream::Err, text) {
        let _ = write_console(ConsoleStream::Out, text);
    }
}

fn message_box(text: &str, flags: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE) {
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("Destello"),
            flags,
        );
    }
}
