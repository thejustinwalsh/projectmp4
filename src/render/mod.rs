//! Per-frame compositing of the title/art/watermark overlay.
pub mod mark;
pub mod plan;
pub mod r#loop;
pub mod overlay;
pub use overlay::{Layer, MarkStyle, Overlay};
