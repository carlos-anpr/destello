use super::{
    annotate::Tool,
    d2d::{self, theme, Canvas, TextStyle},
    icons::{self, Icon},
};
use crate::geom::{place_toolbar, Point, Rect};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Tool(Tool),
    Undo,
    Redo,
    Pin,
    SaveAs,
    Copy,
    Close,
    Color(u32),
    CustomColor,
    Thickness(u8),
    Fill,
}
impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tool(t) => t.label(),
            Self::Undo => "Deshacer · Ctrl+Z",
            Self::Redo => "Rehacer · Ctrl+Y",
            Self::Pin => "Fijar · Ctrl+P",
            Self::SaveAs => "Guardar como… · Ctrl+S / Ctrl+Shift+S",
            Self::Copy => "Copiar · Enter",
            Self::Close => "Cerrar · Esc",
            Self::Color(_) => "Color de la anotación",
            Self::CustomColor => "Color personalizado…",
            Self::Thickness(2) => "Trazo fino · 2 px",
            Self::Thickness(4) => "Trazo medio · 4 px",
            Self::Thickness(_) => "Trazo grueso · 8 px",
            Self::Fill => "Relleno / fondo de texto",
        }
    }
    pub fn enabled(self, state: &ToolbarState) -> bool {
        match self {
            Self::Undo => state.can_undo,
            Self::Redo => state.can_redo,
            _ => true,
        }
    }
}
pub struct ToolbarState {
    pub tool: Tool,
    pub color: u32,
    pub thickness: f32,
    pub fill: bool,
    pub hover: Option<Button>,
    pub previous_hover: Option<Button>,
    pub hover_progress: f32,
    pub pressed: Option<Button>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub opacity: f32,
}
pub struct Toolbar {
    pub bounds: Rect,
    pub cells: Vec<(Rect, Button)>,
    pub scale: f32,
    separators: Vec<Rect>,
    value: Rect,
}
const GROUPS: [usize; 7] = [1, 4, 4, 2, 2, 2, 2];
fn buttons() -> Vec<Button> {
    Tool::ALL
        .into_iter()
        .map(Button::Tool)
        .chain([
            Button::Undo,
            Button::Redo,
            Button::Pin,
            Button::SaveAs,
            Button::Copy,
            Button::Close,
        ])
        .collect()
}
impl Toolbar {
    pub fn new(selection: Rect, work: Rect, scale: f32) -> Self {
        let mut scale = scale.max(0.1);
        let (width, height, cells, separators, value) = loop {
            let px = |n: f32| (n * scale).round().max(1.) as i32;
            let pad = px(10.);
            let size = px(34.);
            let gap = px(2.);
            let group_gap = px(12.);
            let width = px(702.).min(work.width()).max(1);
            let right = width - pad;
            let (mut x, mut y) = (pad, pad);
            let mut cells = Vec::new();
            let mut separators = Vec::new();
            let mut group = 0;
            let mut end = GROUPS[0];
            for (i, b) in buttons().into_iter().enumerate() {
                if i == end {
                    group += 1;
                    end += GROUPS[group];
                    if x + group_gap + size <= right {
                        let sx = x + group_gap / 2;
                        separators.push(Rect::new(sx, y + px(7.), sx + px(1.), y + px(27.)));
                        x += group_gap;
                    }
                }
                if x + size > right && x > pad {
                    x = pad;
                    y += size + gap;
                }
                cells.push((Rect::new(x, y, x + size, y + size), b));
                x += size + gap;
            }
            y += size + px(10.);
            let property_top = y;
            let sw = px(24.);
            let custom = px(28.);
            let value_width = px(52.);
            let fill = px(32.);
            // Property controls use a centered row on normal displays and wrap as a unit on small monitors.
            let total = 8 * sw + custom + 3 * sw + value_width + fill + 2 * group_gap;
            x = if total <= right - pad {
                (width - total) / 2
            } else {
                pad
            };
            let mut value = Rect::default();
            let properties = [
                0x8b5cf6, 0xef4444, 0xf97316, 0xfacc15, 0x34d399, 0x38bdf8, 0xffffff, 0x161820,
            ]
            .into_iter()
            .map(|c| (sw, Some(Button::Color(c))))
            .chain([
                (custom, Some(Button::CustomColor)),
                (sw, Some(Button::Thickness(2))),
                (sw, Some(Button::Thickness(4))),
                (sw, Some(Button::Thickness(8))),
                (value_width, None),
                (fill, Some(Button::Fill)),
            ]);
            for (i, (w, b)) in properties.enumerate() {
                if matches!(i, 9 | 13) && x + group_gap + w <= right {
                    let sx = x + group_gap / 2;
                    separators.push(Rect::new(sx, y + px(5.), sx + px(1.), y + px(21.)));
                    x += group_gap;
                }
                if x + w > right && x > pad {
                    x = pad;
                    y += sw + px(4.);
                }
                let r = Rect::new(x, y, x + w, y + sw);
                if let Some(b) = b {
                    cells.push((r, b))
                } else {
                    value = r
                }
                x += w;
            }
            let height = (y + sw + pad).max(property_top + sw + pad);
            if height <= work.height() || scale <= 0.1 {
                break (width, height.min(work.height()), cells, separators, value);
            }
            scale *= 0.9;
        };
        let bounds = place_toolbar(selection, work, width, height, (12. * scale) as i32);
        let offset = |r: Rect| r.translate(bounds.left, bounds.top).intersect(bounds);
        Self {
            bounds,
            cells: cells.into_iter().map(|(r, b)| (offset(r), b)).collect(),
            scale,
            separators: separators.into_iter().map(offset).collect(),
            value: offset(value),
        }
    }
    pub fn hit(&self, p: Point) -> Option<Button> {
        self.cells
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, b)| *b)
    }
    pub fn paint(&self, c: &Canvas<'_>, s: &ToolbarState) -> windows::core::Result<()> {
        let scale = self.scale;
        let expand = (32. * scale) as i32;
        let layer = Rect::new(
            self.bounds.left - expand,
            self.bounds.top - expand,
            self.bounds.right + expand,
            self.bounds.bottom + expand,
        );
        c.opacity(layer, s.opacity, || {
            c.panel(self.bounds, scale, 1.)?;
            for r in &self.separators {
                c.fill(*r, 0xffffff, 0.1)?;
            }
            for (r, b) in &self.cells {
                let enabled = b.enabled(s);
                let active = *b == Button::Tool(s.tool)
                    || (*b == Button::Fill && s.fill)
                    || matches!(b,Button::Thickness(v)if (*v as f32-s.thickness).abs()<0.5);
                let hover = if s.hover == Some(*b) {
                    s.hover_progress
                } else if s.previous_hover == Some(*b) {
                    1. - s.hover_progress
                } else {
                    0.
                };
                let primary = *b == Button::Copy;
                if primary {
                    c.rounded(*r, 8. * scale, theme::ACCENT, 1.)?
                } else if active {
                    c.rounded(*r, 8. * scale, theme::ACCENT, 0.22)?;
                    let center = r.center();
                    c.rounded(
                        Rect::new(
                            center.x - (4. * scale) as i32,
                            r.bottom - (4. * scale) as i32,
                            center.x + (4. * scale) as i32,
                            r.bottom - (2. * scale) as i32,
                        ),
                        scale,
                        theme::ACCENT,
                        1.,
                    )?;
                }
                if enabled && hover > 0. {
                    c.rounded(
                        *r,
                        8. * scale,
                        if *b == Button::Close {
                            theme::DANGER
                        } else {
                            0xffffff
                        },
                        if s.pressed == Some(*b) { 0.14 } else { 0.08 } * hover,
                    )?;
                }
                let col = if !enabled {
                    d2d::mix(theme::SURFACE, theme::TEXT, 0.35)
                } else if active {
                    theme::ACCENT
                } else {
                    theme::TEXT
                };
                let center = r.center();
                let icon_size = (18. * scale).round() as i32;
                let ir = Rect::new(
                    center.x - icon_size / 2,
                    center.y - icon_size / 2,
                    center.x + (icon_size + 1) / 2,
                    center.y + (icon_size + 1) / 2,
                );
                let icon = match b {
                    Button::Tool(t) => Some(Icon::Tool(*t)),
                    Button::Undo => Some(Icon::Undo),
                    Button::Redo => Some(Icon::Redo),
                    Button::Pin => Some(Icon::Pin),
                    Button::SaveAs => Some(Icon::SaveAs),
                    Button::Copy => Some(Icon::Copy),
                    Button::Close => Some(Icon::Close),
                    _ => None,
                };
                if let Some(icon) = icon {
                    icons::draw(c, icon, ir, col)?;
                }
                match b {
                    Button::Color(v) => {
                        let n = (8. * scale).round() as i32;
                        let circle =
                            Rect::new(center.x - n, center.y - n, center.x + n, center.y + n);
                        c.ellipse(circle, *v, 1., 1., true)?;
                        if *v == s.color {
                            let n = (11. * scale).round() as i32;
                            c.ellipse(
                                Rect::new(center.x - n, center.y - n, center.x + n, center.y + n),
                                theme::TEXT,
                                1.,
                                2. * scale,
                                false,
                            )?;
                        }
                    }
                    Button::CustomColor => {
                        c.ellipse(ir, theme::MUTED, 1., 1.6 * scale, false)?;
                        let d = (4. * scale) as i32;
                        c.line(
                            Point {
                                x: center.x - d,
                                y: center.y,
                            },
                            Point {
                                x: center.x + d,
                                y: center.y,
                            },
                            col,
                            1.,
                            1.6 * scale,
                        )?;
                        c.line(
                            Point {
                                x: center.x,
                                y: center.y - d,
                            },
                            Point {
                                x: center.x,
                                y: center.y + d,
                            },
                            col,
                            1.,
                            1.6 * scale,
                        )?;
                    }
                    Button::Thickness(v) => {
                        let d = ((*v as f32 * 0.5 + 1.) * scale).round() as i32;
                        c.ellipse(
                            Rect::new(center.x - d, center.y - d, center.x + d, center.y + d),
                            col,
                            1.,
                            1.,
                            true,
                        )?;
                    }
                    Button::Fill => {
                        c.rounded_border(ir, 3. * scale, col, 1., 1.6 * scale)?;
                        if s.fill {
                            let d = (4. * scale) as i32;
                            c.rounded(
                                Rect::new(center.x - d, center.y - d, center.x + d, center.y + d),
                                scale,
                                col,
                                1.,
                            )?;
                        }
                    }
                    _ => {}
                }
            }
            c.centered(
                &format!("{:.0} px", s.thickness),
                self.value,
                11. * scale,
                theme::MUTED,
                TextStyle::Body,
            )?;
            Ok(())
        })
    }
    pub fn tooltip(
        &self,
        c: &Canvas<'_>,
        button: Button,
        work: Rect,
        opacity: f32,
    ) -> windows::core::Result<()> {
        let Some((r, _)) = self.cells.iter().find(|(_, b)| *b == button) else {
            return Ok(());
        };
        let scale = self.scale;
        let (name, key) = button
            .label()
            .split_once(" · ")
            .unwrap_or((button.label(), ""));
        let (name_width, _) = c.text_size(name, 12. * scale, TextStyle::Body)?;
        let (key_width, _) = c.text_size(key, 11. * scale, TextStyle::Mono)?;
        let padding = (10. * scale) as i32;
        let key_width = if key.is_empty() {
            0
        } else {
            key_width + padding * 2
        };
        let width = name_width
            + padding * 2
            + if key.is_empty() {
                0
            } else {
                key_width + padding
            };
        let Some(bounds) = place_tooltip(
            self.bounds,
            *r,
            work,
            width,
            (34. * scale) as i32,
            (8. * scale) as i32,
        ) else {
            return Ok(());
        };
        let e = (32. * scale) as i32;
        c.opacity(
            Rect::new(
                bounds.left - e,
                bounds.top - e,
                bounds.right + e,
                bounds.bottom + e,
            ),
            opacity,
            || {
                c.panel(bounds, scale, 1.)?;
                c.label(
                    name,
                    Rect::new(
                        bounds.left + padding,
                        bounds.top,
                        bounds.left + padding + name_width,
                        bounds.bottom,
                    ),
                    12. * scale,
                    theme::TEXT,
                    TextStyle::Body,
                )?;
                if !key.is_empty() {
                    let key_rect = Rect::new(
                        bounds.right - padding - key_width,
                        bounds.top + (6. * scale) as i32,
                        bounds.right - padding,
                        bounds.bottom - (6. * scale) as i32,
                    );
                    c.rounded(key_rect, 4. * scale, 0xffffff, 0.07)?;
                    c.rounded_border(key_rect, 4. * scale, 0xffffff, 0.09, scale)?;
                    c.centered(key, key_rect, 11. * scale, theme::MUTED, TextStyle::Mono)?;
                }
                Ok(())
            },
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_toolbar_keeps_every_control_inside_monitor() {
        for (w, h) in [(320, 200), (640, 360), (1920, 1080)] {
            let work = Rect::new(-w, -h, 0, 0);
            let t = Toolbar::new(Rect::new(-40, -40, 0, 0), work, 2.);
            assert_eq!(t.cells.len(), 30);
            for (r, _) in &t.cells {
                assert_eq!(r.intersect(work), *r);
                assert!(!r.is_empty());
            }
            assert!(t.cells.iter().any(|(_, b)| *b == Button::Thickness(4)));
        }
    }
    #[test]
    fn primary_action_is_in_last_group() {
        let t = Toolbar::new(Rect::new(0, 0, 800, 500), Rect::new(0, 0, 1920, 1080), 1.);
        let copy = t
            .cells
            .iter()
            .find(|(_, b)| *b == Button::Copy)
            .map(|(r, _)| *r)
            .expect("copy");
        let close = t
            .cells
            .iter()
            .find(|(_, b)| *b == Button::Close)
            .map(|(r, _)| *r)
            .expect("close");
        assert_eq!(copy.top, close.top);
        assert!(copy.right <= close.left);
        assert!(t.separators.len() >= 8);
    }
}

fn place_tooltip(
    toolbar: Rect,
    anchor: Rect,
    work: Rect,
    width: i32,
    height: i32,
    gap: i32,
) -> Option<Rect> {
    let width = width.min(work.width());
    let x = (anchor.center().x - width / 2).clamp(work.left, work.right - width);
    let y = if toolbar.top - gap - height >= work.top {
        toolbar.top - gap - height
    } else if toolbar.bottom + gap + height <= work.bottom {
        toolbar.bottom + gap
    } else {
        return None;
    };
    Some(Rect::new(x, y, x + width, y + height))
}
#[cfg(test)]
mod tooltip_tests {
    use super::*;
    #[test]
    fn tooltip_never_intersects_toolbar_or_properties() {
        let work = Rect::new(0, 0, 1920, 1080);
        for toolbar in [
            Rect::new(300, 600, 1040, 688),
            Rect::new(0, 0, 740, 88),
            Rect::new(1180, 992, 1920, 1080),
        ] {
            let r = place_tooltip(
                toolbar,
                Rect::new(
                    toolbar.left,
                    toolbar.top,
                    toolbar.left + 34,
                    toolbar.top + 34,
                ),
                work,
                260,
                34,
                8,
            )
            .expect("space");
            assert!(r.intersect(toolbar).is_empty());
            assert_eq!(r.intersect(work), r);
            if toolbar.top >= 42 {
                assert!(r.bottom <= toolbar.top);
            }
        }
    }
    #[test]
    fn tooltip_is_hidden_if_neither_side_fits() {
        let work = Rect::new(0, 0, 200, 90);
        assert!(place_tooltip(work, work, work, 180, 34, 8).is_none());
    }
}
