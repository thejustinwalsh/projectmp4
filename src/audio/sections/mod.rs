//! Musical-section labelling from a single audio file.

pub mod clip;
pub mod frontend;
pub mod gguf;
pub mod model;
pub mod postprocess;
pub mod schema;
pub mod spectrogram;

pub use gguf::Weights;
pub use schema::SCHEMA;
pub use model::{estimated_working_bytes, predict};
pub use postprocess::{postprocess, Section, LABELS};
pub use clip::{choose, locate, section_energy, Chosen, ClipKind, Reason};
