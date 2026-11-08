//! Arranque con Windows mediante `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
use anyhow::{anyhow, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const RUN_SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("Destello");

/// ¿Está Destello registrado para arrancar con Windows?
pub fn is_enabled() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};

    unsafe {
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_SUBKEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        );
        status.0 == ERROR_SUCCESS.0
    }
}

/// Alias usado por el dispatcher.
pub fn set(enabled: bool) -> Result<()> {
    set_enabled(enabled)
}

/// Activa o desactiva el arranque automático. Idempotente.
pub fn set_enabled(enabled: bool) -> Result<()> {
    if enabled {
        let exe = std::env::current_exe()
            .map_err(|e| anyhow!("no se pudo obtener la ruta del ejecutable: {e}"))?;
        let command = format!("\"{}\"", exe.display());
        let mut units: Vec<u16> = command.encode_utf16().collect();
        units.push(0);
        let bytes = unsafe {
            std::slice::from_raw_parts(units.as_ptr() as *const u8, units.len() * 2)
        };

        unsafe {
            let mut key = HKEY::default();
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_SUBKEY,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            );
            if status.0 != ERROR_SUCCESS.0 {
                return Err(anyhow!("no se pudo abrir la clave Run (código {})", status.0));
            }
            let set = RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes));
            let _ = RegCloseKey(key);
            if set.0 != ERROR_SUCCESS.0 {
                return Err(anyhow!(
                    "no se pudo escribir el valor de arranque (código {})",
                    set.0
                ));
            }
        }
        Ok(())
    } else {
        unsafe {
            let mut key = HKEY::default();
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                RUN_SUBKEY,
                None,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                &mut key,
            );
            match status.0 {
                0 => {}
                code if code == ERROR_FILE_NOT_FOUND.0 => return Ok(()),
                code => return Err(anyhow!("no se pudo abrir la clave Run (código {code})")),
            }
            let deleted = RegDeleteValueW(key, VALUE_NAME);
            let _ = RegCloseKey(key);
            if deleted.0 == ERROR_SUCCESS.0 || deleted.0 == ERROR_FILE_NOT_FOUND.0 {
                Ok(())
            } else {
                Err(anyhow!(
                    "no se pudo borrar el valor de arranque (código {})",
                    deleted.0
                ))
            }
        }
    }
}

/// Deja el registro como indica `enabled` (repara rutas antiguas).
pub fn sync(enabled: bool) -> Result<bool> {
    let before = is_enabled();
    if before == enabled {
        if enabled {
            // Aunque exista, puede apuntar a otra ruta: reescribimos.
            set_enabled(true)?;
        }
        return Ok(false);
    }
    set_enabled(enabled)?;
    Ok(true)
}
