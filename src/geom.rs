#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
    pub fn width(&self) -> i32 {
        self.right.saturating_sub(self.left)
    }
    pub fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reverse_drag_and_zero_area() {
        assert_eq!(
            Rect::from_points(Point { x: 9, y: 8 }, Point { x: -4, y: 2 }),
            Rect::new(-4, 2, 9, 8)
        );
        assert!(Rect::from_points(Point { x: 2, y: 2 }, Point { x: 2, y: 2 }).is_empty());
    }
    #[test]
    fn negative_monitor_and_half_open_edges() {
        let r = Rect::new(-1920, 0, 0, 1080);
        assert!(r.contains(Point { x: -1, y: 1 }));
        assert!(!r.contains(Point { x: 0, y: 1 }));
        assert_eq!(
            r.intersect(Rect::new(-20, -20, 30, 30)),
            Rect::new(-20, 0, 0, 30)
        );
    }
    #[test]
    fn toolbar_always_inside_small_monitor() {
        let screen = Rect::new(-500, -200, -100, 100);
        let bar = place_toolbar(Rect::new(-120, 0, -100, 90), screen, 900, 70, 8);
        assert!(
            bar.left >= screen.left
                && bar.right <= screen.right
                && bar.top >= screen.top
                && bar.bottom <= screen.bottom
        );
    }
    #[test]
    fn moving_preserves_size() {
        let r = Rect::new(-20, 20, 80, 60).clamp_to(Rect::new(0, 0, 200, 100));
        assert_eq!(r, Rect::new(0, 20, 100, 60));
    }
    #[test]
    fn snap_diagonal() {
        let p = snap_45(Point { x: 0, y: 0 }, Point { x: 20, y: 17 });
        assert_eq!(p.x, p.y);
    }
    #[test]
    fn every_handle_resizes() {
        let r = Rect::new(10, 10, 30, 30);
        for (h, p) in r.handles().into_iter().enumerate() {
            assert_eq!(r.hit_handle(p, 3), Some(h));
            assert!(!resize(r, h, Point { x: 5, y: 5 }).is_empty());
        }
    }
}
impl Rect {
    pub fn from_points(a: Point, b: Point) -> Self {
        Self::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
    }
    pub fn normalized(self) -> Self {
        Self::from_points(
            Point {
                x: self.left,
                y: self.top,
            },
            Point {
                x: self.right,
                y: self.bottom,
            },
        )
    }
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }
    pub fn intersect(self, r: Self) -> Self {
        let left = self.left.max(r.left);
        let top = self.top.max(r.top);
        Self::new(
            left,
            top,
            self.right.min(r.right).max(left),
            self.bottom.min(r.bottom).max(top),
        )
    }
    pub fn translate(self, dx: i32, dy: i32) -> Self {
        Self::new(
            self.left.saturating_add(dx),
            self.top.saturating_add(dy),
            self.right.saturating_add(dx),
            self.bottom.saturating_add(dy),
        )
    }
    pub fn clamp_to(self, b: Self) -> Self {
        let w = self.width().max(0).min(b.width().max(0));
        let h = self.height().max(0).min(b.height().max(0));
        let x = self.left.clamp(b.left, b.right - w);
        let y = self.top.clamp(b.top, b.bottom - h);
        Self::new(x, y, x + w, y + h)
    }
    pub fn center(self) -> Point {
        Point {
            x: self.left + self.width() / 2,
            y: self.top + self.height() / 2,
        }
    }
    pub fn handles(self) -> [Point; 8] {
        let x = self.left + self.width() / 2;
        let y = self.top + self.height() / 2;
        [
            Point {
                x: self.left,
                y: self.top,
            },
            Point { x, y: self.top },
            Point {
                x: self.right,
                y: self.top,
            },
            Point { x: self.right, y },
            Point {
                x: self.right,
                y: self.bottom,
            },
            Point { x, y: self.bottom },
            Point {
                x: self.left,
                y: self.bottom,
            },
            Point { x: self.left, y },
        ]
    }
    pub fn hit_handle(self, p: Point, r: i32) -> Option<usize> {
        self.handles()
            .iter()
            .position(|h| (h.x - p.x).abs() <= r && (h.y - p.y).abs() <= r)
    }
}
pub fn resize(mut r: Rect, h: usize, d: Point) -> Rect {
    if matches!(h, 0 | 6 | 7) {
        r.left += d.x
    }
    if matches!(h, 2..=4) {
        r.right += d.x
    }
    if matches!(h, 0..=2) {
        r.top += d.y
    }
    if matches!(h, 4..=6) {
        r.bottom += d.y
    }
    r.normalized()
}
pub fn place_toolbar(s: Rect, b: Rect, w: i32, h: i32, gap: i32) -> Rect {
    let w = w.min(b.width()).max(0);
    let h = h.min(b.height()).max(0);
    let x = s.center().x - w / 2;
    let y = if s.bottom + gap + h <= b.bottom {
        s.bottom + gap
    } else if s.top - gap - h >= b.top {
        s.top - gap - h
    } else {
        s.bottom - gap - h
    };
    Rect::new(x, y, x + w, y + h).clamp_to(b)
}
pub fn snap_45(a: Point, b: Point) -> Point {
    let dx = (b.x - a.x) as f64;
    let dy = (b.y - a.y) as f64;
    let angle = (dy.atan2(dx) / std::f64::consts::FRAC_PI_4).round() * std::f64::consts::FRAC_PI_4;
    let len = dx.hypot(dy);
    Point {
        x: a.x + (len * angle.cos()).round() as i32,
        y: a.y + (len * angle.sin()).round() as i32,
    }
}
pub fn place_callout(p: Point, work: Rect, width: i32, height: i32, gap: i32) -> Rect {
    let candidates = [
        Rect::new(p.x + gap, p.y + gap, p.x + gap + width, p.y + gap + height),
        Rect::new(p.x - gap - width, p.y + gap, p.x - gap, p.y + gap + height),
        Rect::new(p.x + gap, p.y - gap - height, p.x + gap + width, p.y - gap),
        Rect::new(p.x - gap - width, p.y - gap - height, p.x - gap, p.y - gap),
    ];
    if let Some(r) = candidates.iter().find(|r| r.intersect(work) == **r) {
        return *r;
    }
    candidates
        .into_iter()
        .map(|r| r.clamp_to(work))
        .max_by_key(|r| {
            let dx = if p.x < r.left {
                r.left - p.x
            } else if p.x >= r.right {
                p.x - r.right + 1
            } else {
                0
            };
            let dy = if p.y < r.top {
                r.top - p.y
            } else if p.y >= r.bottom {
                p.y - r.bottom + 1
            } else {
                0
            };
            (dx as i64).pow(2) + (dy as i64).pow(2)
        })
        .unwrap_or(work)
}
pub fn place_size_label(selection: Rect, work: Rect, width: i32, height: i32, gap: i32) -> Rect {
    let y = if selection.top - height - gap >= work.top {
        selection.top - height - gap
    } else {
        selection.top + gap
    };
    Rect::new(selection.left, y, selection.left + width, y + height).clamp_to(work)
}
#[cfg(test)]
mod visual_tests {
    use super::*;
    #[test]
    fn magnifier_never_covers_cursor_at_screen_edges() {
        let work = Rect::new(-1920, 0, 0, 1080);
        for p in [
            Point { x: -1, y: 1079 },
            Point { x: -1920, y: 0 },
            Point { x: -960, y: 540 },
            Point { x: -1, y: 0 },
        ] {
            let r = place_callout(p, work, 136, 180, 24);
            assert_eq!(r.intersect(work), r);
            assert!(!r.contains(p));
        }
    }
    #[test]
    fn size_label_prefers_top_left_then_inside() {
        let work = Rect::new(0, 0, 1920, 1080);
        let above = place_size_label(Rect::new(120, 100, 800, 500), work, 90, 26, 8);
        assert_eq!(above.left, 120);
        assert_eq!(above.bottom, 92);
        let inside = place_size_label(Rect::new(120, 0, 800, 500), work, 90, 26, 8);
        assert_eq!(inside.top, 8);
    }
}
