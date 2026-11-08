//! Icono de bandeja, menú contextual y globos de notificación.
use anyhow::{anyhow, Result};
use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NOTIFYICONDATAW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_DATA_FLAGS, NOTIFY_ICON_INFOTIP_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetSystemMetrics, LoadIconW, LoadImageW,
    PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON,
    LR_DEFAULTCOLOR, LR_SHARED, MENU_ITEM_FLAGS, MF_CHECKED, MF_SEPARATOR, MF_STRING, SM_CXSMICON,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

use crate::app::{Command, MonitorSelection};
use crate::config::Config;
use crate::output::DeliveryPlan;

/// Mensaje de callback del icono de bandeja.
pub const WM_APP_TRAY: u32 = WM_APP + 1;

const TRAY_ID: u32 = 1;
const TRAY_TIP: &str = "Destello — capturas de pantalla";

// IDs del menú (coinciden con `action_for`).
pub const ID_AREA: usize = 1001;
pub const ID_FULL: usize = 1002;
pub const ID_WINDOW: usize = 1003;
pub const ID_DELAY_3: usize = 1004;
pub const ID_DELAY_5: usize = 1005;
pub const ID_DELAY_10: usize = 1006;
pub const ID_REPEAT: usize = 1007;
pub const ID_SETTINGS: usize = 1009;
pub const ID_AUTOSTART: usize = 1010;
pub const ID_QUIT: usize = 1011;

/// Acción elegida en el menú de bandeja.
#[derive(Clone, Debug)]
pub enum TrayAction {
    Run(Command),
    ToggleAutostart,
}

pub fn action_for(id: usize) -> Option<TrayAction> {
    let command = match id {
        ID_AREA => Command::CaptureArea { delay: 0 },
        ID_FULL => Command::CaptureMonitors {
            selection: MonitorSelection::Cursor,
            delay: 0,
            plan: DeliveryPlan::copy_only(),
        },
        ID_WINDOW => Command::CaptureWindow {
            delay: 0,
            plan: DeliveryPlan::copy_only(),
        },
        ID_DELAY_3 => Command::CaptureArea { delay: 3 },
        ID_DELAY_5 => Command::CaptureArea { delay: 5 },
        ID_DELAY_10 => Command::CaptureArea { delay: 10 },
        ID_REPEAT => Command::RepeatLastArea,
        ID_SETTINGS => Command::OpenSettings,
        ID_QUIT => Command::Quit,
        ID_AUTOSTART => return Some(TrayAction::ToggleAutostart),
        _ => return None,
    };
    Some(TrayAction::Run(command))
}

pub struct Tray {
    hwnd: HWND,
    hicon: HICON,
    added: bool,
}

impl Tray {
    pub fn install(hwnd: HWND) -> Result<Self> {
        let hicon = load_icon()?;
        let mut tray = Self {
            hwnd,
            hicon,
            added: false,
        };
        tray.add()?;
        Ok(tray)
    }

    fn add(&mut self) -> Result<()> {
        let data = self.data(NIF_MESSAGE | NIF_ICON | NIF_TIP);
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        if !ok {
            return Err(anyhow!("Shell_NotifyIconW(NIM_ADD) falló"));
        }
        self.added = true;
        Ok(())
    }

    /// Vuelve a añadir el icono tras reiniciarse el Explorador.
    pub fn re_add(&mut self) {
        self.added = false;
        if let Err(e) = self.add() {
            crate::logging::error(&format!("no se pudo readd el icono: {e}"));
        }
    }

    pub fn remove(&mut self) {
        if !self.added {
            return;
        }
        let data = self.data(NOTIFY_ICON_DATA_FLAGS(0));
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        self.added = false;
    }

    fn data(&self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            uFlags: flags,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.hicon,
            szTip: fixed::<128>(TRAY_TIP),
            ..Default::default()
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Globo para un HWND que ya tiene icono de bandeja (usado por notify.rs).
pub fn show_balloon(
    hwnd: HWND,
    title: &str,
    text: &str,
    flags: NOTIFY_ICON_INFOTIP_FLAGS,
) -> bool {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_INFO,
        szInfoTitle: fixed::<64>(title),
        szInfo: fixed::<256>(text),
        dwInfoFlags: flags,
        ..Default::default()
    };
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool()
}

pub fn balloon_error(hwnd: HWND, text: &str) -> bool {
    show_balloon(hwnd, "Destello", text, NIIF_ERROR)
}

/// Activa el modo oscuro de los menús nativos si el sistema usa apps oscuras.
///
/// Usa los ordinales no documentados de uxtheme:
/// 135 = SetPreferredAppMode(AllowDark), 136 = FlushMenuThemes.
pub fn apply_theme() {
    unsafe {
        if !system_uses_dark_apps() {
            return;
        }
        let Ok(uxtheme) = GetModuleHandleW(w!("uxtheme.dll"))
            .or_else(|_| LoadLibraryW(w!("uxtheme.dll")))
        else {
            return;
        };
        if let Some(set) = GetProcAddress(uxtheme, PCSTR(std::ptr::without_provenance::<u8>(135))) {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set);
            let _ = set(1); // AllowDark
        }
        if let Some(flush) =
            GetProcAddress(uxtheme, PCSTR(std::ptr::without_provenance::<u8>(136)))
        {
            let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}

fn system_uses_dark_apps() -> bool {
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    status.0 == 0 && value == 0
}

fn display_shortcut(spec: &str) -> String {
    spec.trim()
        .split('+')
        .map(|token| {
            if token.trim().eq_ignore_ascii_case("printscreen") {
                "ImpPnt"
            } else {
                token.trim()
            }
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join("+")
}

fn menu_text(label: &str, shortcut: &str) -> String {
    let shortcut = display_shortcut(shortcut);
    if shortcut.is_empty() {
        label.to_string()
    } else {
        format!("{label}\t{shortcut}")
    }
}

unsafe fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
}

/// Muestra el menú contextual y devuelve el ID elegido (None si se cancela).
pub fn show_menu(hwnd: HWND, cfg: &Config) -> Option<usize> {
    apply_theme();
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        append(
            menu,
            MF_STRING,
            ID_AREA,
            &menu_text("Capturar área", &cfg.hotkeys.area),
        );
        append(
            menu,
            MF_STRING,
            ID_FULL,
            &menu_text("Pantalla completa", &cfg.hotkeys.full_monitor),
        );
        append(
            menu,
            MF_STRING,
            ID_WINDOW,
            &menu_text("Ventana activa", &cfg.hotkeys.active_window),
        );
        append(menu, MF_STRING, ID_DELAY_3, "Capturar en 3 s");
        append(menu, MF_STRING, ID_DELAY_5, "Capturar en 5 s");
        append(menu, MF_STRING, ID_DELAY_10, "Capturar en 10 s");
        append(
            menu,
            MF_STRING,
            ID_REPEAT,
            &menu_text("Repetir última área", &cfg.hotkeys.repeat_area),
        );
        append(menu, MF_SEPARATOR, 0, "");
        append(menu, MF_STRING, ID_SETTINGS, "Ajustes…");
        let flags = if cfg.launch_at_startup {
            MENU_ITEM_FLAGS(MF_STRING.0 | MF_CHECKED.0)
        } else {
            MF_STRING
        };
        append(menu, flags, ID_AUTOSTART, "Iniciar con Windows");
        append(menu, MF_SEPARATOR, 0, "");
        append(menu, MF_STRING, ID_QUIT, "Salir");

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let result = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        let id = result.0;
        if id > 0 {
            Some(id as usize)
        } else {
            None
        }
    }
}

fn load_icon() -> Result<HICON> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let loaded = LoadImageW(
            Some(windows::Win32::Foundation::HINSTANCE(hinstance.0)),
            PCWSTR(std::ptr::without_provenance::<u16>(1)), // ID del icono en assets/app.rc
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR | LR_SHARED,
        );
        if let Ok(handle) = loaded {
            if !handle.0.is_null() {
                return Ok(HICON(handle.0));
            }
        }
        LoadIconW(None, IDI_APPLICATION).map_err(|e| anyhow!("no se pudo cargar el icono: {e}"))
    }
}

fn fixed<const N: usize>(text: &str) -> [u16; N] {
    let mut out = [0u16; N];
    for (slot, unit) in out.iter_mut().take(N.saturating_sub(1)).zip(text.encode_utf16()) {
        *slot = unit;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_muestra_atajos_con_tab() {
        assert_eq!(
            menu_text("Capturar área", "PrintScreen"),
            "Capturar área\tImpPnt"
        );
        assert_eq!(
            menu_text("Pantalla completa", "Ctrl+PrintScreen"),
            "Pantalla completa\tCtrl+ImpPnt"
        );
        assert_eq!(
            menu_text("Ventana activa", "Ctrl+Shift+PrintScreen"),
            "Ventana activa\tCtrl+Shift+ImpPnt"
        );
        assert_eq!(menu_text("Ajustes…", ""), "Ajustes…");
        assert_eq!(menu_text("Ajustes…", "   "), "Ajustes…");
    }
}
