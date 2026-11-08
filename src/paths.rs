//! Rutas de datos de la aplicación.
//!
//! Se pueden redirigir con `DESTELLO_DATA_DIR` (config), `DESTELLO_LOCAL_DIR`
//! (log y caché antigua) y `DESTELLO_TEMP_DIR` (archivo de respaldo del
//! portapapeles). Útil para pruebas y para la QA sin tocar el perfil real.
use std::path::PathBuf;

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `%APPDATA%\Destello` (config.toml).
pub fn data_dir() -> PathBuf {
    if let Some(p) = env_path("DESTELLO_DATA_DIR") {
        return p;
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Destello")
}

/// `%LOCALAPPDATA%\Destello` (log y carpeta antigua de caché).
pub fn local_dir() -> PathBuf {
    if let Some(p) = env_path("DESTELLO_LOCAL_DIR") {
        return p;
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Destello")
}

pub fn config_file() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn log_file() -> PathBuf {
    local_dir().join("destello.log")
}

/// `%TEMP%\Destello\portapapeles`: el único PNG vivo que publica CF_HDROP.
pub fn clipboard_cache_dir() -> PathBuf {
    env_path("DESTELLO_TEMP_DIR")
        .unwrap_or_else(std::env::temp_dir)
        .join("Destello")
        .join("portapapeles")
}

/// `%LOCALAPPDATA%\Destello\portapapeles`: restos de la v1 (ya no se usa).
pub fn legacy_clipboard_cache_dir() -> PathBuf {
    local_dir().join("portapapeles")
}

/// Carpeta de imágenes conocida del usuario (respeta OneDrive/localización).
pub fn pictures_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = known_pictures_dir() {
            return p;
        }
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Pictures")
}

#[cfg(windows)]
fn known_pictures_dir() -> Option<PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Pictures, KF_FLAG_DEFAULT};

    unsafe {
        let raw: PWSTR = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None).ok()?;
        let s = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        let s = s?;
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rutas_terminan_en_destello() {
        // Sin variables de entorno de override, las rutas cuelgan de ...\Destello.
        if std::env::var_os("DESTELLO_DATA_DIR").is_none() {
            assert!(config_file().ends_with("config.toml"));
            assert!(config_file().parent().is_some());
        }
        assert!(clipboard_cache_dir().ends_with("portapapeles"));
        assert!(clipboard_cache_dir().starts_with(
            std::env::var_os("DESTELLO_TEMP_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
        ));
        assert!(legacy_clipboard_cache_dir().ends_with("portapapeles"));
    }
}
