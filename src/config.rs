//! Configuración persistente en `%APPDATA%\Destello\config.toml`.
//!
//! Tolerante a corrupción: si el TOML no se puede leer se conserva una copia
//! `.bak` y se arranca con los valores por defecto (nunca se cierra la app).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpg,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
        }
    }
}

fn default_pattern() -> String {
    "Captura {fecha} {hora}".to_string()
}

fn default_quality() -> u8 {
    92
}

fn default_thickness() -> f32 {
    4.0
}

fn default_tool() -> String {
    "pen".to_string()
}

fn default_color() -> u32 {
    0xE5_48_4D
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub filename_pattern: String,
    pub format: ImageFormat,
    pub jpeg_quality: u8,
    /// Última carpeta usada en "Guardar como". `None` = Imágenes.
    pub last_save_dir: Option<PathBuf>,
    pub include_cursor: bool,
    pub show_magnifier: bool, // [ui]
    pub launch_at_startup: bool,
    pub last_color: u32,     // [ui] 0xRRGGBB
    pub last_thickness: f32, // [ui]
    pub last_tool: String,   // [ui]
    pub last_area: Option<[i32; 4]>,
    /// Atajos globales configurables. Cadena vacía = desactivado.
    pub hotkeys: Hotkeys,
    /// Ya se intentó desactivar "Usar Impr Pant para Recortes" (HKCU).
    pub snipping_fix_applied: bool,
}

/// Atajos globales. Formato: `Ctrl+Alt+Shift+Win+Tecla`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hotkeys {
    pub area: String,
    pub full_monitor: String,
    pub all_monitors: String,
    pub active_window: String,
    pub repeat_area: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            area: "PrintScreen".to_string(),
            full_monitor: "Ctrl+PrintScreen".to_string(),
            all_monitors: "Shift+PrintScreen".to_string(),
            active_window: "Ctrl+Shift+PrintScreen".to_string(),
            repeat_area: "Ctrl+Alt+PrintScreen".to_string(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            filename_pattern: default_pattern(),
            format: ImageFormat::default(),
            jpeg_quality: default_quality(),
            last_save_dir: None,
            include_cursor: false,
            show_magnifier: true,
            launch_at_startup: true,
            last_color: default_color(),
            last_thickness: default_thickness(),
            last_tool: default_tool(),
            last_area: None,
            hotkeys: Hotkeys::default(),
            snipping_fix_applied: false,
        }
    }
}

impl Config {
    /// Ruta estándar del fichero de configuración.
    pub fn path() -> PathBuf {
        paths::config_file()
    }

    /// Carga desde la ruta estándar. Nunca falla.
    pub fn load() -> LoadOutcome {
        Self::load_from(&Self::path())
    }

    /// Carga desde una ruta arbitraria. Nunca falla.
    pub fn load_from(path: &Path) -> LoadOutcome {
        let mut outcome = LoadOutcome {
            config: Config::default(),
            created: false,
            corrupt: false,
            normalized: false,
        };

        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                outcome.created = true;
                return outcome;
            }
            Err(err) => {
                // Sin permisos, disco raro... se arranca con defaults.
                crate::logging::error(&format!(
                    "no se pudo leer la configuración {}: {err}",
                    path.display()
                ));
                outcome.created = true;
                return outcome;
            }
        };

        match toml::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                outcome.normalized = cfg.normalize();
                outcome.config = cfg;
            }
            Err(err) => {
                crate::logging::error(&format!(
                    "configuración corrupta ({}): {err}; se usan valores por defecto",
                    path.display()
                ));
                let bak = backup_path(path);
                if let Err(e) = std::fs::rename(path, &bak) {
                    crate::logging::error(&format!(
                        "no se pudo renombrar {} a {}: {e}",
                        path.display(),
                        bak.display()
                    ));
                }
                outcome.corrupt = true;
            }
        }

        outcome
    }

    /// Guarda en la ruta estándar de forma atómica.
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("no se pudo crear {}: {e}", parent.display())
            })?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text.as_bytes())
            .map_err(|e| anyhow::anyhow!("no se pudo escribir {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            anyhow::anyhow!("no se pudo guardar {}: {e}", path.display())
        })?;
        Ok(())
    }

    /// Corrige valores fuera de rango. Devuelve true si cambió algo.
    pub fn normalize(&mut self) -> bool {
        let mut changed = false;
        if self.jpeg_quality == 0 || self.jpeg_quality > 100 {
            self.jpeg_quality = self.jpeg_quality.clamp(1, 100);
            changed = true;
        }
        if self
            .last_save_dir
            .as_ref()
            .is_some_and(|dir| dir.as_os_str().is_empty())
        {
            self.last_save_dir = None;
            changed = true;
        }
        if self.filename_pattern.trim().is_empty() {
            self.filename_pattern = default_pattern();
            changed = true;
        }
        if !self.last_thickness.is_finite() || self.last_thickness <= 0.0 {
            self.last_thickness = default_thickness();
            changed = true;
        }
        if self.last_thickness > 64.0 {
            self.last_thickness = 64.0;
            changed = true;
        }
        if self.last_tool.trim().is_empty() {
            self.last_tool = default_tool();
            changed = true;
        }
        if let Some(area) = self.last_area {
            let [l, t, r, b] = area;
            if r <= l || b <= t {
                self.last_area = None;
                changed = true;
            }
        }
        for shortcut in [
            &mut self.hotkeys.area,
            &mut self.hotkeys.full_monitor,
            &mut self.hotkeys.all_monitors,
            &mut self.hotkeys.active_window,
            &mut self.hotkeys.repeat_area,
        ] {
            let trimmed = shortcut.trim().to_string();
            if *shortcut != trimmed {
                *shortcut = trimmed;
                changed = true;
            }
        }
        changed
    }
}fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
}

/// Resultado de cargar la configuración.
#[derive(Clone, Debug)]
pub struct LoadOutcome {
    pub config: Config,
    /// El fichero no existía (primer arranque).
    pub created: bool,
    /// El fichero estaba corrupto y se movió a `.bak`.
    pub corrupt: bool,
    /// Se corrigieron valores inválidos.
    pub normalized: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn defaults_coherentes() {
        let cfg = Config::default();
        assert_eq!(cfg.format, ImageFormat::Png);
        assert_eq!(cfg.jpeg_quality, 92);
        assert!(cfg.launch_at_startup);
        assert!(cfg.last_save_dir.is_none());
        assert_eq!(cfg.filename_pattern, "Captura {fecha} {hora}");
        assert!(cfg.last_area.is_none());
    }

    #[test]
    fn roundtrip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut cfg = Config {
            format: ImageFormat::Jpg,
            jpeg_quality: 77,
            last_save_dir: Some(dir.path().join("capturas")),
            last_area: Some([10, 20, 300, 400]),
            ..Config::default()
        };
        cfg.hotkeys.area = "Ctrl+Alt+D".into();
        cfg.save_to(&path).unwrap();

        let loaded = Config::load_from(&path);
        assert!(!loaded.created);
        assert!(!loaded.corrupt);
        assert_eq!(loaded.config, cfg);
    }

    #[test]
    fn corrupta_se_respalda_y_usa_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"esto no es ] toml valido").unwrap();
        drop(f);

        let loaded = Config::load_from(&path);
        assert!(loaded.corrupt);
        assert_eq!(loaded.config, Config::default());
        assert!(!path.exists(), "el original debe moverse");
        let mut bak = path.as_os_str().to_os_string();
        bak.push(".bak");
        let bak = PathBuf::from(bak);
        assert!(bak.exists());
        let backup = std::fs::read_to_string(bak).unwrap();
        assert!(backup.contains("esto no es"));
    }

    #[test]
    fn fichero_inexistente_es_primer_arranque() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Config::load_from(&dir.path().join("nope.toml"));
        assert!(loaded.created);
        assert!(!loaded.corrupt);
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn normaliza_valores_invalidos() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
jpeg_quality = 0
last_save_dir = ""
filename_pattern = "   "
last_thickness = -2.0
last_tool = ""
last_area = [10, 10, 5, 5]
campo_desconocido = 42
"#,
        )
        .unwrap();

        let loaded = Config::load_from(&path);
        assert!(loaded.normalized);
        assert_eq!(loaded.config.jpeg_quality, 1);
        assert_eq!(loaded.config.filename_pattern, "Captura {fecha} {hora}");
        assert_eq!(loaded.config.last_tool, "pen");
        assert!(loaded.config.last_area.is_none());
        assert!(loaded.config.last_save_dir.is_none());
    }

    #[test]
    fn campos_ausentes_se_rellenan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "format = \"jpg\"\n").unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded.config.format, ImageFormat::Jpg);
        assert_eq!(loaded.config.jpeg_quality, 92);
        assert!(loaded.config.last_save_dir.is_none());
    }

    #[test]
    fn config_vieja_ignora_campos_eliminados() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
save_dir = "C:\\Users\\usuario\\Pictures\\Destello"
save_on_copy = false
show_toasts = false
format = "jpg"
jpeg_quality = 80
"#,
        )
        .unwrap();

        let loaded = Config::load_from(&path);
        assert!(!loaded.corrupt, "los campos viejos no invalidan la config");
        assert_eq!(loaded.config.format, ImageFormat::Jpg);
        assert_eq!(loaded.config.jpeg_quality, 80);
        assert!(loaded.config.last_save_dir.is_none());
    }

    #[test]
    fn last_save_dir_ida_y_vuelta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = Config {
            last_save_dir: Some(dir.path().join("Mis capturas")),
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded.config.last_save_dir, cfg.last_save_dir);
    }

    #[test]
    fn guardar_crea_carpetas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/config.toml");
        Config::default().save_to(&path).unwrap();
        assert!(path.exists());
        assert!(!dir.path().join("a/b/c/config.toml.tmp").exists());
    }
}
