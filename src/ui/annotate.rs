use super::d2d::Canvas;
use crate::{
    capture::Screenshot,
    geom::{Point, Rect},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Move,
    Rectangle,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Marker,
    Text,
    Counter,
    Pixelate,
    Redact,
}
impl Tool {
    pub const ALL: [Self; 11] = [
        Self::Move,
        Self::Rectangle,
        Self::Ellipse,
        Self::Arrow,
        Self::Line,
        Self::Pen,
        Self::Marker,
        Self::Text,
        Self::Counter,
        Self::Pixelate,
        Self::Redact,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Move => "Mover · V",
            Self::Rectangle => "Rectángulo · R",
            Self::Ellipse => "Elipse · E",
            Self::Arrow => "Flecha · A",
            Self::Line => "Línea · L",
            Self::Pen => "Lápiz · P",
            Self::Marker => "Marcador · M",
            Self::Text => "Texto · T",
            Self::Counter => "Contador · N",
            Self::Pixelate => "Pixelar · B",
            Self::Redact => "Censurar · X",
        }
    }
    pub fn key(k: u32) -> Option<Self> {
        Some(match k {
            86 => Self::Move,
            82 => Self::Rectangle,
            69 => Self::Ellipse,
            65 => Self::Arrow,
            76 => Self::Line,
            80 => Self::Pen,
            77 => Self::Marker,
            84 => Self::Text,
            78 => Self::Counter,
            66 => Self::Pixelate,
            88 => Self::Redact,
            _ => return None,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub tool: Tool,
    pub points: Vec<Point>,
    pub color: u32,
    pub thickness: f32,
    pub fill: bool,
    pub text: String,
    pub number: usize,
}
impl Annotation {
    pub fn bounds(&self) -> Rect {
        let mut r = Rect::new(i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for p in &self.points {
            r.left = r.left.min(p.x);
            r.top = r.top.min(p.y);
            r.right = r.right.max(p.x);
            r.bottom = r.bottom.max(p.y)
        }
        if self.tool == Tool::Text {
            r.right = r.left
                + (self
                    .text
                    .lines()
                    .map(|l| l.chars().count())
                    .max()
                    .unwrap_or(1) as f32
                    * self.thickness
                    * 6.5) as i32
                + 12;
            r.bottom =
                r.top + self.text.lines().count().max(1) as i32 * (self.thickness * 8.) as i32 + 8;
        }
        if self.tool == Tool::Counter {
            r = r.translate(-16, -16);
            r.right += 32;
            r.bottom += 32;
        }
        r
    }
    pub fn hit(&self, p: Point) -> bool {
        let r = self.bounds();
        let pad = self.thickness.ceil() as i32 + 6;
        let expanded = Rect::new(
            r.left - pad,
            r.top - pad,
            r.right + pad + 1,
            r.bottom + pad + 1,
        );
        if !expanded.contains(p) {
            return false;
        }
        if matches!(
            self.tool,
            Tool::Line | Tool::Arrow | Tool::Pen | Tool::Marker
        ) {
            return self
                .points
                .windows(2)
                .any(|s| segment_distance(p, s[0], s[1]) <= pad as f64);
        }
        true
    }
    pub fn translate(&mut self, dx: i32, dy: i32) {
        for p in &mut self.points {
            p.x += dx;
            p.y += dy
        }
    }
}
fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let dx = (b.x - a.x) as f64;
    let dy = (b.y - a.y) as f64;
    let t = if dx * dx + dy * dy == 0. {
        0.
    } else {
        (((p.x - a.x) as f64 * dx + (p.y - a.y) as f64 * dy) / (dx * dx + dy * dy)).clamp(0., 1.)
    };
    ((p.x - a.x) as f64 - dx * t).hypot((p.y - a.y) as f64 - dy * t)
}
#[derive(Default)]
pub struct History {
    pub items: Vec<Annotation>,
    undo: Vec<Vec<Annotation>>,
    redo: Vec<Vec<Annotation>>,
}
impl History {
    pub fn checkpoint(&mut self) {
        self.undo.push(self.items.clone());
        self.redo.clear();
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }
    pub fn push(&mut self, a: Annotation) {
        self.checkpoint();
        self.items.push(a)
    }
    pub fn undo(&mut self) {
        if let Some(v) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.items, v));
        }
    }
    pub fn redo(&mut self) {
        if let Some(v) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.items, v));
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn hit(&self, p: Point) -> Option<usize> {
        self.items.iter().rposition(|a| a.hit(p))
    }
}
/// UTF-16 input is assembled before inserting scalar values; caret always stays on a UTF-8 boundary.
#[derive(Default)]
pub struct TextEdit {
    pub text: String,
    pub caret: usize,
    pending: Option<u16>,
}
impl TextEdit {
    pub fn input(&mut self, unit: u16) {
        if (0xD800..=0xDBFF).contains(&unit) {
            self.pending = Some(unit);
            return;
        }
        let ch = if (0xDC00..=0xDFFF).contains(&unit) {
            self.pending.take().and_then(|h| {
                char::from_u32(0x10000 + ((h as u32 - 0xD800) << 10) + (unit as u32 - 0xDC00))
            })
        } else {
            self.pending = None;
            char::from_u32(unit as u32)
        };
        if let Some(ch) = ch {
            if !ch.is_control() || ch == '\n' {
                self.text.insert(self.caret, ch);
                self.caret += ch.len_utf8()
            }
        }
    }
    pub fn left(&mut self) {
        self.caret = self.text[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    pub fn right(&mut self) {
        if let Some(c) = self.text[self.caret..].chars().next() {
            self.caret += c.len_utf8()
        }
    }
    pub fn vertical(&mut self, direction: i32) {
        let line_start = self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
        let column = self.text[line_start..self.caret].chars().count();
        let target = if direction < 0 {
            if line_start == 0 {
                return;
            }
            let end = line_start - 1;
            let start = self.text[..end].rfind('\n').map_or(0, |i| i + 1);
            start..end
        } else {
            let Some(end) = self.text[self.caret..].find('\n').map(|i| i + self.caret) else {
                return;
            };
            let start = end + 1;
            let end = self.text[start..]
                .find('\n')
                .map_or(self.text.len(), |i| i + start);
            start..end
        };
        self.caret = self.text[target.clone()]
            .char_indices()
            .nth(column)
            .map_or(target.end, |(i, _)| target.start + i);
    }
    pub fn backspace(&mut self) {
        let end = self.caret;
        self.left();
        self.text.drain(self.caret..end);
    }
    pub fn delete(&mut self) {
        let start = self.caret;
        self.right();
        self.text.drain(start..self.caret);
        self.caret = start;
    }
}
pub fn render(
    c: &Canvas<'_>,
    items: &[Annotation],
    shot: &Screenshot,
    origin: Point,
) -> windows::core::Result<()> {
    for a in items {
        draw(c, a, shot, origin)?
    }
    Ok(())
}
pub fn draw(
    c: &Canvas<'_>,
    a: &Annotation,
    shot: &Screenshot,
    origin: Point,
) -> windows::core::Result<()> {
    let off = |p: Point| Point {
        x: p.x - origin.x,
        y: p.y - origin.y,
    };
    let Some(&start) = a.points.first() else {
        return Ok(());
    };
    let end = a.points.last().copied().unwrap_or(start);
    let r = a.bounds().translate(-origin.x, -origin.y);
    let alpha = if a.tool == Tool::Marker { 0.4 } else { 1. };
    match a.tool {
        Tool::Move => {}
        Tool::Rectangle | Tool::Redact => {
            if a.fill || a.tool == Tool::Redact {
                c.fill(r, a.color, 1.)?
            } else {
                c.border(r, a.color, a.thickness)?
            }
        }
        Tool::Ellipse => c.ellipse(r, a.color, 1., a.thickness, a.fill)?,
        Tool::Line | Tool::Arrow => {
            c.line(off(start), off(end), a.color, 1., a.thickness)?;
            if a.tool == Tool::Arrow {
                let angle = ((end.y - start.y) as f64).atan2((end.x - start.x) as f64);
                let len = (a.thickness * 4.).max(12.) as f64;
                for side in [-0.52f64, 0.52] {
                    let p = Point {
                        x: end.x - (len * (angle + side).cos()) as i32,
                        y: end.y - (len * (angle + side).sin()) as i32,
                    };
                    c.line(off(end), off(p), a.color, 1., a.thickness)?
                }
            }
        }
        Tool::Pen | Tool::Marker => {
            let w = if a.tool == Tool::Marker {
                a.thickness * 5.
            } else {
                a.thickness
            };
            c.path(
                &a.points.iter().copied().map(off).collect::<Vec<_>>(),
                a.color,
                alpha,
                w,
            )?;
            if a.points.len() == 1 {
                c.ellipse(
                    Rect::new(
                        r.left - (w / 2.).ceil() as i32,
                        r.top - (w / 2.).ceil() as i32,
                        r.left + (w / 2.).ceil() as i32,
                        r.top + (w / 2.).ceil() as i32,
                    ),
                    a.color,
                    alpha,
                    w,
                    true,
                )?
            }
        }
        Tool::Text => {
            if a.fill {
                c.rounded(r, 4., 0x111521, 0.8)?
            }
            c.text(&a.text, r, a.thickness * 6., a.color)?
        }
        Tool::Counter => {
            c.ellipse(r, a.color, 1., 2., true)?;
            c.text(
                &a.number.to_string(),
                Rect::new(r.left + 8, r.top + 3, r.right + 8, r.bottom),
                18.,
                0xffffff,
            )?
        }
        Tool::Pixelate => {
            let bounds = a.bounds().intersect(Rect::new(
                shot.origin_x,
                shot.origin_y,
                shot.origin_x + shot.width as i32,
                shot.origin_y + shot.height as i32,
            ));
            let size = (a.thickness * 4.).max(8.) as usize;
            for y in (bounds.top..bounds.bottom).step_by(size) {
                for x in (bounds.left..bounds.right).step_by(size) {
                    let right = (x + size as i32).min(bounds.right);
                    let bottom = (y + size as i32).min(bounds.bottom);
                    let (mut b, mut g, mut red, mut n) = (0u64, 0u64, 0u64, 0u64);
                    for py in y..bottom {
                        for px in x..right {
                            let i = ((py - shot.origin_y) as usize * shot.width as usize
                                + (px - shot.origin_x) as usize)
                                * 4;
                            if let Some(v) = shot.bgra.get(i..i + 4) {
                                b += v[0] as u64;
                                g += v[1] as u64;
                                red += v[2] as u64;
                                n += 1
                            }
                        }
                    }
                    if let (Some(red), Some(g), Some(b)) =
                        (red.checked_div(n), g.checked_div(n), b.checked_div(n))
                    {
                        let col = (red as u32) << 16 | (g as u32) << 8 | b as u32;
                        c.fill(
                            Rect::new(
                                x - origin.x,
                                y - origin.y,
                                right - origin.x,
                                bottom - origin.y,
                            ),
                            col,
                            1.,
                        )?
                    }
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_caret_and_surrogates() {
        let mut e = TextEdit::default();
        for c in "ñ😀".encode_utf16() {
            e.input(c)
        }
        assert_eq!(e.text, "ñ😀");
        e.left();
        e.delete();
        assert_eq!(e.text, "ñ");
        e.backspace();
        assert!(e.text.is_empty());
    }
    #[test]
    fn vertical_caret_keeps_unicode_column() {
        let mut e = TextEdit::default();
        for u in "ñ😀x\nabc".encode_utf16() {
            e.input(u)
        }
        e.vertical(-1);
        assert_eq!(e.caret, "ñ😀x".len());
        e.vertical(1);
        assert_eq!(e.caret, e.text.len());
    }
    #[test]
    fn undo_redo_branch() {
        let mut h = History::default();
        let a = Annotation {
            tool: Tool::Line,
            points: vec![Point::default()],
            color: 0,
            thickness: 2.,
            fill: false,
            text: String::new(),
            number: 1,
        };
        h.push(a.clone());
        h.undo();
        assert!(h.items.is_empty());
        h.redo();
        assert_eq!(h.items.len(), 1);
        h.undo();
        h.push(a);
        h.redo();
        assert_eq!(h.items.len(), 1);
    }
    #[test]
    fn line_hit_is_not_bounding_box() {
        let a = Annotation {
            tool: Tool::Line,
            points: vec![Point { x: 0, y: 0 }, Point { x: 100, y: 100 }],
            color: 0,
            thickness: 1.,
            fill: false,
            text: String::new(),
            number: 0,
        };
        assert!(a.hit(Point { x: 50, y: 50 }));
        assert!(!a.hit(Point { x: 5, y: 90 }));
    }
}
