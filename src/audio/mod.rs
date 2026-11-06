pub mod normalise;

pub use normalise::{normalise, normalise_with_ffmpeg, wav_frames, Source};
pub mod analysis;
pub use analysis::{analyse, analyse_outcome, analyse_outcome_with_cache_dir, Analysis, Outcome};
pub mod driver;
pub use driver::{Driver, DriverKind, MAX_WINDOW_FRAMES};
pub mod timeline;
pub mod sections;
