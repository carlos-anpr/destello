//! Immutable icon geometries are built once by Graphics and shared by all render targets.
use super::{annotate::Tool, d2d::Canvas};
use crate::geom::Rect;
use windows::Win32::Graphics::Direct2D::{Common::*, *};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Tool(Tool),
    Undo,
    Redo,
    Pin,
    SaveAs,
    Copy,
    Close,
}
impl Icon {
    fn index(self) -> usize {
        match self {
            Self::Tool(t) => t as usize,
            Self::Undo => 11,
            Self::Redo => 12,
            Self::Pin => 13,
            Self::SaveAs => 14,
            Self::Copy => 16,
            Self::Close => 17,
        }
    }
}
pub fn draw(c: &Canvas<'_>, icon: Icon, r: Rect, col: u32) -> windows::core::Result<()> {
    c.icon(icon.index(), r, col)
}
fn line(a: i32, b: i32, c: i32, d: i32) -> Vec<(f32, f32)> {
    vec![(a as f32, b as f32), (c as f32, d as f32)]
}
fn rectangle(x: f32, y: f32, w: f32, h: f32) -> Vec<(f32, f32)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]
}
fn circle() -> Vec<(f32, f32)> {
    (0..=32)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 32.;
            (12. + 8. * a.cos(), 12. + 8. * a.sin())
        })
        .collect()
}
fn paths(i: usize) -> Vec<Vec<(f32, f32)>> {
    match i {
        0 => [
            (4, 3, 4, 19),
            (4, 3, 17, 13),
            (4, 19, 9, 14),
            (9, 14, 13, 22),
            (13, 22, 16, 20),
            (16, 20, 12, 13),
            (12, 13, 17, 13),
        ]
        .map(|(a, b, c, d)| line(a, b, c, d))
        .to_vec(),
        1 => vec![rectangle(4., 4., 16., 16.)],
        2 => vec![circle()],
        3 => vec![line(4, 20, 20, 4), line(10, 4, 20, 4), line(20, 4, 20, 14)],
        4 => vec![line(4, 20, 20, 4)],
        5 => [
            (5, 18, 17, 4),
            (17, 4, 21, 8),
            (21, 8, 9, 21),
            (5, 18, 4, 22),
            (4, 22, 9, 21),
            (14, 7, 18, 11),
        ]
        .map(|(a, b, c, d)| line(a, b, c, d))
        .to_vec(),
        6 => [
            (4, 18, 16, 4),
            (16, 4, 22, 10),
            (22, 10, 10, 22),
            (4, 18, 10, 22),
            (2, 23, 18, 23),
        ]
        .map(|(a, b, c, d)| line(a, b, c, d))
        .to_vec(),
        7 => vec![line(4, 5, 20, 5), line(12, 5, 12, 21), line(8, 21, 16, 21)],
        8 => vec![
            circle(),
            line(10, 8, 12, 6),
            line(12, 6, 12, 17),
            line(9, 17, 15, 17),
        ],
        9 => [(4., 4.), (14., 4.), (4., 14.), (14., 14.)]
            .map(|(x, y)| rectangle(x, y, 6., 6.))
            .to_vec(),
        10 => vec![
            rectangle(4., 4., 16., 16.),
            line(4, 9, 20, 9),
            line(4, 15, 20, 15),
        ],
        11 | 12 => {
            let mut p = vec![
                line(4, 11, 4, 4),
                line(4, 11, 11, 11),
                line(4, 11, 10, 5),
                line(10, 5, 17, 5),
                line(17, 5, 21, 10),
                line(21, 10, 20, 17),
                line(20, 17, 15, 20),
            ];
            if i == 12 {
                for points in &mut p {
                    for (x, _) in points {
                        *x = 24. - *x;
                    }
                }
            }
            p
        }
        13 => [
            (9, 3, 19, 3),
            (10, 3, 10, 10),
            (18, 3, 18, 10),
            (10, 10, 6, 14),
            (18, 10, 21, 14),
            (6, 14, 21, 14),
            (13, 14, 8, 23),
        ]
        .map(|(a, b, c, d)| line(a, b, c, d))
        .to_vec(),
        14 | 15 => {
            let mut p = vec![
                rectangle(4., 4., 16., 16.),
                line(8, 4, 8, 10),
                line(8, 10, 16, 10),
                line(16, 10, 16, 4),
                line(8, 20, 8, 14),
                line(8, 14, 16, 14),
                line(16, 14, 16, 20),
            ];
            if i == 14 {
                p.extend([line(18, 2, 23, 2), line(21, 0, 21, 5)]);
            }
            p
        }
        16 => vec![line(4, 12, 10, 18), line(10, 18, 21, 6)],
        _ => vec![line(6, 6, 18, 18), line(18, 6, 6, 18)],
    }
}
pub(super) fn build(factory: &ID2D1Factory) -> windows::core::Result<Vec<ID2D1PathGeometry>> {
    let mut icons = Vec::with_capacity(18);
    unsafe {
        for i in 0..18 {
            let geometry = factory.CreatePathGeometry()?;
            let sink = geometry.Open()?;
            for path in paths(i) {
                let vectors: Vec<_> = path
                    .into_iter()
                    .map(|(x, y)| {
                        let mut e = D2D1_ELLIPSE::default();
                        e.point.X = x;
                        e.point.Y = y;
                        e.point
                    })
                    .collect();
                if let Some(first) = vectors.first() {
                    sink.BeginFigure(*first, D2D1_FIGURE_BEGIN_HOLLOW);
                    sink.AddLines(&vectors[1..]);
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
            }
            sink.Close()?;
            icons.push(geometry);
        }
    }
    Ok(icons)
}
