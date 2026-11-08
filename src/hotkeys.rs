//! Hook global `WH_KEYBOARD_LL` para ImpPnt + RegisterHotKey para el resto.
//!
//! Los atajos se leen de `Config::hotkeys` y se pueden recargar en caliente
//! desde Ajustes. `parse_hotkey` / `format_hotkey` son la base del capturador
//! de teclas de la UI.
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Registry::{
    RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
    MOD_SHIFT, MOD_WIN, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PAUSE, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_RWIN, VK_SHIFT, VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
    KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::app::{Command, MonitorSelection};
use crate::config::Config;
use crate::output::DeliveryPlan;

/// Mensaje que el hook envía a la ventana de control con la acción a ejecutar.
pub const WM_APP_KEYHOOK: u32 = WM_APP + 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum HotkeyAction {
    Area = 1,
    FullMonitor = 2,
    AllMonitors = 3,
    ActiveWindow = 4,
    RepeatArea = 5,
}

impl HotkeyAction {
    pub fn command(self) -> Command {
        match self {
            Self::Area => Command::CaptureArea { delay: 0 },
            Self::FullMonitor => Command::CaptureMonitors {
                selection: MonitorSelection::Cursor,
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            },
            Self::AllMonitors => Command::CaptureMonitors {
                selection: MonitorSelection::All,
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            },
            Self::ActiveWindow => Command::CaptureWindow {
                delay: 0,
                plan: DeliveryPlan::copy_only(),
            },
            Self::RepeatArea => Command::RepeatLastArea,
        }
    }

    fn from_wparam(value: usize) -> Option<Self> {
        match value {
            1 => Some(Self::Area),
            2 => Some(Self::FullMonitor),
            3 => Some(Self::AllMonitors),
            4 => Some(Self::ActiveWindow),
            5 => Some(Self::RepeatArea),
            _ => None,
        }
    }
}

struct Binding {
    action: HotkeyAction,
    mods: u32,
    vk: u32,
}

static TARGET: AtomicIsize = AtomicIsize::new(0);
static BINDINGS: Mutex<Vec<Binding>> = Mutex::new(Vec::new());
static REGISTERED: Mutex<Vec<i32>> = Mutex::new(Vec::new());
static REGISTERED_SPECS: Mutex<Vec<(i32, u32, u32)>> = Mutex::new(Vec::new());
static SNAPSHOT_DOWN: AtomicBool = AtomicBool::new(false);
static SUSPENDED: AtomicBool = AtomicBool::new(false);
static AREA_TRIGGER: Mutex<Option<Instant>> = Mutex::new(None);

/// Momento en que el hook recibió ImpPnt para una captura de área.
/// `main/app` lo consume para medir la latencia total hasta `run_overlay`.
pub fn take_area_trigger() -> Option<Instant> {
    AREA_TRIGGER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

const REGISTER_IDS: [i32; 5] = [210, 211, 212, 213, 214];

/// Deja pasar todas las teclas (para que Ajustes pueda capturar ImpPnt).
pub fn suspend() {
    SUSPENDED.store(true, Ordering::SeqCst);
    SNAPSHOT_DOWN.store(false, Ordering::SeqCst);
    *AREA_TRIGGER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let hwnd = HWND(TARGET.load(Ordering::SeqCst) as *mut core::ffi::c_void);
    for id in REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
    }
}

/// Reactiva el hook y vuelve a registrar los atajos que había.
pub fn resume() {
    SUSPENDED.store(false, Ordering::SeqCst);
    SNAPSHOT_DOWN.store(false, Ordering::SeqCst);
    *AREA_TRIGGER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let hwnd = HWND(TARGET.load(Ordering::SeqCst) as *mut core::ffi::c_void);
    if hwnd.0.is_null() {
        return;
    }
    let specs = REGISTERED_SPECS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    for (id, mods, vk) in specs {
        match unsafe {
            RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(mods | 0x4000), vk)
        } {
            Ok(()) => REGISTERED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(id),
            Err(e) => crate::logging::warn(&format!(
                "no se pudo reactivar el atajo {id}: {e}"
            )),
        }
    }
}

/// (Re)registra todos los atajos según la configuración.
/// Devuelve las especificaciones que no se pudieron registrar.
pub fn reload(cfg: &Config) -> Vec<String> {
    let hwnd = HWND(TARGET.load(Ordering::SeqCst) as *mut core::ffi::c_void);

    for id in REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
    }
    REGISTERED_SPECS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    let mut errors = Vec::new();
    let mut bindings = Vec::new();
    let entries: [(HotkeyAction, &str); 5] = [
        (HotkeyAction::Area, cfg.hotkeys.area.as_str()),
        (HotkeyAction::FullMonitor, cfg.hotkeys.full_monitor.as_str()),
        (HotkeyAction::AllMonitors, cfg.hotkeys.all_monitors.as_str()),
        (HotkeyAction::ActiveWindow, cfg.hotkeys.active_window.as_str()),
        (HotkeyAction::RepeatArea, cfg.hotkeys.repeat_area.as_str()),
    ];

    for (index, (action, spec)) in entries.into_iter().enumerate() {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let Some((mods, vk)) = parse_hotkey(spec) else {
            errors.push(format!("{spec} (formato no válido)"));
            continue;
        };
        if vk == VK_SNAPSHOT.0 as u32 {
            // ImpPnt lo gestiona el hook de bajo nivel.
            bindings.push(Binding {
                action,
                mods,
                vk,
            });
        } else {
            let id = REGISTER_IDS[index];
            REGISTERED_SPECS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((id, mods, vk));
            if SUSPENDED.load(Ordering::SeqCst) {
                // Ajustes está capturando teclas: no registrar todavía.
                continue;
            }
            let result = unsafe {
                RegisterHotKey(
                    Some(hwnd),
                    id,
                    HOT_KEY_MODIFIERS(mods | 0x4000), // MOD_NOREPEAT
                    vk,
                )
            };
            match result {
                Ok(()) => REGISTERED
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(id),
                Err(_) => errors.push(format!("{spec} (ocupado o no disponible)")),
            }
        }
    }

    *BINDINGS.lock().unwrap_or_else(|e| e.into_inner()) = bindings;
    errors
}

/// Comando asociado a un WM_HOTKEY.
pub fn command_for_hotkey(id: i32) -> Option<Command> {
    if SUSPENDED.load(Ordering::SeqCst) {
        return None;
    }
    let index = REGISTER_IDS.iter().position(|x| *x == id)?;
    let action = match index {
        0 => HotkeyAction::Area,
        1 => HotkeyAction::FullMonitor,
        2 => HotkeyAction::AllMonitors,
        3 => HotkeyAction::ActiveWindow,
        _ => HotkeyAction::RepeatArea,
    };
    Some(action.command())
}

/// Acción recibida por WM_APP_KEYHOOK.
pub fn action_from_wparam(value: usize) -> Option<HotkeyAction> {
    HotkeyAction::from_wparam(value)
}

pub struct Hotkeys {
    hook: Option<windows::Win32::UI::WindowsAndMessaging::HHOOK>,
    /// Error al instalar el hook (la app sigue funcionando con RegisterHotKey).
    pub error: Option<String>,
}

/// Instala el hook de teclado. Devuelve un guard que lo retira al soltarse.
pub fn install(hwnd: HWND, _cfg: &Config) -> Hotkeys {
    TARGET.store(hwnd.0 as isize, Ordering::SeqCst);
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
        Ok(hook) => Hotkeys {
            hook: Some(hook),
            error: None,
        },
        Err(e) => {
            let message = format!("no se pudo instalar el hook de teclado: {e}");
            crate::logging::error(&message);
            Hotkeys {
                hook: None,
                error: Some(message),
            }
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        if let Some(hook) = self.hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        let hwnd = HWND(TARGET.load(Ordering::SeqCst) as *mut core::ffi::c_void);
        for id in REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            unsafe {
                let _ = UnregisterHotKey(Some(hwnd), id);
            }
        }
        BINDINGS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        SNAPSHOT_DOWN.store(false, Ordering::SeqCst);
        *AREA_TRIGGER.lock().unwrap_or_else(|e| e.into_inner()) = None;
        TARGET.store(0, Ordering::SeqCst);
    }
}

unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code != HC_ACTION as i32 || SUSPENDED.load(Ordering::SeqCst) {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let message = wparam.0 as u32;
    let is_down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
    let is_up = message == WM_KEYUP || message == WM_SYSKEYUP;
    if !is_down && !is_up {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let is_snapshot = kb.vkCode == VK_SNAPSHOT.0 as u32;

    // El keyup se traga si el keydown se tragó, aunque el usuario haya soltado
    // antes los modificadores (si no, el flag quedaría atascado).
    if is_snapshot && is_up && SNAPSHOT_DOWN.swap(false, Ordering::SeqCst) {
        return LRESULT(1);
    }

    let mods = current_mods();
    let action = {
        let bindings = BINDINGS.lock().unwrap_or_else(|e| e.into_inner());
        bindings
            .iter()
            .find(|b| b.vk == kb.vkCode && b.mods == mods)
            .map(|b| b.action)
    };
    let Some(action) = action else {
        return CallNextHookEx(None, code, wparam, lparam);
    };

    if is_down && is_snapshot && !SNAPSHOT_DOWN.swap(true, Ordering::SeqCst) {
        if action == HotkeyAction::Area {
            *AREA_TRIGGER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
        let hwnd = HWND(TARGET.load(Ordering::SeqCst) as *mut core::ffi::c_void);
        if !hwnd.0.is_null() {
            let posted = unsafe {
                PostMessageW(
                    Some(hwnd),
                    WM_APP_KEYHOOK,
                    WPARAM(action as usize),
                    LPARAM(0),
                )
            };
            if posted.is_err() {
                *AREA_TRIGGER.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
    }
    // Se traga la tecla (down y up) para que Recortes/Windows no la vean.
    LRESULT(1)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

fn current_mods() -> u32 {
    let mut mods = 0;
    if key_down(VK_CONTROL) {
        mods |= MOD_CONTROL.0;
    }
    if key_down(VK_MENU) {
        mods |= MOD_ALT.0;
    }
    if key_down(VK_SHIFT) {
        mods |= MOD_SHIFT.0;
    }
    if key_down(VK_LWIN) || key_down(VK_RWIN) {
        mods |= MOD_WIN.0;
    }
    mods
}

/// Desactiva "Usar Impr Pant para abrir Recortes". Devuelve true si cambió.
pub fn disable_snipping() -> Result<bool> {
    let subkey = w!("Control Panel\\Keyboard");
    let value_name = w!("PrintScreenKeyForSnippingEnabled");

    unsafe {
        let mut value: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            value_name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        );
        // Si no existe, Windows lo trata como activado: también lo ponemos a 0.
        if status.0 == 0 && value == 0 {
            return Ok(false);
        }

        let new_value: u32 = 0;
        let set = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            subkey,
            value_name,
            REG_DWORD.0,
            Some(&new_value as *const u32 as *const core::ffi::c_void),
            std::mem::size_of::<u32>() as u32,
        );
        if set.0 != 0 {
            return Err(anyhow!(
                "no se pudo desactivar Recortes con ImpPnt (código {})",
                set.0
            ));
        }
        Ok(true)
    }
}

/// Parsea "Ctrl+Alt+Shift+Win+Tecla" (orden libre, nombres en español o inglés).
pub fn parse_hotkey(spec: &str) -> Option<(u32, u32)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    let parts: Vec<&str> = spec.split('+').collect();
    let mut mods = 0u32;
    let mut key: Option<u32> = None;

    for (index, raw) in parts.iter().enumerate() {
        let token = raw.trim();
        if token.is_empty() {
            return None;
        }
        let upper = token.to_uppercase();
        if index + 1 == parts.len() {
            key = key_code(&upper);
        } else {
            match upper.as_str() {
                "CTRL" | "CONTROL" => mods |= MOD_CONTROL.0,
                "ALT" => mods |= MOD_ALT.0,
                "SHIFT" => mods |= MOD_SHIFT.0,
                "WIN" | "WINDOWS" | "SUPER" | "META" => mods |= MOD_WIN.0,
                _ => return None,
            }
        }
    }

    key.map(|vk| (mods, vk))
}

/// Formatea modificadores + tecla en el orden Ctrl+Alt+Shift+Win.
pub fn format_hotkey(mods: u32, vk: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if mods & MOD_CONTROL.0 != 0 {
        parts.push("Ctrl".to_string());
    }
    if mods & MOD_ALT.0 != 0 {
        parts.push("Alt".to_string());
    }
    if mods & MOD_SHIFT.0 != 0 {
        parts.push("Shift".to_string());
    }
    if mods & MOD_WIN.0 != 0 {
        parts.push("Win".to_string());
    }
    parts.push(key_name(vk));
    parts.join("+")
}

fn key_code(token: &str) -> Option<u32> {
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(rest) = token.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    Some(match token {
        "PRINTSCREEN" | "PRTSC" | "IMPRPANT" | "IMPPNT" | "SNAPSHOT" => VK_SNAPSHOT.0 as u32,
        "INSERT" | "INS" | "INSERTAR" => VK_INSERT.0 as u32,
        "DELETE" | "DEL" | "SUPR" | "SUPRIMIR" => VK_DELETE.0 as u32,
        "HOME" | "INICIO" => VK_HOME.0 as u32,
        "END" | "FIN" => VK_END.0 as u32,
        "PAGEUP" | "PGUP" | "REPAG" => VK_PRIOR.0 as u32,
        "PAGEDOWN" | "PGDN" | "AVPAG" => VK_NEXT.0 as u32,
        "PAUSE" | "PAUSA" => VK_PAUSE.0 as u32,
        "TAB" | "TABULADOR" => VK_TAB.0 as u32,
        "SPACE" | "ESPACIO" => VK_SPACE.0 as u32,
        "ESC" | "ESCAPE" => VK_ESCAPE.0 as u32,
        "ENTER" | "INTRO" | "RETURN" => VK_RETURN.0 as u32,
        "BACKSPACE" | "BACK" | "RETROCESO" => VK_BACK.0 as u32,
        "UP" | "ARRIBA" => VK_UP.0 as u32,
        "DOWN" | "ABAJO" => VK_DOWN.0 as u32,
        "LEFT" | "IZQUIERDA" => VK_LEFT.0 as u32,
        "RIGHT" | "DERECHA" => VK_RIGHT.0 as u32,
        _ => return None,
    })
}

fn key_name(vk: u32) -> String {
    if (0x41..=0x5A).contains(&vk) || (0x30..=0x39).contains(&vk) {
        if let Some(c) = char::from_u32(vk) {
            return c.to_string();
        }
    }
    if (0x70..=0x87).contains(&vk) {
        return format!("F{}", vk - 0x70 + 1);
    }
    let name = match VIRTUAL_KEY(vk as u16) {
        VK_SNAPSHOT => "PrintScreen",
        VK_INSERT => "Insert",
        VK_DELETE => "Delete",
        VK_HOME => "Home",
        VK_END => "End",
        VK_PRIOR => "PageUp",
        VK_NEXT => "PageDown",
        VK_PAUSE => "Pause",
        VK_TAB => "Tab",
        VK_SPACE => "Space",
        VK_ESCAPE => "Escape",
        VK_RETURN => "Enter",
        VK_BACK => "Backspace",
        VK_UP => "Up",
        VK_DOWN => "Down",
        VK_LEFT => "Left",
        VK_RIGHT => "Right",
        _ => return format!("VK_{vk:02X}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_atajos_basicos() {
        assert_eq!(
            parse_hotkey("PrintScreen"),
            Some((0, VK_SNAPSHOT.0 as u32))
        );
        assert_eq!(
            parse_hotkey("Ctrl+PrintScreen"),
            Some((MOD_CONTROL.0, VK_SNAPSHOT.0 as u32))
        );
        assert_eq!(
            parse_hotkey("Ctrl+Shift+PrintScreen"),
            Some((MOD_CONTROL.0 | MOD_SHIFT.0, VK_SNAPSHOT.0 as u32))
        );
        assert_eq!(
            parse_hotkey("ctrl + alt + d"),
            Some((MOD_CONTROL.0 | MOD_ALT.0, b'D' as u32))
        );
        assert_eq!(
            parse_hotkey("Shift+F12"),
            Some((MOD_SHIFT.0, 0x7B))
        );
        assert_eq!(
            parse_hotkey("Win+Alt+7"),
            Some((MOD_WIN.0 | MOD_ALT.0, b'7' as u32))
        );
    }

    #[test]
    fn rechaza_formatos_invalidos() {
        for bad in ["", "   ", "Ctrl", "Ctrl+", "+D", "Ctrl+D+E", "Ctrl+Ñ", "F25", "Ctrl+0x41"] {
            assert!(parse_hotkey(bad).is_none(), "debería rechazar {bad:?}");
        }
    }

    #[test]
    fn formatea_en_orden_canonico() {
        let mods = MOD_WIN.0 | MOD_SHIFT.0 | MOD_ALT.0 | MOD_CONTROL.0;
        assert_eq!(
            format_hotkey(mods, VK_SNAPSHOT.0 as u32),
            "Ctrl+Alt+Shift+Win+PrintScreen"
        );
        assert_eq!(format_hotkey(MOD_CONTROL.0, b'C' as u32), "Ctrl+C");
        assert_eq!(format_hotkey(0, 0x70), "F1");
        assert_eq!(format_hotkey(0, 0xFF), "VK_FF");
    }

    #[test]
    fn ida_y_vuelta() {
        for spec in [
            "PrintScreen",
            "Ctrl+PrintScreen",
            "Shift+PrintScreen",
            "Ctrl+Shift+PrintScreen",
            "Ctrl+Alt+PrintScreen",
            "Ctrl+Alt+Shift+Win+F24",
            "Alt+Insert",
            "Ctrl+PageDown",
        ] {
            let (mods, vk) = parse_hotkey(spec).unwrap_or_else(|| panic!("{spec}"));
            let formatted = format_hotkey(mods, vk);
            assert_eq!(parse_hotkey(&formatted), Some((mods, vk)));
        }
    }

    #[test]
    fn defaults_de_config_son_parseables() {
        let cfg = Config::default();
        for spec in [
            &cfg.hotkeys.area,
            &cfg.hotkeys.full_monitor,
            &cfg.hotkeys.all_monitors,
            &cfg.hotkeys.active_window,
            &cfg.hotkeys.repeat_area,
        ] {
            assert!(parse_hotkey(spec).is_some(), "no parsea {spec}");
        }
    }
}
