pub mod normalise;

pub use normalise::{normalise, normalise_with_ffmpeg, wav_frames, Source};
pub mod analysis;
pub use analysis::{analyse, analyse_outcome, Analysis, Outcome};
pub mod driver;
pub use driver::{Driver, MAX_WINDOW_FRAMES};
pub mod timeline;
pub mod sections;
