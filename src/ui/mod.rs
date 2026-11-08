pub mod annotate;
pub mod d2d;
pub mod export;
pub mod icons;
pub mod overlay;
pub mod pin;
pub mod settings;
pub mod toolbar;
pub use overlay::{run_overlay, OverlayResult};

/// Creates only a hidden, preallocated capture surface; never shows a window.
pub fn warm_up() {
    d2d::warm_up();
    overlay::warm_up_surface();
}
