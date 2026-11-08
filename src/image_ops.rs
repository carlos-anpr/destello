//! Operaciones de imagen sin dependencias de UI: conversión, recorte, pixelado
//! y codificación PNG/JPEG (crate `image`, solo esos dos formatos).
use anyhow::Result;

/// RGBA8 no premultiplicado, filas contiguas (stride = width*4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[allow(dead_code)] // API del módulo: recorte/pixelado los usa la exportación/UI
impl RgbaImage {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    /// Convierte B,G,R,A (captura GDI) a R,G,B,A. Si el buffer es corto se
    /// rellena con píxeles negros opacos en lugar de entrar en pánico.
    pub fn from_bgra(width: u32, height: u32, bgra: &[u8]) -> Self {
        let pixels = (width as usize) * (height as usize);
        let mut data = vec![0u8; pixels * 4];
        let n = pixels.min(bgra.len() / 4);
        for i in 0..n {
            let s = i * 4;
            let d = i * 4;
            data[d] = bgra[s + 2];
            data[d + 1] = bgra[s + 1];
            data[d + 2] = bgra[s];
            data[d + 3] = 255;
        }
        // Cola incompleta: negro opaco.
        for px in data.as_chunks_mut::<4>().0.iter_mut().skip(n) {
            px[3] = 255;
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// Convierte una captura completa a RGBA.
    pub fn from_screenshot(shot: &crate::capture::Screenshot) -> Self {
        Self::from_bgra(shot.width, shot.height, &shot.bgra)
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.data.is_empty()
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Recorte por coordenadas locales. Se recorta a los límites de la imagen.
    pub fn crop(&self, x: i64, y: i64, w: i64, h: i64) -> Self {
        if w <= 0 || h <= 0 || self.is_empty() {
            return Self::default();
        }
        let x0 = x.clamp(0, self.width as i64);
        let y0 = y.clamp(0, self.height as i64);
        let x1 = (x + w).clamp(0, self.width as i64);
        let y1 = (y + h).clamp(0, self.height as i64);
        let (cw, ch) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if cw == 0 || ch == 0 {
            return Self::default();
        }

        let mut data = Vec::with_capacity((cw as usize) * (ch as usize) * 4);
        let stride = self.width as usize * 4;
        for row in 0..ch as usize {
            let start = (y0 as usize + row) * stride + (x0 as usize) * 4;
            data.extend_from_slice(&self.data[start..start + (cw as usize) * 4]);
        }
        Self {
            width: cw,
            height: ch,
            data,
        }
    }

    /// Recorte usando coordenadas del escritorio virtual y una captura que
    /// empieza en `origin_x, origin_y`.
    pub fn crop_rect(&self, origin_x: i32, origin_y: i32, rect: crate::geom::Rect) -> Self {
        self.crop(
            rect.left as i64 - origin_x as i64,
            rect.top as i64 - origin_y as i64,
            rect.width() as i64,
            rect.height() as i64,
        )
    }

    /// Pixelado (mosaico) por media de bloques. `block` se limita a >= 2.
    pub fn pixelate(&mut self, x: i64, y: i64, w: i64, h: i64, block: u32) {
        if self.is_empty() || w <= 0 || h <= 0 {
            return;
        }
        let block = block.max(2) as i64;
        let x0 = x.clamp(0, self.width as i64);
        let y0 = y.clamp(0, self.height as i64);
        let x1 = (x + w).clamp(0, self.width as i64);
        let y1 = (y + h).clamp(0, self.height as i64);
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let mut by = y0;
        while by < y1 {
            let bh = block.min(y1 - by);
            let mut bx = x0;
            while bx < x1 {
                let bw = block.min(x1 - bx);
                let mut sum = [0u64; 4];
                let count = (bw * bh) as u64;
                for py in by..by + bh {
                    for px in bx..bx + bw {
                        let p = self.pixel(px as u32, py as u32);
                        for c in 0..4 {
                            sum[c] += p[c] as u64;
                        }
                    }
                }
                let avg = [
                    (sum[0] / count) as u8,
                    (sum[1] / count) as u8,
                    (sum[2] / count) as u8,
                    (sum[3] / count) as u8,
                ];
                for py in by..by + bh {
                    for px in bx..bx + bw {
                        let i = ((py as usize) * (self.width as usize) + px as usize) * 4;
                        self.data[i..i + 4].copy_from_slice(&avg);
                    }
                }
                bx += bw;
            }
            by += bh;
        }
    }
}

pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    if img.is_empty() {
        anyhow::bail!("imagen vacía");
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&img.data, img.width, img.height, ExtendedColorType::Rgba8)
        .map_err(|e| anyhow::anyhow!("no se pudo codificar PNG: {e}"))?;
    Ok(out)
}

pub fn encode_jpeg(img: &RgbaImage, quality: u8) -> Result<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    if img.is_empty() {
        anyhow::bail!("imagen vacía");
    }
    let quality = quality.clamp(1, 100);
    let mut rgb = Vec::with_capacity((img.width as usize) * (img.height as usize) * 3);
    for px in img.data.as_chunks::<4>().0 {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .write_image(&rgb, img.width, img.height, ExtendedColorType::Rgb8)
        .map_err(|e| anyhow::anyhow!("no se pudo codificar JPEG: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RgbaImage {
        RgbaImage {
            width: 3,
            height: 2,
            data: vec![
                1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, // fila 0
                10, 11, 12, 255, 13, 14, 15, 255, 16, 17, 18, 255, // fila 1
            ],
        }
    }

    #[test]
    fn bgra_a_rgba() {
        let bgra = [10, 20, 30, 255, 40, 50, 60, 0];
        let img = RgbaImage::from_bgra(2, 1, &bgra);
        assert_eq!(img.pixel(0, 0), [30, 20, 10, 255]);
        // El alfa de la captura siempre es opaco.
        assert_eq!(img.pixel(1, 0), [60, 50, 40, 255]);
    }

    #[test]
    fn buffer_corto_no_panica() {
        let img = RgbaImage::from_bgra(4, 4, &[1, 2, 3, 4]);
        assert_eq!(img.data.len(), 4 * 4 * 4);
        assert_eq!(img.pixel(0, 0), [3, 2, 1, 255]);
        assert_eq!(img.pixel(3, 3), [0, 0, 0, 255]);
    }

    #[test]
    fn recorte_basico_y_limites() {
        let img = sample();
        let c = img.crop(1, 1, 2, 1);
        assert_eq!((c.width, c.height), (2, 1));
        assert_eq!(c.pixel(0, 0), [13, 14, 15, 255]);
        assert_eq!(c.pixel(1, 0), [16, 17, 18, 255]);

        let clamped = img.crop(-10, -10, 100, 100);
        assert_eq!((clamped.width, clamped.height), (3, 2));

        let outside = img.crop(10, 10, 5, 5);
        assert!(outside.is_empty());
    }

    #[test]
    fn recorte_por_rect_global() {
        use crate::geom::Rect;
        let shot = crate::capture::Screenshot {
            origin_x: 100,
            origin_y: 200,
            width: 3,
            height: 2,
            bgra: vec![0; 3 * 2 * 4],
            monitors: Vec::new(),
            targets: Vec::new(),
        };
        let img = RgbaImage::from_screenshot(&shot);
        let c = img.crop_rect(100, 200, Rect::new(101, 201, 103, 202));
        assert_eq!((c.width, c.height), (2, 1));
    }

    #[test]
    fn pixelado_promedia_bloques() {
        let mut img = RgbaImage {
            width: 4,
            height: 4,
            data: Vec::new(),
        };
        for y in 0..4 {
            for x in 0..4 {
                let v = if x < 2 { 0 } else { 200 };
                img.data.extend_from_slice(&[v, v, v, 255]);
                let _ = y;
            }
        }
        img.pixelate(0, 0, 4, 4, 2);
        assert_eq!(img.pixel(0, 0), [0, 0, 0, 255]);
        assert_eq!(img.pixel(1, 1), [0, 0, 0, 255]);
        assert_eq!(img.pixel(2, 0), [200, 200, 200, 255]);
        assert_eq!(img.pixel(3, 3), [200, 200, 200, 255]);
    }

    #[test]
    fn codifica_png_y_jpeg() {
        let img = sample();
        let png = encode_png(&img).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (3, 2));

        let jpg = encode_jpeg(&img, 85).unwrap();
        assert_eq!(&jpg[..2], b"\xFF\xD8");
        let decoded = image::load_from_memory_with_format(&jpg, image::ImageFormat::Jpeg)
            .unwrap()
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (3, 2));
    }

    #[test]
    fn imagen_vacia_no_se_codifica() {
        assert!(encode_png(&RgbaImage::default()).is_err());
        assert!(encode_jpeg(&RgbaImage::default(), 90).is_err());
    }
}
