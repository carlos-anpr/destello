//! Portapapeles: imagen en PNG + CF_DIBV5 + CF_DIB + CF_HDROP + Preferred
//! DropEffect (COPY). Así Ctrl+V pega un `.png` en el Explorador y la imagen en
//! Paint, Word, navegadores, Teams, Discord…
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{
    BI_BITFIELDS, BI_RGB, BITMAPINFOHEADER, BITMAPV5HEADER,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY};
use windows::Win32::UI::Shell::DROPFILES;

use crate::image_ops::{self, RgbaImage};

const OPEN_RETRIES: usize = 10;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(30);

/// Ventana de control que actúa como dueña del portapapeles.
static OWNER: AtomicIsize = AtomicIsize::new(0);

/// La ventana oculta de main.rs se registra aquí para que `OpenClipboard`
/// tenga un dueño real (con NULL, `EmptyClipboard` deja el portapapeles sin
/// dueño y algunos `SetClipboardData` pueden fallar).
pub fn set_owner(hwnd: HWND) {
    OWNER.store(hwnd.0 as isize, Ordering::SeqCst);
}

fn owner() -> Option<HWND> {
    let raw = OWNER.load(Ordering::SeqCst);
    if raw == 0 {
        None
    } else {
        Some(HWND(raw as *mut core::ffi::c_void))
    }
}

/// Dueño actual del portapapeles (HWND nulo si no lo hay). No abre el
/// portapapeles: es seguro llamarlo desde cualquier hilo.
pub fn owner_hwnd() -> HWND {
    unsafe { GetClipboardOwner() }.unwrap_or_default()
}

/// Deja la imagen en el portapapeles con todos los formatos.
///
/// `file_for_paste` debe ser un PNG ya escrito: se publica como CF_HDROP para
/// que Ctrl+V en una carpeta del Explorador cree el archivo.
pub fn copy_image(img: &RgbaImage, file_for_paste: &Path) -> Result<()> {
    if img.is_empty() {
        return Err(anyhow!("no se puede copiar una imagen vacía"));
    }
    if !file_for_paste.is_file() {
        return Err(anyhow!(
            "el PNG para pegar como archivo no existe: {}",
            file_for_paste.display()
        ));
    }

    let png = image_ops::encode_png(img)?;
    let dib_v5 = dib_v5(img);
    let dib = dib_v1(img);
    let hdrop = hdrop_bytes(file_for_paste)?;
    let drop_effect = DROPEFFECT_COPY.0.to_ne_bytes();

    let clipboard = ClipboardGuard::open()?;

    let png_format = clipboard_format(w!("PNG"))?;
    let drop_effect_format = clipboard_format(w!("Preferred DropEffect"))?;

    let mut first_error: Option<anyhow::Error> = None;
    let mut set = |format: u32, bytes: &[u8]| {
        if let Err(e) = set_bytes(format, bytes) {
            if first_error.is_none() {
                first_error = Some(e);
            }
        }
    };

    // El orden importa: las apps suelen preferir los primeros formatos.
    set(png_format, &png);
    set(CF_DIBV5.0 as u32, &dib_v5);
    set(CF_DIB.0 as u32, &dib);
    set(CF_HDROP.0 as u32, &hdrop);
    set(drop_effect_format, &drop_effect);

    drop(clipboard);

    match first_error {
        Some(e) => Err(e.context("no se pudo publicar todo el portapapeles")),
        None => Ok(()),
    }
}

/// Texto Unicode (por ejemplo, el color hexadecimal que copia el overlay).
pub fn copy_text(text: &str) -> Result<()> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let bytes: Vec<u8> = utf16.iter().flat_map(|u| u.to_ne_bytes()).collect();

    let clipboard = ClipboardGuard::open()?;
    set_bytes(CF_UNICODETEXT.0 as u32, &bytes)?;
    drop(clipboard);
    Ok(())
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self> {
        for attempt in 0..OPEN_RETRIES {
            match unsafe { OpenClipboard(owner()) } {
                Ok(()) => {
                    if let Err(e) = unsafe { EmptyClipboard() } {
                        unsafe {
                            let _ = CloseClipboard();
                        }
                        return Err(anyhow!("no se pudo vaciar el portapapeles: {e}"));
                    }
                    return Ok(Self);
                }
                Err(_) if attempt + 1 < OPEN_RETRIES => {
                    std::thread::sleep(OPEN_RETRY_DELAY);
                }
                Err(e) => {
                    return Err(anyhow!("el portapapeles está ocupado (10 intentos): {e}"));
                }
            }
        }
        Err(anyhow!("el portapapeles está ocupado"))
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn clipboard_format(name: PCWSTR) -> Result<u32> {
    let id = unsafe { RegisterClipboardFormatW(name) };
    if id == 0 {
        Err(anyhow!("no se pudo registrar el formato de portapapeles"))
    } else {
        Ok(id)
    }
}

/// Reserva memoria movible, copia los bytes y se la entrega al portapapeles.
fn set_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Err(anyhow!("formato {format} vacío"));
    }
    unsafe {
        let hglobal: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
            .map_err(|e| anyhow!("GlobalAlloc({} bytes) falló: {e}", bytes.len()))?;
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err(anyhow!("GlobalLock falló"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        let _ = GlobalUnlock(hglobal);

        let handle = HANDLE(hglobal.0);
        match SetClipboardData(format, Some(handle)) {
            Ok(_) => Ok(()),
            Err(e) => {
                // Si SetClipboardData falla, la memoria sigue siendo nuestra.
                let _ = GlobalFree(Some(hglobal));
                Err(anyhow!("SetClipboardData({format}) falló: {e}"))
            }
        }
    }
}

/// BITMAPV5HEADER + píxeles BGRA de abajo arriba.
fn dib_v5(img: &RgbaImage) -> Vec<u8> {
    let header = BITMAPV5HEADER {
        bV5Size: std::mem::size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: img.width as i32,
        bV5Height: img.height as i32,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5SizeImage: img.width.saturating_mul(img.height).saturating_mul(4),
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: 0xFF00_0000,
        bV5CSType: 0x7352_4742, // LCS_sRGB ('sRGB')
        bV5Intent: 4,           // LCS_GM_IMAGES
        bV5XPelsPerMeter: 2835,
        bV5YPelsPerMeter: 2835,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(std::mem::size_of::<BITMAPV5HEADER>() + (img.data.len()));
    out.extend_from_slice(as_bytes(&header));
    append_bgra_bottom_up(&mut out, img, true);
    out
}

/// BITMAPINFOHEADER + píxeles BGRX (alfa 255) para apps que solo leen CF_DIB.
fn dib_v1(img: &RgbaImage) -> Vec<u8> {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: img.width as i32,
        biHeight: img.height as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: img.width.saturating_mul(img.height).saturating_mul(4),
        ..Default::default()
    };
    let mut out = Vec::with_capacity(std::mem::size_of::<BITMAPINFOHEADER>() + img.data.len());
    out.extend_from_slice(as_bytes(&header));
    append_bgra_bottom_up(&mut out, img, false);
    out
}

fn append_bgra_bottom_up(out: &mut Vec<u8>, img: &RgbaImage, with_alpha: bool) {
    let stride = img.width as usize * 4;
    for y in (0..img.height as usize).rev() {
        let row = &img.data[y * stride..y * stride + stride];
        for px in row.as_chunks::<4>().0 {
            out.push(px[2]);
            out.push(px[1]);
            out.push(px[0]);
            out.push(if with_alpha { px[3] } else { 255 });
        }
    }
}

fn hdrop_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;

    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path_units = absolute.as_os_str().encode_wide();

    let mut out: Vec<u8> = Vec::new();
    // DROPFILES (20 bytes, empaquetado): pFiles=20, pt=0,0, fNC=FALSE, fWide=TRUE
    let offset = std::mem::size_of::<DROPFILES>() as u32;
    out.extend_from_slice(&offset.to_ne_bytes());
    out.extend_from_slice(&0i32.to_ne_bytes());
    out.extend_from_slice(&0i32.to_ne_bytes());
    out.extend_from_slice(&0i32.to_ne_bytes()); // fNC
    out.extend_from_slice(&1i32.to_ne_bytes()); // fWide

    for unit in path_units {
        out.extend_from_slice(&unit.to_ne_bytes());
    }
    out.extend_from_slice(&0u16.to_ne_bytes());
    out.extend_from_slice(&0u16.to_ne_bytes());

    if out.len() <= offset as usize + 2 {
        return Err(anyhow!("ruta de portapapeles vacía"));
    }
    Ok(out)
}

fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>()) }
}

/// Lee de vuelta un formato del portapapeles (para tests y diagnóstico).
#[allow(dead_code)]
pub fn read_format_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let handle = GetClipboardData(format).ok()?;
        let hglobal = HGLOBAL(handle.0);
        let ptr = GlobalLock(hglobal) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let len = GlobalSize(hglobal);
        if len == 0 {
            let _ = GlobalUnlock(hglobal);
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
        let _ = GlobalUnlock(hglobal);
        Some(bytes)
    }
}

/// Abre el portapapeles para inspeccionarlo; el llamante debe cerrarlo.
#[allow(dead_code)]
pub fn open_for_inspection(owner_hwnd: Option<HWND>) -> Result<()> {
    unsafe { OpenClipboard(owner_hwnd.or_else(owner)) }
        .map_err(|e| anyhow!("no se pudo abrir el portapapeles: {e}"))
}

#[allow(dead_code)]
pub fn close_inspection() {
    unsafe {
        let _ = CloseClipboard();
    }
}

/// ID del formato registrado "PNG" (0 si no existe).
#[allow(dead_code)]
pub fn png_format_id() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

/// ID del formato "Preferred DropEffect" (0 si no existe).
#[allow(dead_code)]
pub fn preferred_drop_effect_id() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
}

/// Rutas de archivo publicadas como CF_HDROP en el portapapeles actual.
/// `None` significa que no se pudo inspeccionar (ocupado, sin acceso…), no que
/// esté vacío. La limpieza usa esa distinción para no borrar por error.
pub fn try_file_paths() -> Option<Vec<PathBuf>> {
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    let mut paths = Vec::new();
    unsafe {
        if OpenClipboard(owner()).is_err() {
            return None;
        }
        if let Ok(handle) = GetClipboardData(CF_HDROP.0 as u32) {
            let hdrop = HDROP(handle.0);
            let count = DragQueryFileW(hdrop, u32::MAX, None);
            for index in 0..count {
                let length = DragQueryFileW(hdrop, index, None);
                if length == 0 {
                    continue;
                }
                let mut buffer = vec![0u16; length as usize + 1];
                let copied = DragQueryFileW(hdrop, index, Some(&mut buffer));
                if copied > 0 {
                    paths.push(PathBuf::from(String::from_utf16_lossy(
                        &buffer[..copied as usize],
                    )));
                }
            }
        }
        let _ = CloseClipboard();
    }
    Some(paths)
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    static SERIAL: Mutex<()> = Mutex::new(());

    /// Ventana oculta real para probar el portapapeles con dueño (como main).
    struct TestOwner(Option<HWND>);

    impl TestOwner {
        fn new() -> Self {
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    windows::core::w!("STATIC"),
                    windows::core::w!("DestelloClipboardTest"),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    None,
                    None,
                )
                .ok()
            };
            if let Some(hwnd) = hwnd {
                set_owner(hwnd);
            } else {
                eprintln!(
                    "TestOwner: no se pudo crear la ventana oculta; el test usará NULL"
                );
            }
            Self(hwnd)
        }
    }

    impl Drop for TestOwner {
        fn drop(&mut self) {
            set_owner(HWND::default());
            if let Some(hwnd) = self.0 {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }

    fn image() -> RgbaImage {
        let mut data = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                data.extend_from_slice(&[
                    x * 40 + 10,
                    y * 50 + 20,
                    x * y + 30,
                    255,
                ]);
            }
        }
        RgbaImage {
            width: 3,
            height: 2,
            data,
        }
    }

    fn skip_busy(test: &str) {
        eprintln!("{test}: portapapeles ocupado, test omitido");
    }

    #[test]
    fn ac1_copia_todos_los_formatos_y_se_leen_de_vuelta() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _owner = TestOwner::new();
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("captura.png");
        let img = image();
        std::fs::write(&png_path, image_ops::encode_png(&img).unwrap()).unwrap();

        if let Err(e) = copy_image(&img, &png_path) {
            skip_busy(&format!("copia_todos_los_formatos ({e})"));
            return;
        }

        if let Some(expected) = _owner.0 {
            let actual = unsafe {
                windows::Win32::System::DataExchange::GetClipboardOwner()
            }
            .unwrap_or_default();
            assert_eq!(
                actual, expected,
                "el portapapeles debe pertenecer a nuestra ventana, no a NULL"
            );
        }

        if open_for_inspection(None).is_err() {
            skip_busy("copia_todos_los_formatos (inspección)");
            return;
        }

        // PNG: bytes válidos y mismas dimensiones/píxeles.
        let png_format = png_format_id();
        let png = read_format_bytes(png_format).expect("PNG en el portapapeles");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("PNG decodificable")
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (3, 2));
        assert_eq!(decoded.get_pixel(1, 1).0, [50, 70, 31, 255]);

        // CF_DIBV5: cabecera de 124 bytes, 3x2, máscaras correctas.
        let v5 = read_format_bytes(CF_DIBV5.0 as u32).expect("CF_DIBV5");
        assert_eq!(u32::from_ne_bytes(v5[0..4].try_into().unwrap()), 124);
        assert_eq!(i32::from_ne_bytes(v5[4..8].try_into().unwrap()), 3);
        assert_eq!(i32::from_ne_bytes(v5[8..12].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_ne_bytes(v5[52..56].try_into().unwrap()),
            0xFF00_0000
        );
        // Primer píxel de la fila inferior (BGRA): y=1, x=0 -> RGBA [10, 70, 30]
        let off = 124;
        assert_eq!(&v5[off..off + 4], &[30, 70, 10, 255]);

        // CF_DIB: cabecera clásica de 40 bytes, 32 bpp.
        let v1 = read_format_bytes(CF_DIB.0 as u32).expect("CF_DIB");
        assert_eq!(u32::from_ne_bytes(v1[0..4].try_into().unwrap()), 40);
        assert_eq!(u16::from_ne_bytes(v1[14..16].try_into().unwrap()), 32);

        // CF_HDROP: exactamente el PNG de caché.
        let hdrop_bytes = read_format_bytes(CF_HDROP.0 as u32).expect("CF_HDROP");
        let hdrop = windows::Win32::UI::Shell::HDROP(
            hdrop_bytes.as_ptr() as *mut core::ffi::c_void,
        );
        let mut buffer = [0u16; 512];
        let len = unsafe {
            windows::Win32::UI::Shell::DragQueryFileW(hdrop, 0, Some(&mut buffer))
        };
        assert!(len > 0, "CF_HDROP sin archivo");
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        assert_eq!(
            std::path::PathBuf::from(&path),
            std::path::absolute(&png_path).unwrap()
        );
        let count = unsafe {
            windows::Win32::UI::Shell::DragQueryFileW(hdrop, u32::MAX, None)
        };
        assert_eq!(count, 1);

        // Preferred DropEffect = DROPEFFECT_COPY
        let effect = read_format_bytes(preferred_drop_effect_id()).expect("DropEffect");
        assert_eq!(effect.len(), 4);
        assert_eq!(u32::from_ne_bytes(effect[0..4].try_into().unwrap()), 1);

        close_inspection();
    }

    #[test]
    fn texto_unicode_ida_y_vuelta() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _owner = TestOwner::new();
        if copy_text("Hola ñandú 🎨").is_err() {
            skip_busy("texto_unicode");
            return;
        }
        if open_for_inspection(None).is_err() {
            skip_busy("texto_unicode (inspección)");
            return;
        }
        let bytes = read_format_bytes(CF_UNICODETEXT.0 as u32).expect("texto");
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_ne_bytes([c[0], c[1]]))
            .take_while(|u| *u != 0)
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "Hola ñandú 🎨");
        close_inspection();
    }

    #[test]
    fn archivo_inexistente_es_error() {
        let img = image();
        let missing = std::path::Path::new("Z:\\no\\existe\\captura.png");
        let err = copy_image(&img, missing).unwrap_err();
        assert!(err.to_string().contains("no existe"));
    }

    #[test]
    fn ac1_hdrop_lleva_utf16_doble_nulo() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ñandú 🎨.png");
        let bytes = hdrop_bytes(&p).unwrap();
        assert_eq!(u32::from_ne_bytes(bytes[0..4].try_into().unwrap()), 20);
        assert_eq!(u32::from_ne_bytes(bytes[16..20].try_into().unwrap()), 1);
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
    }
}
