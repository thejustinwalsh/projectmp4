//! Realtime preview: the render's plan, played in a window with sound.
//!
//! Faithfulness is the point. Preview follows the same [`RenderPlan`] the
//! render will use -- same presets, same switch frames, same PCM feed, and the
//! same `n / fps` timestamp for frame `n` -- so a track previews the way it
//! will render. The keys are an override on top of that schedule.
//!
//! Framing is part of that. projectM's warp mesh samples outside the previous
//! frame at the edge of the viewport, so the outermost rows and columns smear.
//! The render has always drawn [`overscanned`] and kept the centre; the preview
//! now does the same, into its own framebuffer, and blits that centre to the
//! window -- so the preview shows the frame the render keeps and not the 5%
//! border the render crops. Both paths then apply the guard and the overlay to
//! the cropped frame, in that order.
//!
//! Timing is the opposite of the offline loop. The render keeps a virtual
//! clock (`t = n / fps`) and never drops a frame, because every frame of a
//! file must exist. Here the *audio device* is the clock: audio is continuous
//! and cannot stall, video can. Each pass asks the audio clock where we are,
//! draws the frame for that position (skipping the ones that are already
//! past), and counts what it skipped. The audio callback touches no GL and
//! takes no lock that can block it.
//!
//! The title/art/watermark overlay is drawn on the GPU from the render's own
//! tiles ([`crate::gl_overlay`]). Not reproduced: the render's replacement of a
//! preset that renders black, which needs a per-frame readback preview avoids.

use crate::audio::timeline::Cut;
use winit::keyboard::{KeyCode, PhysicalKey};
use crate::ffi::{Error, Instance, MakeCurrent, Result};
use crate::gl_context::{proc_loader, Offscreen};
use crate::gl_overlay::GlOverlay;
use crate::render::flash::FlashGuard;
use crate::render::r#loop::bass_per_frame;
use crate::render::Overlay;
use crate::render::plan::{overscanned, prepare, RenderArgs, RenderPlan};
use crate::render::r#loop::switch_frames;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Which preset is showing
// ---------------------------------------------------------------------------

/// A preset to load now.
#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    pub path: PathBuf,
    /// The `smooth_transition` flag for projectM: a soft cut blends, a hard cut does not.
    pub smooth: bool,
}

/// The plan's preset schedule plus the user's overrides.
///
/// The schedule is the render's: a cursor walks the pool, each switch takes
/// the pool entry under the cursor, and a preset projectM cannot load is
/// skipped for good with the cursor moving on (`loop.rs` `Playlist::advance`).
/// Following the cursor, not `preset_sequence`, keeps every later preset the
/// same as the render's after a skip. Two things preview cannot know and the
/// render does: a skip it never loads (switches passed while locked, or
/// stepped over by a stall) and a preset the render sets aside as black.
///
/// A scheduled switch always lands on the schedule's preset, so stepping off
/// lasts until the next switch. Locking freezes the display: switches pass
/// (the cursor still moves) but are not shown, and unlocking does not jump.
#[derive(Debug, Clone)]
pub struct PreviewState {
    pool: Vec<PathBuf>,
    /// First frame of each segment after the first, exactly as the render computes them.
    switch_at: Vec<u64>,
    smooth: Vec<bool>,
    /// Segment the schedule is in at the latest frame seen.
    segment: usize,
    /// Pool index the next scheduled load takes (before skipping unloadable ones).
    cursor: usize,
    /// Pool index the schedule shows now.
    planned: usize,
    /// Pool index on screen.
    shown: usize,
    locked: bool,
    unloadable: Vec<bool>,
    notice: Option<String>,
    last_smooth: bool,
    /// Direction of the latest manual step, for skipping past one that will not load.
    forward: bool,
}

impl PreviewState {
    pub fn from_plan(plan: &RenderPlan) -> Result<PreviewState> {
        let n = plan.pool.len();
        if n == 0 || plan.preset_sequence.is_empty() {
            return Err(Error::msg("the plan has no presets to preview"));
        }
        // The cursor reproduces the sequence only if the plan is the pool walked in order.
        if let Some(i) = plan.preset_sequence.iter().enumerate().position(|(i, p)| *p != plan.pool[i % n]) {
            return Err(Error::msg(format!(
                "the plan's preset {i} is not the pool's entry {}; preview cannot reproduce it",
                i % n
            )));
        }
        let mut s = PreviewState {
            pool: plan.pool.clone(),
            switch_at: switch_frames(plan),
            smooth: plan.switches.iter().map(|s| s.cut == Cut::Soft).collect(),
            segment: 0,
            cursor: 0,
            planned: 0,
            shown: 0,
            locked: false,
            unloadable: vec![false; n],
            notice: None,
            last_smooth: false,
            forward: true,
        };
        s.planned = s.pick().expect("a fresh pool has no unloadable entry");
        s.shown = s.planned;
        Ok(s)
    }

    /// The pool entry under the cursor, passing over unloadable ones. Moves the cursor.
    fn pick(&mut self) -> Option<usize> {
        let n = self.pool.len();
        for _ in 0..n {
            let i = self.cursor % n;
            self.cursor += 1;
            if !self.unloadable[i] {
                return Some(i);
            }
        }
        None
    }

    pub fn current(&self) -> &Path {
        &self.pool[self.shown]
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// True when what is on screen is not what the render will show now.
    pub fn off_schedule(&self) -> bool {
        self.shown != self.planned
    }

    /// Call once per drawn frame with its index. Returns the preset to load
    /// when the schedule has entered a new segment. If several switches went
    /// by (frames were dropped), the cursor passes each but only the one now
    /// due is returned. A frame earlier than one already seen is ignored.
    pub fn on_frame(&mut self, frame: u64) -> Option<Load> {
        // A frame before the segment already reached lands in `segment <= self.segment`.
        let segment = self.switch_at.partition_point(|&f| f <= frame);
        if segment <= self.segment {
            return None;
        }
        for _ in self.segment..segment {
            if let Some(i) = self.pick() {
                self.planned = i;
            }
        }
        self.last_smooth = self.smooth.get(segment - 1).copied().unwrap_or(false);
        let passed = segment - self.segment;
        self.segment = segment;
        if self.locked {
            self.notice = Some("switch passed while locked; a preset the render would skip cannot be seen from here, so later presets may differ".into());
            return None;
        }
        if passed > 1 {
            self.notice = Some(format!("dropped frames passed {passed} switches without loading them; a preset the render would skip cannot be seen from here, so later presets may differ"));
        }
        self.shown = self.planned;
        Some(Load { path: self.pool[self.shown].clone(), smooth: self.last_smooth })
    }

    /// A warning that preview may no longer match the render, if one arose
    /// since the last call (a switch passed unloaded). Taken once.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// projectM could not load what [`current`](Self::current) names. Sets it
    /// aside for good and returns the preset to try instead: the cursor's next
    /// if it was the scheduled one, else the next in the stepping direction.
    /// `None` when nothing is left.
    pub fn load_failed(&mut self) -> Option<Load> {
        self.unloadable[self.shown] = true;
        let smooth = if self.shown == self.planned {
            self.planned = self.pick()?;
            self.shown = self.planned;
            self.last_smooth
        } else {
            self.step(self.forward)?;
            true
        };
        Some(Load { path: self.pool[self.shown].clone(), smooth })
    }

    fn step(&mut self, forward: bool) -> Option<()> {
        let n = self.pool.len();
        self.forward = forward;
        for _ in 0..n {
            self.shown = if forward { (self.shown + 1) % n } else { (self.shown + n - 1) % n };
            if !self.unloadable[self.shown] {
                return Some(());
            }
        }
        None
    }

    fn manual(&mut self, forward: bool) -> Load {
        let _ = self.step(forward);
        Load { path: self.pool[self.shown].clone(), smooth: true }
    }

    /// The pool's next loadable preset, leaving the schedule.
    pub fn next(&mut self) -> Load {
        self.manual(true)
    }

    /// The pool's previous loadable preset, leaving the schedule.
    pub fn previous(&mut self) -> Load {
        self.manual(false)
    }

    pub fn lock(&mut self) {
        self.locked = true;
    }

    pub fn unlock(&mut self) {
        self.locked = false;
    }

    pub fn toggle_lock(&mut self) {
        self.locked = !self.locked;
    }
}

/// What a key press asks the window to do. [`key_action`] decides; the window
/// carries it out.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    Quit,
    Load(Load),
    /// Print the current preset's path (so it can be pinned with `--preset`).
    Print(PathBuf),
    /// Lock changed; the value is the new state.
    Lock(bool),
    Ignored,
}

pub fn key_action(code: KeyCode, state: &mut PreviewState) -> KeyAction {
    match code {
        KeyCode::Escape => KeyAction::Quit,
        KeyCode::ArrowRight => KeyAction::Load(state.next()),
        KeyCode::ArrowLeft => KeyAction::Load(state.previous()),
        KeyCode::KeyL => {
            state.toggle_lock();
            KeyAction::Lock(state.is_locked())
        }
        KeyCode::KeyP => KeyAction::Print(state.current().to_owned()),
        _ => KeyAction::Ignored,
    }
}

// ---------------------------------------------------------------------------
// Which frame is due
// ---------------------------------------------------------------------------

/// A frame to draw and how many were passed over to reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due {
    pub frame: u64,
    pub dropped: u64,
}

/// Chooses the frame for an audio position and never goes back. A frame that
/// is already past is dropped, not drawn late.
#[derive(Debug, Clone)]
pub struct Pacer {
    fps: f64,
    /// First frame index not yet drawn or dropped.
    next: u64,
    rendered: u64,
    dropped: u64,
}

impl Pacer {
    pub fn new(fps: u32) -> Pacer {
        Pacer { fps: fps as f64, next: 0, rendered: 0, dropped: 0 }
    }

    /// The frame to draw for audio position `secs`, or `None` if that frame
    /// (or a later one) has been handled already.
    pub fn due(&mut self, secs: f64) -> Option<Due> {
        if !secs.is_finite() || secs < 0.0 {
            return None;
        }
        // The epsilon keeps 123/60 * 60 from landing a hair under 123.
        let frame = (secs * self.fps + 1e-9).floor() as u64;
        // A saturated cast is a position no track has, and `frame + 1` would overflow.
        if frame == u64::MAX || frame < self.next {
            return None;
        }
        let dropped = frame - self.next;
        self.next = frame + 1;
        self.rendered += 1;
        self.dropped += dropped;
        Some(Due { frame, dropped })
    }

    pub fn rendered(&self) -> u64 {
        self.rendered
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Seconds until the next undrawn frame begins, from audio position `secs`.
    fn until_next(&self, secs: f64) -> f64 {
        (self.next as f64 / self.fps - secs).max(0.0)
    }
}

// ---------------------------------------------------------------------------
// Audio: the clock and the callback
// ---------------------------------------------------------------------------

/// Where the listener is in the track: what the callback has written, minus
/// the time before the last sample written is heard (`lead`), plus the time
/// since that callback ran. Never ahead of what was written, never negative.
pub fn heard_secs(frames_written: u64, rate: u32, lead_secs: f64, since_callback_secs: f64) -> f64 {
    let still_to_hear = (lead_secs - since_callback_secs).max(0.0);
    (frames_written as f64 / rate as f64 - still_to_hear).max(0.0)
}

/// Fills one output buffer from interleaved stereo `src`, reading `step`
/// source frames per output frame starting at `*pos` (a step other than 1.0
/// resamples, linearly). Past the end of `src` it writes silence and keeps
/// counting, so the clock keeps running. Channels beyond the second are
/// silent; a mono device gets the left channel.
pub fn fill_output(src: &[f32], pos: &mut f64, step: f64, out: &mut [f32], channels: usize) {
    let frames = src.len() / 2;
    let at = |k: usize, c: usize| if k < frames { src[k * 2 + c] } else { 0.0 };
    for frame in out.chunks_exact_mut(channels.max(1)) {
        let floor = pos.floor();
        let frac = (*pos - floor) as f32;
        let i = floor as usize;
        for (c, d) in frame.iter_mut().enumerate() {
            *d = if c < 2 { at(i, c) * (1.0 - frac) + at(i + 1, c) * frac } else { 0.0 };
        }
        *pos += step;
    }
}

/// Source rate of the normalised wav.
const SOURCE_RATE: u32 = crate::audio::normalise::PIPELINE_RATE;

struct ClockSample {
    frames_written: u64,
    at: std::time::Instant,
    lead_secs: f64,
}

/// The output stream and the clock it keeps.
struct AudioOut {
    _stream: cpal::Stream,
    clock: std::sync::Arc<std::sync::Mutex<ClockSample>>,
}

impl AudioOut {
    fn start(samples: std::sync::Arc<Vec<f32>>) -> Result<AudioOut> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let fail = |what: &str, e: &dyn std::fmt::Display| Error::msg(format!("audio output: {what}: {e}"));
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| Error::msg("audio output: no default output device"))?;
        // Prefer the source rate, so no resampling; else whatever the device runs at.
        let wanted = cpal::SampleRate(SOURCE_RATE);
        let config = device
            .supported_output_configs()
            .map_err(|e| fail("configs", &e))?
            .find(|c| {
                c.sample_format() == cpal::SampleFormat::F32
                    && c.min_sample_rate() <= wanted
                    && wanted <= c.max_sample_rate()
            })
            .map(|c| c.with_sample_rate(wanted))
            .map_or_else(|| device.default_output_config().map_err(|e| fail("default config", &e)), Ok)?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            return Err(Error::msg(format!("audio output: device wants {:?}, only f32 is supported", config.sample_format())));
        }
        let channels = config.channels() as usize;
        let device_rate = config.sample_rate().0;
        let step = SOURCE_RATE as f64 / device_rate as f64;

        let clock = std::sync::Arc::new(std::sync::Mutex::new(ClockSample {
            frames_written: 0,
            at: std::time::Instant::now(),
            lead_secs: 0.0,
        }));
        let shared = clock.clone();
        let mut pos = 0.0f64;
        let stream = device
            .build_output_stream(
                &config.config(),
                move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
                    // No allocation, no GL, no waiting.
                    fill_output(&samples, &mut pos, step, out, channels);
                    let ts = info.timestamp();
                    let first = ts.playback.duration_since(&ts.callback).map_or(0.0, |d| d.as_secs_f64());
                    let buffer = (out.len() / channels.max(1)) as f64 / device_rate as f64;
                    // If the render thread holds the lock this once, it just
                    // keeps extrapolating from the previous sample.
                    if let Ok(mut c) = shared.try_lock() {
                        *c = ClockSample {
                            frames_written: pos as u64,
                            at: std::time::Instant::now(),
                            lead_secs: first + buffer,
                        };
                    }
                },
                |e| eprintln!("preview: audio stream error: {e}"),
                None,
            )
            .map_err(|e| fail("open stream", &e))?;
        stream.play().map_err(|e| fail("play", &e))?;
        Ok(AudioOut { _stream: stream, clock })
    }

    /// Seconds into the track at the listener's ear.
    fn heard(&self) -> f64 {
        let c = self.clock.lock().unwrap_or_else(|p| p.into_inner());
        heard_secs(c.frames_written, SOURCE_RATE, c.lead_secs, c.at.elapsed().as_secs_f64())
    }
}

fn decode_wav(wav: &Path) -> Result<Vec<f32>> {
    let mut pcm = Vec::new();
    deck_media_codec::visit_stereo(wav, |f| {
        pcm.extend_from_slice(f);
        Ok(())
    })
    .map_err(|e| Error::msg(format!("cannot decode {}: {e}", wav.display())))?;
    Ok(pcm)
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextApi, ContextAttributesBuilder, GlProfile, PossiblyCurrentContext, Version};
use glutin::display::{Display, DisplayApiPreference};
use glutin::prelude::*;
use glutin::surface::{Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use std::num::NonZeroU32;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowId};

/// The window's GL context and surface, as one thing projectM can re-make current.
struct WindowGl {
    // The surface goes first so it drops before the context.
    surface: Surface<WindowSurface>,
    ctx: PossiblyCurrentContext,
}

impl MakeCurrent for WindowGl {
    fn make_current(&self) -> Result<()> {
        self.ctx.make_current(&self.surface).map_err(|e| Error::msg(format!("make the preview context current: {e}")))
    }
}

fn gl_for(window: &Window) -> Result<WindowGl> {
    let gl_err = |what: &str, e: &dyn std::fmt::Display| Error::msg(format!("preview GL: {what}: {e}"));
    let display_handle = window.display_handle().map_err(|e| gl_err("display handle", &e))?.as_raw();
    let window_handle = window.window_handle().map_err(|e| gl_err("window handle", &e))?.as_raw();
    // Safety: the handles come from a live window that outlives the display, surface and context.
    let display = unsafe { Display::new(display_handle, DisplayApiPreference::Cgl) }.map_err(|e| gl_err("display", &e))?;
    let template = ConfigTemplateBuilder::new().with_alpha_size(8).with_depth_size(24).build();
    let config = unsafe { display.find_configs(template) }
        .map_err(|e| gl_err("configs", &e))?
        .next()
        .ok_or_else(|| Error::msg("preview GL: no framebuffer configuration"))?;
    // The same API level the headless context uses.
    let attrs = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 2))))
        .with_profile(GlProfile::Core)
        .build(Some(window_handle));
    let not_current = unsafe { display.create_context(&config, &attrs) }.map_err(|e| gl_err("create context", &e))?;
    let size = window.inner_size();
    let nz = |v: u32| NonZeroU32::new(v.max(1)).expect("max(1) is non-zero");
    let surf = SurfaceAttributesBuilder::<WindowSurface>::new().build(window_handle, nz(size.width), nz(size.height));
    let surface = unsafe { display.create_window_surface(&config, &surf) }.map_err(|e| gl_err("create surface", &e))?;
    let ctx = not_current.make_current(&surface).map_err(|e| gl_err("make current", &e))?;
    surface
        .set_swap_interval(&ctx, SwapInterval::Wait(NonZeroU32::MIN))
        .map_err(|e| gl_err("swap interval", &e))?;
    Ok(WindowGl { surface, ctx })
}

/// What a run measured.
#[derive(Debug, Clone, Default)]
pub struct PreviewStats {
    pub rendered: u64,
    pub dropped: u64,
    /// Audio seconds covered, up to the end of the last frame drawn.
    pub seconds: f64,
    /// Time inside the draw (projectM + swap), summed and worst.
    pub draw_total: Duration,
    pub draw_worst: Duration,
    /// Of `draw_total`, the part inside projectM.
    pub render_total: Duration,
}

impl PreviewStats {
    pub fn fps(&self) -> f64 {
        if self.seconds > 0.0 { self.rendered as f64 / self.seconds } else { 0.0 }
    }
}

struct App<'a> {
    window: &'a Window,
    gl: &'a WindowGl,
    pm: &'a Instance<'a>,
    audio: &'a AudioOut,
    driver: &'a crate::audio::Driver,
    overlay: Overlay,
    gl_overlay: GlOverlay,
    /// projectM draws into this at the overscanned size; its centre is blitted
    /// to the window, which is what `render` keeps. Rebuilt on resize.
    off: Offscreen,
    /// Limits luminance change before the overlay is drawn; `None` under `--no-flash-guard`.
    guard: Option<FlashGuard>,
    guard_buf: Vec<u8>,
    /// Time inside the guard (readback, limit, upload), summed.
    guard_total: Duration,
    /// Bass level per frame, as the render computes it for the watermark pulse.
    bass: Vec<f32>,
    size: (u32, u32),
    state: PreviewState,
    pacer: Pacer,
    fps: u32,
    end_secs: f64,
    /// Source frames already handed to projectM.
    fed: usize,
    last_frame: u64,
    stats: PreviewStats,
    last_title: Instant,
    error: Option<Error>,
}

impl App<'_> {
    /// Loads `load`; a preset projectM rejects is set aside and the state names the next, as the render does.
    fn load(&mut self, mut load: Load) {
        loop {
            match self.pm.load_preset_file(&load.path, load.smooth) {
                Ok(()) => return,
                Err(e) => {
                    eprintln!("preview: skipping {}: {e}", load.path.display());
                    match self.state.load_failed() {
                        Some(next) => load = next,
                        None => {
                            eprintln!("preview: no preset in the pool will load");
                            return;
                        }
                    }
                }
            }
        }
    }

    fn draw(&mut self, due: Due) -> Result<()> {
        let started = Instant::now();
        let load = self.state.on_frame(due.frame);
        if let Some(n) = self.state.take_notice() {
            eprintln!("preview: {n}");
        }
        if let Some(load) = load {
            self.load(load);
        }
        // The PCM since the last frame drawn, capped so a long stall does not dump a burst.
        let rate = self.driver.rate as f64;
        let end = (((due.frame + 1) as f64 * rate / self.fps as f64).round() as usize).max(self.fed);
        let start = self.fed.max(end.saturating_sub((rate / 10.0) as usize));
        if end > start {
            let window = self.driver.window((start as f64 + 0.5) / rate, end - start);
            self.pm.pcm_add_float(window, crate::ffi::Channels::Stereo);
        }
        self.fed = end;
        self.pm.set_frame_time(due.frame as f64 / self.fps as f64);
        let t0 = Instant::now();
        // Overscanned into our own target, then the centre onto the window, so
        // the preview shows the frame `render` would encode and not the 5% of
        // edge it crops. Everything after this point works on the window at
        // `self.size`, which is the render's order too: crop, guard, overlay.
        self.pm.render_frame_fbo(self.off.fbo());
        self.off.blit_centre(0, self.size.0, self.size.1)?;
        if let Some(g) = self.guard.as_mut() {
            let (w, h) = (self.size.0 as usize, self.size.1 as usize);
            // A window under 8 px a side holds no region WCAG could count.
            if w >= 8 && h >= 8 {
                let tg = Instant::now();
                g.resize(w, h);
                self.gl_overlay.filter_frame(&mut self.guard_buf, self.size.0, self.size.1, |px| {
                    g.limit(px);
                });
                self.guard_total += tg.elapsed();
            }
        }
        let bass = self.bass.get(due.frame as usize).or(self.bass.last()).copied().unwrap_or(0.0);
        self.gl_overlay.draw(&self.overlay, self.size.0, self.size.1, bass);
        self.stats.render_total += t0.elapsed();
        self.gl.surface.swap_buffers(&self.gl.ctx).map_err(|e| Error::msg(format!("swap buffers: {e}")))?;
        let took = started.elapsed();
        self.stats.draw_total += took;
        self.stats.draw_worst = self.stats.draw_worst.max(took);
        Ok(())
    }

    fn retitle(&mut self) {
        if self.last_title.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_title = Instant::now();
        let name = self.state.current().file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let flags = match (self.state.is_locked(), self.state.off_schedule()) {
            (true, _) => " [locked]",
            (false, true) => " [off schedule]",
            _ => "",
        };
        self.window.set_title(&format!(
            "{name}{flags} - {} dropped of {}",
            self.pacer.dropped(),
            self.pacer.rendered() + self.pacer.dropped()
        ));
    }

    fn key(&mut self, code: KeyCode, el: &ActiveEventLoop) {
        match key_action(code, &mut self.state) {
            KeyAction::Quit => el.exit(),
            KeyAction::Load(l) => self.load(l),
            KeyAction::Print(p) => println!("{}", p.display()),
            KeyAction::Lock(on) => eprintln!("preview: {}", if on { "locked" } else { "unlocked" }),
            KeyAction::Ignored => {}
        }
    }
}

impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
                    self.gl.surface.resize(&self.gl.ctx, w, h);
                }
                let (ow, oh) = overscanned(size.width, size.height);
                match Offscreen::new(ow, oh) {
                    Ok(off) => {
                        self.off = off;
                        self.pm.set_window_size(ow, oh);
                        self.size = (size.width, size.height);
                    }
                    // Keep drawing the old size into the new window: the frame
                    // then sits in a corner of it until a later resize works.
                    // The alternative is a blit `draw` refuses, and a refused
                    // draw ends the preview.
                    Err(e) => eprintln!("preview: cannot resize the render target to {ow}x{oh}: {e}"),
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !event.repeat => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.key(code, el);
                }
            }
            _ => {}
        }
    }

    /// One pass of the frame loop: where is the audio, is a new frame due, draw it, sleep until the next.
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let heard = self.audio.heard();
        if heard >= self.end_secs {
            el.exit();
            return;
        }
        if let Some(due) = self.pacer.due(heard) {
            if let Err(e) = self.draw(due) {
                self.error = Some(e);
                el.exit();
                return;
            }
            self.last_frame = due.frame;
            self.retitle();
        }
        let wait = self.pacer.until_next(self.audio.heard()).min(0.004);
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_secs_f64(wait)));
    }
}

/// Plays the plan `args` describes in a window, with sound, until the track
/// ends, the window closes or Esc is pressed.
pub fn preview(args: &RenderArgs) -> Result<()> {
    let prep = prepare(args)?;
    let plan = &prep.plan;
    let end_secs = args.limit_seconds.map_or(plan.duration, |l| l.min(plan.duration));
    let samples = std::sync::Arc::new(decode_wav(&prep.wav)?);

    let event_loop = EventLoop::new().map_err(|e| Error::msg(format!("event loop: {e}")))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    #[allow(deprecated)] // Instance borrows the GL context, so the window must exist before the loop.
    let window = event_loop
        .create_window(Window::default_attributes().with_title("projectmp4 preview").with_inner_size(LogicalSize::new(960.0, 540.0)))
        .map_err(|e| Error::msg(format!("create window: {e}")))?;
    let gl = gl_for(&window)?;
    let pm = Instance::new(&gl, proc_loader())?;
    // A window whose size is not yet known can report 0x0 (as `gl_for` and the
    // `Resized` handler also guard against).
    // Shadowed: every consumer below sees the clamped pair, and the raw one is gone.
    let raw = window.inner_size();
    let size = (raw.width.max(1), raw.height.max(1));
    let (sw, sh) = size;
    // projectM draws 5% wider and taller than the window; `App::draw` blits the
    // centre. The mesh is sized for the drawn surface, not the window.
    let (ow, oh) = overscanned(sw, sh);
    pm.set_window_size(ow, oh);
    pm.set_mesh_size(64, 48);
    pm.set_fps(plan.fps as i32);
    if let Some(t) = &plan.textures {
        pm.set_texture_search_paths(&[t.as_path()])?;
    }

    let state = PreviewState::from_plan(plan)?;
    let overlay = Overlay::new(&plan.title, plan.artist.as_deref(), plan.art.as_deref(), plan.watermark_style, !plan.static_overlay)?;
    let gl_overlay = GlOverlay::new(&overlay, sw, sh)?;
    let bass = bass_per_frame(&prep.driver, plan.fps, plan.total_frames);
    let first = Load { path: state.current().to_owned(), smooth: false };
    eprintln!(
        "preview: {} - {:.0} s, {} presets scheduled. Right/Left: step off the schedule. L: lock. P: print preset path. Esc: quit.",
        plan.title,
        end_secs,
        plan.preset_sequence.len()
    );

    let audio = AudioOut::start(samples)?;
    let mut app = App {
        window: &window,
        gl: &gl,
        pm: &pm,
        audio: &audio,
        driver: &prep.driver,
        overlay,
        gl_overlay,
        off: Offscreen::new(ow, oh)?,
        guard: plan.flash_guard.then(|| FlashGuard::new(sw.max(8) as usize, sh.max(8) as usize, plan.fps, plan.flash_rate)),
        guard_buf: Vec::new(),
        guard_total: Duration::ZERO,
        bass,
        size,
        state,
        pacer: Pacer::new(plan.fps),
        fps: plan.fps,
        end_secs,
        fed: 0,
        last_frame: 0,
        stats: PreviewStats::default(),
        last_title: Instant::now(),
        error: None,
    };
    app.load(first);
    let ran = event_loop.run_app(&mut app).map_err(|e| Error::msg(format!("event loop: {e}")));
    if gl.make_current().is_ok() {
        app.gl_overlay.delete();
    }
    ran?;
    if let Some(g) = &app.guard {
        let n = app.pacer.rendered().max(1);
        eprintln!(
            "preview: flash guard engaged on {}/{n} frames ({:.1}%), hardest alpha {:.3}, {:.1} ms per frame",
            g.engaged_frames(),
            100.0 * g.engaged_frames() as f64 / n as f64,
            g.min_alpha(),
            app.guard_total.as_secs_f64() * 1000.0 / n as f64
        );
    }
    if let Some(e) = app.error.take() {
        return Err(e);
    }
    let s = PreviewStats {
        rendered: app.pacer.rendered(),
        dropped: app.pacer.dropped(),
        seconds: (app.last_frame + 1) as f64 / plan.fps as f64,
        ..app.stats.clone()
    };
    eprintln!(
        "preview: drew {} frames, dropped {} ({:.1}%), {:.1} fps over {:.1} s; render avg {:.1} ms, draw avg {:.1} ms, worst {:.1} ms",
        s.rendered,
        s.dropped,
        100.0 * s.dropped as f64 / (s.rendered + s.dropped).max(1) as f64,
        s.fps(),
        s.seconds,
        s.render_total.as_secs_f64() * 1000.0 / s.rendered.max(1) as f64,
        s.draw_total.as_secs_f64() * 1000.0 / s.rendered.max(1) as f64,
        s.draw_worst.as_secs_f64() * 1000.0,
    );
    Ok(())
}
