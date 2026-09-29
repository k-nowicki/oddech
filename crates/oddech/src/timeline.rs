//! Timeline view math: mapping between seconds and pixels, zoom and scroll.
//! Kept free of egui types so it can be unit-tested.

/// Deepest zoom: 10 µs per pixel (1 ms spans 100 px).
pub const MIN_SECS_PER_PX: f64 = 1e-5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineView {
    /// Time at the left edge of the lanes, in seconds.
    pub start: f64,
    pub secs_per_px: f64,
    /// Width of the lane area in pixels.
    pub width: f64,
    /// Total timeline length in seconds.
    pub duration: f64,
}

impl TimelineView {
    pub fn new(duration: f64) -> Self {
        let mut v = Self { start: 0.0, secs_per_px: 0.01, width: 1000.0, duration: duration.max(1e-3) };
        v.fit();
        v
    }

    pub fn end(&self) -> f64 {
        self.start + self.width * self.secs_per_px
    }

    pub fn visible_secs(&self) -> f64 {
        self.width * self.secs_per_px
    }

    /// Pixel offset (from the lane's left edge) of time `t`.
    pub fn x_of(&self, t: f64) -> f64 {
        (t - self.start) / self.secs_per_px
    }

    pub fn t_of(&self, x: f64) -> f64 {
        self.start + x * self.secs_per_px
    }

    fn max_secs_per_px(&self) -> f64 {
        // Allow zooming out to a bit more than the whole file.
        (self.duration * 1.1 / self.width.max(1.0)).max(MIN_SECS_PER_PX)
    }

    /// Updates the width while keeping the time under the left edge fixed.
    pub fn set_width(&mut self, width: f64) {
        if (width - self.width).abs() > 0.5 {
            let was_fit = (self.secs_per_px - self.fit_secs_per_px()).abs() < 1e-12;
            self.width = width.max(1.0);
            if was_fit {
                self.fit();
            }
            self.clamp();
        }
    }

    fn fit_secs_per_px(&self) -> f64 {
        (self.duration / self.width.max(1.0)).max(MIN_SECS_PER_PX)
    }

    /// Shows the whole file.
    pub fn fit(&mut self) {
        self.secs_per_px = self.fit_secs_per_px();
        self.start = 0.0;
    }

    /// Zooms by `factor` (> 1 zooms in) keeping the time under pixel `x`
    /// in place.
    pub fn zoom_at(&mut self, x: f64, factor: f64) {
        let anchor = self.t_of(x);
        self.secs_per_px = (self.secs_per_px / factor).clamp(MIN_SECS_PER_PX, self.max_secs_per_px());
        self.start = anchor - x * self.secs_per_px;
        self.clamp();
    }

    pub fn scroll_px(&mut self, dx: f64) {
        self.start += dx * self.secs_per_px;
        self.clamp();
    }

    /// Keeps the view inside the timeline (with a small margin at the end).
    pub fn clamp(&mut self) {
        let span = self.visible_secs();
        let max_start = (self.duration - span * 0.9).max(0.0);
        self.start = self.start.clamp(-span * 0.02, max_start.max(-span * 0.02));
        if span >= self.duration {
            // Zoomed out past the whole file: pin to the start.
            self.start = self.start.min(0.0).max(-span * 0.02);
        }
    }

    /// Scrolls so that `t` is visible; `margin` is a fraction of the width
    /// kept free on the right when following the playhead.
    pub fn ensure_visible(&mut self, t: f64, margin: f64) {
        let span = self.visible_secs();
        if t < self.start || t > self.end() - span * margin {
            self.start = t - span * 0.1;
            self.clamp();
        }
    }

    /// Centres `[a, b]` in the view, zooming out if it does not fit.
    pub fn reveal(&mut self, a: f64, b: f64) {
        let span = self.visible_secs();
        if a >= self.start && b <= self.end() {
            return;
        }
        if b - a > span * 0.8 {
            self.secs_per_px = ((b - a) / (self.width * 0.8)).clamp(MIN_SECS_PER_PX, self.max_secs_per_px());
        }
        self.start = (a + b) / 2.0 - self.visible_secs() / 2.0;
        self.clamp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> TimelineView {
        let mut v = TimelineView::new(600.0);
        v.set_width(1000.0);
        v
    }

    #[test]
    fn fit_shows_everything() {
        let v = view();
        assert_eq!(v.start, 0.0);
        assert!((v.end() - 600.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_keeps_anchor_under_cursor() {
        let mut v = view();
        let x = 730.0;
        let t = v.t_of(x);
        v.zoom_at(x, 8.0);
        assert!((v.t_of(x) - t).abs() < 1e-9);
        v.zoom_at(x, 1.0 / 2.0);
        assert!((v.t_of(x) - t).abs() < 1e-9);
    }

    #[test]
    fn zoom_is_bounded() {
        let mut v = view();
        v.zoom_at(500.0, 1e12);
        assert_eq!(v.secs_per_px, MIN_SECS_PER_PX);
        v.zoom_at(500.0, 1e-12);
        assert!(v.visible_secs() <= 600.0 * 1.1 + 1e-9);
    }

    #[test]
    fn scroll_is_clamped() {
        let mut v = view();
        v.zoom_at(0.0, 10.0);
        v.scroll_px(-1e9);
        assert!(v.start <= 0.0 && v.start > -v.visible_secs() * 0.05);
        v.scroll_px(1e12);
        assert!(v.start < 600.0);
    }

    #[test]
    fn reveal_and_follow() {
        let mut v = view();
        v.zoom_at(0.0, 60.0); // 10 s visible
        v.reveal(300.0, 301.0);
        assert!(v.start <= 300.0 && v.end() >= 301.0);
        v.ensure_visible(400.0, 0.1);
        assert!(v.start <= 400.0 && v.end() >= 400.0);
    }
}
