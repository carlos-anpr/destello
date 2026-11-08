use super::{
    annotate::{self, Annotation},
    d2d::{Canvas, Graphics},
};
use crate::{
    capture::Screenshot,
    geom::{Point, Rect},
    image_ops::RgbaImage,
};
use windows::Win32::Graphics::Imaging::*;
/// Screen and export call the identical annotation renderer, with only an integer origin shift.
pub fn render(shot: &Screenshot, area: Rect, items: &[Annotation]) -> anyhow::Result<RgbaImage> {
    let area = area.intersect(Rect::new(
        shot.origin_x,
        shot.origin_y,
        shot.origin_x + shot.width as i32,
        shot.origin_y + shot.height as i32,
    ));
    anyhow::ensure!(!area.is_empty(), "Selecciona un área de al menos un píxel");
    let gfx = Graphics::new()?;
    unsafe {
        let bitmap = gfx.wic.resolve()?.CreateBitmap(
            area.width() as u32,
            area.height() as u32,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapCacheOnLoad,
        )?;
        let rt = gfx
            .factory
            .CreateWicBitmapRenderTarget(&bitmap, &Graphics::properties())?;
        rt.SetTextAntialiasMode(
            windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
        );
        let c = Canvas { rt: &rt, gfx: &gfx };
        let bg = c.bitmap(shot.width, shot.height, &shot.bgra)?;
        rt.BeginDraw();
        c.image(
            &bg,
            Rect::new(0, 0, area.width(), area.height()),
            Some(area.translate(-shot.origin_x, -shot.origin_y)),
            false,
        );
        let result = annotate::render(
            &c,
            items,
            shot,
            Point {
                x: area.left,
                y: area.top,
            },
        );
        let end = rt.EndDraw(None, None);
        result?;
        end?;
        let mut data = vec![0; area.width() as usize * area.height() as usize * 4];
        bitmap.CopyPixels(std::ptr::null(), area.width() as u32 * 4, &mut data)?;
        for p in data.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            let a = p[3] as u32;
            if a != 0 && a != 255 {
                for v in &mut p[..3] {
                    *v = ((*v as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        Ok(RgbaImage {
            width: area.width() as u32,
            height: area.height() as u32,
            data,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::annotate::Tool;
    use windows::Win32::System::Com::*;
    struct Com;
    impl Com {
        fn init() -> Self {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .expect("COM para render WIC");
            }
            Self
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    fn shot() -> Screenshot {
        Screenshot {
            targets: Vec::new(),
            origin_x: -2,
            origin_y: -1,
            width: 2,
            height: 2,
            bgra: vec![
                240, 30, 10, 255, 50, 60, 70, 255, 80, 90, 100, 255, 110, 120, 130, 255,
            ],
            monitors: Vec::new(),
        }
    }
    #[test]
    fn crop_keeps_native_pixel_and_negative_origin() {
        let _com = Com::init();
        let img = render(&shot(), Rect::new(-1, 0, 0, 1), &[]).expect("export");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(img.data, vec![130, 120, 110, 255]);
    }
    #[test]
    fn empty_selection_is_rejected() {
        let _com = Com::init();
        assert!(render(&shot(), Rect::new(0, 0, 0, 1), &[]).is_err());
    }
    #[test]
    fn pixelation_reads_original_pixels_under_annotations() {
        let _com = Com::init();
        let mut source = shot();
        source.bgra = [255, 0, 0, 255].repeat(4);
        let rectangle = Annotation {
            tool: Tool::Redact,
            points: vec![Point { x: -2, y: -1 }, Point { x: 0, y: 1 }],
            color: 0xff0000,
            thickness: 2.,
            fill: true,
            text: String::new(),
            number: 0,
        };
        let mut privacy = rectangle.clone();
        privacy.tool = Tool::Pixelate;
        let img = render(&source, Rect::new(-2, -1, 0, 1), &[rectangle, privacy]).expect("export");
        assert!(img
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [0, 0, 255, 255]));
    }
    #[test]
    fn marker_uses_normal_source_over_at_forty_percent() {
        let _com = Com::init();
        let mut source = shot();
        source.bgra = [0, 0, 0, 255].repeat(4);
        let marker = Annotation {
            tool: Tool::Marker,
            points: vec![Point { x: -10, y: -1 }, Point { x: 10, y: 0 }],
            color: 0xffd60a,
            thickness: 4.,
            fill: false,
            text: String::new(),
            number: 0,
        };
        let result = render(&source, Rect::new(-2, -1, 0, 1), &[marker]).expect("marker");
        for pixel in result.data.as_chunks::<4>().0 {
            assert!((101..=103).contains(&pixel[0]));
            assert!((85..=87).contains(&pixel[1]));
            assert!((3..=5).contains(&pixel[2]));
            assert_eq!(pixel[3], 255);
        }
    }
    #[test]
    fn reserved_bitmap_is_updated_in_place_between_frames() {
        let _com = Com::init();
        let gfx = Graphics::new().expect("graphics");
        unsafe {
            let output = gfx
                .wic
                .resolve()
                .expect("WIC")
                .CreateBitmap(2, 2, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)
                .expect("surface");
            let rt = gfx
                .factory
                .CreateWicBitmapRenderTarget(&output, &Graphics::properties())
                .expect("RT");
            let canvas = Canvas { rt: &rt, gfx: &gfx };
            let bitmap = canvas.reserve_bitmap(2, 2).expect("reserved bitmap");
            use windows::core::Interface;
            let identity = bitmap.as_raw();
            for color in [[255u8, 0, 0, 255], [0, 0, 255, 255]] {
                let pixels = color.repeat(4);
                bitmap
                    .CopyFromMemory(None, pixels.as_ptr().cast(), 8)
                    .expect("copy");
                rt.BeginDraw();
                canvas.image(&bitmap, Rect::new(0, 0, 2, 2), None, true);
                rt.EndDraw(None, None).expect("paint");
                let mut actual = vec![0u8; 16];
                output
                    .CopyPixels(std::ptr::null(), 8, &mut actual)
                    .expect("read");
                assert_eq!(actual, pixels);
                assert_eq!(identity, bitmap.as_raw());
            }
        }
    }
}
