//! Safe wrapper over the libprojectM 4.x C API (vendored, statically linked).

#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code)]
#[allow(clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use std::borrow::Cow;
use std::ffi::{c_char, c_void, CString};
use std::path::Path;

/// OpenGL symbol loader: given a NUL-terminated name, returns the function pointer.
pub type LoadProc = unsafe extern "C" fn(name: *const c_char, user: *mut c_void) -> *mut c_void;

#[derive(Debug)]
pub struct Error(pub Cow<'static, str>);

impl Error {
    pub fn msg(message: impl Into<Cow<'static, str>>) -> Error {
        Error(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Returns the (major, minor, patch) of the linked libprojectM.
pub fn version() -> (u32, u32, u32) {
    let (mut a, mut b, mut c) = (0i32, 0i32, 0i32);
    unsafe { sys::projectm_get_version_components(&mut a, &mut b, &mut c) };
    (a as u32, b as u32, c as u32)
}

/// Link-level probe: addresses of the master-only symbols this project depends on
/// (`projectm_set_frame_time`, `projectm_opengl_render_frame_fbo`). Referencing them
/// forces a link error if a submodule bump drops them. Returns true if both are non-null.
pub fn master_symbols_present() -> bool {
    // `black_box` keeps the optimiser from folding `addr != 0` to `true` and
    // discarding the reference: without it, `cargo test --release` passes even
    // if the symbols are gone. The linker must still be asked to resolve them.
    let a = std::hint::black_box(sys::projectm_set_frame_time as *const () as usize);
    let b = std::hint::black_box(sys::projectm_opengl_render_frame_fbo as *const () as usize);
    a != 0 && b != 0
}

/// Audio channel layout of a buffer passed to [`Instance::pcm_add_float`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    Mono,
    Stereo,
}

impl Channels {
    fn count(self) -> usize {
        match self {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        }
    }

    fn raw(self) -> sys::projectm_channels {
        match self {
            Channels::Mono => sys::projectm_channels_PROJECTM_MONO,
            Channels::Stereo => sys::projectm_channels_PROJECTM_STEREO,
        }
    }
}

/// RAII owner of a `projectm_handle`.
///
/// Invariants:
/// - A current OpenGL context is required at creation, for every rendering call,
///   and when the `Instance` is dropped (`projectm_destroy` frees GL resources).
/// - A Rust `LoadProc` callback must wrap its body in `std::panic::catch_unwind`;
///   unwinding out of an `extern "C"` fn aborts the process.
/// - libprojectM's GL resolver is process-global and "first create wins": later
///   instances ignore their `load_proc`, so treat `Instance` as a per-process singleton.
/// - Holds a raw pointer, so it is `!Send` and `!Sync`.
pub struct Instance {
    handle: sys::projectm_handle,
}

impl Instance {
    pub fn new(load_proc: LoadProc) -> Result<Instance> {
        let handle = unsafe {
            sys::projectm_create_with_opengl_load_proc(Some(load_proc), std::ptr::null_mut())
        };
        if handle.is_null() {
            Err(Error::msg(
                "projectm_create_with_opengl_load_proc returned null",
            ))
        } else {
            Ok(Instance { handle })
        }
    }

    #[allow(dead_code)]
    pub(crate) fn raw(&self) -> sys::projectm_handle {
        self.handle
    }

    /// Render/viewport size in pixels. Must match the offscreen framebuffer.
    pub fn set_window_size(&self, width: u32, height: u32) {
        unsafe { sys::projectm_set_window_size(self.handle, width as usize, height as usize) };
    }

    /// Per-pixel equation mesh resolution (MilkDrop's `meshx`/`meshy`).
    pub fn set_mesh_size(&self, width: u32, height: u32) {
        unsafe { sys::projectm_set_mesh_size(self.handle, width as usize, height as usize) };
    }

    /// Nominal frame rate. Presets read it, and it seeds projectM's timers.
    pub fn set_fps(&self, fps: i32) {
        unsafe { sys::projectm_set_fps(self.handle, fps) };
    }

    pub fn set_beat_sensitivity(&self, sensitivity: f32) {
        unsafe { sys::projectm_set_beat_sensitivity(self.handle, sensitivity) };
    }

    /// Pins the next frame's timestamp, in seconds since the first frame.
    /// Setting it every frame is what makes offline rendering deterministic
    /// instead of wall-clock driven.
    pub fn set_frame_time(&self, seconds_since_first_frame: f64) {
        unsafe { sys::projectm_set_frame_time(self.handle, seconds_since_first_frame) };
    }

    /// Appends interleaved f32 PCM in the range -1.0..=1.0 (stereo order LRLR).
    /// `samples.len()` must be a whole number of frames for `channels`.
    pub fn pcm_add_float(&self, samples: &[f32], channels: Channels) {
        let per_channel = samples.len() / channels.count();
        if per_channel == 0 {
            return;
        }
        unsafe {
            sys::projectm_pcm_add_float(
                self.handle,
                samples.as_ptr(),
                per_channel as std::ffi::c_uint,
                channels.raw(),
            )
        };
    }

    /// Renders one frame into `fbo`. Requires the creating GL context to be
    /// current. projectM binds `fbo` as `GL_DRAW_FRAMEBUFFER` and sets the
    /// viewport from the window size; it does not touch the read framebuffer.
    pub fn render_frame_fbo(&self, fbo: u32) {
        unsafe { sys::projectm_opengl_render_frame_fbo(self.handle, fbo) };
    }

    /// Loads a preset file. projectM reports failure only through its log --
    /// the current preset simply stays put -- so an `Err` here means the path
    /// could not be turned into a C string, not that the preset is valid.
    pub fn load_preset_file(&self, path: &Path, smooth_transition: bool) -> Result<()> {
        let s = path
            .to_str()
            .ok_or_else(|| Error::msg("preset path is not valid UTF-8"))?;
        let c = CString::new(s).map_err(|_| Error::msg("preset path contains an interior NUL"))?;
        unsafe { sys::projectm_load_preset_file(self.handle, c.as_ptr(), smooth_transition) };
        Ok(())
    }
}

/// Destroys the instance. The GL context that was current at creation must be
/// current here, or GL resource deletion is undefined.
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { sys::projectm_destroy(self.handle) };
    }
}
