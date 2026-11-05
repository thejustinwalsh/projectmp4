//! Safe wrapper over the libprojectM 4.x C API (vendored, statically linked).

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]
#[allow(clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use std::borrow::Cow;
use std::ffi::{c_char, c_void, CString};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// A GL context an [`Instance`] can re-make current before it frees GL objects.
/// The headless [`crate::gl_context::GlContext`] and the preview window's
/// context both implement it.
pub trait MakeCurrent {
    fn make_current(&self) -> Result<()>;
}

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
/// - libprojectM's GL resolver is process-global and "first create wins": the
///   `load_proc` handed to the second and later instances is ignored. That is
///   harmless here only because `gl_context::gl_load_proc` is a context-free
///   `static fn` that `dlsym`s OpenGL.framework: every instance would have
///   supplied the same resolver, so there is nothing to win. Swap in a loader
///   that closes over per-context state and `Instance` becomes a true
///   per-process singleton.
/// - Holds a raw pointer, so it is `!Send` and `!Sync`.
/// - The borrow of the [`GlContext`] this instance was created against does
///   double duty: it stops the context being dropped first, *and* it lets
///   `Drop` make the context current again before `projectm_destroy`. Drop
///   order alone is not enough, because tearing down some *other* `GlContext`
///   in between leaves no context current at all, and projectM's `glDelete*`
///   calls then silently become no-ops that leak GPU objects.
///   Construct one with [`GlContext::create_instance`], not directly.
pub struct Instance<'gl> {
    handle: sys::projectm_handle,
    gl: &'gl dyn MakeCurrent,
    /// Bumped by [`on_preset_switch_failed`]. Boxed so its address is stable
    /// across moves of the `Instance`; projectM holds that address as the
    /// callback's user data for the life of the instance.
    load_failures: Box<AtomicU64>,
}

/// projectM calls this synchronously from inside `projectm_load_preset_file`
/// when the preset cannot be parsed or found
/// (`vendor/projectm/src/libprojectM/ProjectM.cpp:70`). The body is wrapped in
/// `catch_unwind` because unwinding out of an `extern "C"` fn aborts; it is in
/// practice panic-free, doing nothing but one relaxed increment.
unsafe extern "C" fn on_preset_switch_failed(
    _filename: *const c_char,
    _message: *const c_char,
    user_data: *mut c_void,
) {
    let _ = std::panic::catch_unwind(|| {
        if user_data.is_null() {
            return;
        }
        unsafe { &*(user_data as *const AtomicU64) }.fetch_add(1, Ordering::Relaxed);
    });
}

impl<'gl> Instance<'gl> {
    /// [`GlContext::create_instance`] is the only caller; it has already made
    /// `gl` current. Holding the reference is what turns the context-currency
    /// invariant from a comment into something the type system carries.
    pub(crate) fn new(gl: &'gl dyn MakeCurrent, loader: LoadProc) -> Result<Instance<'gl>> {
        let handle = unsafe {
            sys::projectm_create_with_opengl_load_proc(Some(loader), std::ptr::null_mut())
        };
        if handle.is_null() {
            Err(Error::msg(
                "projectm_create_with_opengl_load_proc returned null",
            ))
        } else {
            let load_failures = Box::new(AtomicU64::new(0));
            unsafe {
                sys::projectm_set_preset_switch_failed_event_callback(
                    handle,
                    Some(on_preset_switch_failed),
                    &*load_failures as *const AtomicU64 as *mut c_void,
                )
            };
            Ok(Instance {
                handle,
                gl,
                load_failures,
            })
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

    /// Directories projectM searches for the `sampler` textures presets name.
    /// Must be set before the presets that need them are loaded.
    pub fn set_texture_search_paths(&self, paths: &[&Path]) -> Result<()> {
        let c: Vec<CString> = paths
            .iter()
            .map(|p| {
                p.to_str()
                    .and_then(|s| CString::new(s).ok())
                    .ok_or_else(|| Error::msg(format!("texture path {} is not valid UTF-8 without NULs", p.display())))
            })
            .collect::<Result<_>>()?;
        let ptrs: Vec<*const c_char> = c.iter().map(|s| s.as_ptr()).collect();
        unsafe { sys::projectm_set_texture_search_paths(self.handle, ptrs.as_ptr() as *mut *const c_char, ptrs.len()) };
        Ok(())
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

    /// Loads a preset file, failing if projectM could not parse or find it.
    ///
    /// `projectm_load_preset_file` itself returns `void` and only logs: on
    /// failure the previous preset -- or the idle "M" logo -- silently stays on
    /// screen. The preset-switch-failed callback registered in
    /// [`Instance::new`] fires synchronously from inside that call, so counting
    /// its invocations across the call turns the silence into a `Result`.
    /// projectM's log line on stderr carries the reason.
    pub fn load_preset_file(&self, path: &Path, smooth_transition: bool) -> Result<()> {
        let s = path
            .to_str()
            .ok_or_else(|| Error::msg("preset path is not valid UTF-8"))?;
        let c = CString::new(s).map_err(|_| Error::msg("preset path contains an interior NUL"))?;
        let before = self.load_failures.load(Ordering::Relaxed);
        unsafe { sys::projectm_load_preset_file(self.handle, c.as_ptr(), smooth_transition) };
        if self.load_failures.load(Ordering::Relaxed) == before {
            Ok(())
        } else {
            Err(Error::msg(format!(
                "projectM could not load preset {s} (reason logged to stderr)"
            )))
        }
    }
}

/// Destroys the instance, re-making its context current first: something else
/// may have changed or cleared currency since the last render, and
/// `projectm_destroy` frees GL resources.
impl Drop for Instance<'_> {
    fn drop(&mut self) {
        let _ = self.gl.make_current();
        unsafe { sys::projectm_destroy(self.handle) };
    }
}
