//! Headless OpenGL context (CGL, macOS/Apple Silicon) plus the offscreen
//! framebuffer libprojectM renders into.
//!
//! No window, no drawable: `CGLCreateContext` with
//! `kCGLPFAOpenGLProfile = kCGLOGLPVersion_3_2_Core` yields a context that is
//! legal to make current and render with as long as every draw targets a
//! framebuffer object we own. Task 13's visible preview window is a separate
//! concern and will bring its own `winit`/`glutin` context.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

use crate::ffi::{Error, LoadProc, Result};

// ---------------------------------------------------------------------------
// CGL (OpenGL.framework)
// ---------------------------------------------------------------------------

type CGLPixelFormatObj = *mut c_void;
type CGLContextObj = *mut c_void;
type CGLError = i32;

// kCGLPFADoubleBuffer (5) is deliberately not requested: there is no drawable.
const K_CGL_PFA_COLOR_SIZE: i32 = 8;
const K_CGL_PFA_DEPTH_SIZE: i32 = 12;
const K_CGL_PFA_ACCELERATED: i32 = 73;
const K_CGL_PFA_OPENGL_PROFILE: i32 = 99;
const K_CGL_OGLP_VERSION_3_2_CORE: i32 = 0x3200;

unsafe extern "C" {
    fn CGLChoosePixelFormat(
        attribs: *const i32,
        pix: *mut CGLPixelFormatObj,
        npix: *mut i32,
    ) -> CGLError;
    fn CGLDestroyPixelFormat(pix: CGLPixelFormatObj) -> CGLError;
    fn CGLCreateContext(
        pix: CGLPixelFormatObj,
        share: CGLContextObj,
        ctx: *mut CGLContextObj,
    ) -> CGLError;
    fn CGLDestroyContext(ctx: CGLContextObj) -> CGLError;
    fn CGLSetCurrentContext(ctx: CGLContextObj) -> CGLError;
    fn CGLGetCurrentContext() -> CGLContextObj;
    fn CGLErrorString(error: CGLError) -> *const c_char;
}

fn cgl_err(what: &str, e: CGLError) -> Error {
    let detail = if e == 0 {
        "kCGLNoError".to_string()
    } else {
        unsafe { CStr::from_ptr(CGLErrorString(e)) }
            .to_string_lossy()
            .into_owned()
    };
    Error::msg(format!("{what} failed: {detail} ({e})"))
}

fn cgl_check(what: &str, e: CGLError) -> Result<()> {
    if e == 0 {
        Ok(())
    } else {
        Err(cgl_err(what, e))
    }
}

// ---------------------------------------------------------------------------
// dlsym-based GL proc loader
// ---------------------------------------------------------------------------

const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;
const OPENGL_FRAMEWORK: &[u8] = b"/System/Library/Frameworks/OpenGL.framework/OpenGL\0";

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Handle to OpenGL.framework, opened once per process. `usize` because
/// `*mut c_void` is not `Send`/`Sync` and `OnceLock` requires both.
fn opengl_framework() -> *mut c_void {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    *HANDLE.get_or_init(|| unsafe {
        dlopen(
            OPENGL_FRAMEWORK.as_ptr() as *const c_char,
            RTLD_LAZY | RTLD_LOCAL,
        ) as usize
    }) as *mut c_void
}

/// `ffi::LoadProc` handed to `projectm_create_with_opengl_load_proc`.
///
/// Returning null is not an error: projectM's `GLResolver` treats the user
/// resolver as a first-chance hook and falls back to its own native CGL
/// resolution. The body is wrapped in `catch_unwind` because unwinding out of
/// an `extern "C"` fn aborts the process.
unsafe extern "C" fn gl_load_proc(name: *const c_char, _user: *mut c_void) -> *mut c_void {
    std::panic::catch_unwind(|| {
        let handle = opengl_framework();
        if handle.is_null() || name.is_null() {
            return std::ptr::null_mut();
        }
        unsafe { dlsym(handle, name) }
    })
    .unwrap_or(std::ptr::null_mut())
}

// ---------------------------------------------------------------------------
// The slice of OpenGL we need ourselves
// ---------------------------------------------------------------------------

#[allow(non_camel_case_types)]
mod gl {
    use std::ffi::c_void;

    pub type GLenum = u32;
    pub type GLuint = u32;
    pub type GLint = i32;
    pub type GLsizei = i32;
    pub type GLbitfield = u32;
    pub type GLfloat = f32;

    pub const FRAMEBUFFER: GLenum = 0x8D40;
    pub const READ_FRAMEBUFFER: GLenum = 0x8CA8;
    pub const RENDERBUFFER: GLenum = 0x8D41;
    pub const TEXTURE_2D: GLenum = 0x0DE1;
    pub const RGBA8: GLint = 0x8058;
    pub const RGBA: GLenum = 0x1908;
    pub const UNSIGNED_BYTE: GLenum = 0x1401;
    pub const COLOR_ATTACHMENT0: GLenum = 0x8CE0;
    pub const DEPTH_ATTACHMENT: GLenum = 0x8D00;
    pub const DEPTH_COMPONENT24: GLenum = 0x81A6;
    pub const FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
    pub const TEXTURE_MIN_FILTER: GLenum = 0x2801;
    pub const TEXTURE_MAG_FILTER: GLenum = 0x2800;
    pub const TEXTURE_WRAP_S: GLenum = 0x2802;
    pub const TEXTURE_WRAP_T: GLenum = 0x2803;
    pub const LINEAR: GLint = 0x2601;
    pub const CLAMP_TO_EDGE: GLint = 0x812F;
    pub const PACK_ALIGNMENT: GLenum = 0x0D05;
    pub const COLOR_BUFFER_BIT: GLbitfield = 0x4000;
    pub const DEPTH_BUFFER_BIT: GLbitfield = 0x0100;
    pub const NO_ERROR: GLenum = 0;
    pub const VERSION: GLenum = 0x1F02;
    pub const RENDERER: GLenum = 0x1F01;

    unsafe extern "C" {
        #[link_name = "glGenFramebuffers"]
        pub fn gen_framebuffers(n: GLsizei, ids: *mut GLuint);
        #[link_name = "glDeleteFramebuffers"]
        pub fn delete_framebuffers(n: GLsizei, ids: *const GLuint);
        #[link_name = "glBindFramebuffer"]
        pub fn bind_framebuffer(target: GLenum, fb: GLuint);
        #[link_name = "glCheckFramebufferStatus"]
        pub fn check_framebuffer_status(target: GLenum) -> GLenum;
        #[link_name = "glGenTextures"]
        pub fn gen_textures(n: GLsizei, ids: *mut GLuint);
        #[link_name = "glDeleteTextures"]
        pub fn delete_textures(n: GLsizei, ids: *const GLuint);
        #[link_name = "glBindTexture"]
        pub fn bind_texture(target: GLenum, tex: GLuint);
        #[link_name = "glTexImage2D"]
        #[allow(clippy::too_many_arguments)]
        pub fn tex_image_2d(
            target: GLenum,
            level: GLint,
            internal_format: GLint,
            width: GLsizei,
            height: GLsizei,
            border: GLint,
            format: GLenum,
            ty: GLenum,
            pixels: *const c_void,
        );
        #[link_name = "glTexParameteri"]
        pub fn tex_parameter_i(target: GLenum, pname: GLenum, param: GLint);
        #[link_name = "glFramebufferTexture2D"]
        pub fn framebuffer_texture_2d(
            target: GLenum,
            attachment: GLenum,
            tex_target: GLenum,
            tex: GLuint,
            level: GLint,
        );
        #[link_name = "glGenRenderbuffers"]
        pub fn gen_renderbuffers(n: GLsizei, ids: *mut GLuint);
        #[link_name = "glDeleteRenderbuffers"]
        pub fn delete_renderbuffers(n: GLsizei, ids: *const GLuint);
        #[link_name = "glBindRenderbuffer"]
        pub fn bind_renderbuffer(target: GLenum, rb: GLuint);
        #[link_name = "glRenderbufferStorage"]
        pub fn renderbuffer_storage(
            target: GLenum,
            internal_format: GLenum,
            width: GLsizei,
            height: GLsizei,
        );
        #[link_name = "glFramebufferRenderbuffer"]
        pub fn framebuffer_renderbuffer(
            target: GLenum,
            attachment: GLenum,
            rb_target: GLenum,
            rb: GLuint,
        );
        #[link_name = "glReadPixels"]
        pub fn read_pixels(
            x: GLint,
            y: GLint,
            width: GLsizei,
            height: GLsizei,
            format: GLenum,
            ty: GLenum,
            pixels: *mut c_void,
        );
        #[link_name = "glPixelStorei"]
        pub fn pixel_store_i(pname: GLenum, param: GLint);
        #[link_name = "glViewport"]
        pub fn viewport(x: GLint, y: GLint, width: GLsizei, height: GLsizei);
        #[link_name = "glClearColor"]
        pub fn clear_color(r: GLfloat, g: GLfloat, b: GLfloat, a: GLfloat);
        #[link_name = "glClear"]
        pub fn clear(mask: GLbitfield);
        #[link_name = "glGetError"]
        pub fn get_error() -> GLenum;
        #[link_name = "glGetString"]
        pub fn get_string(name: GLenum) -> *const u8;
    }
}

fn gl_check(what: &str) -> Result<()> {
    let e = unsafe { gl::get_error() };
    if e == gl::NO_ERROR {
        Ok(())
    } else {
        Err(Error::msg(format!("{what}: GL error 0x{e:04X}")))
    }
}

fn gl_string(name: gl::GLenum) -> String {
    let p = unsafe { gl::get_string(name) };
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p as *const c_char) }
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// GlContext
// ---------------------------------------------------------------------------

/// A headless CGL context owning one offscreen RGBA8 + depth24 framebuffer.
///
/// Invariants:
/// - `new` leaves the context current on the calling thread. Everything that
///   touches GL — including `ffi::Instance::new`, every render call, and
///   `Instance`'s `Drop` — must happen on that thread with this context
///   current. Call [`GlContext::make_current`] if anything may have changed it.
/// - Drop order matters: an `ffi::Instance` created against this context must
///   be dropped *before* the `GlContext`, or `projectm_destroy` deletes GL
///   objects with no current context. Declaring the `GlContext` first in a
///   scope gives this for free, since Rust drops locals in reverse order.
/// - Holds raw pointers and relies on thread-local GL currency, so it is
///   `!Send` and `!Sync`.
pub struct GlContext {
    ctx: CGLContextObj,
    width: u32,
    height: u32,
    fbo: gl::GLuint,
    color: gl::GLuint,
    depth: gl::GLuint,
}

impl GlContext {
    /// Creates an accelerated OpenGL 3.2+ core-profile context with no
    /// drawable, makes it current, and allocates the render target.
    pub fn new(width: u32, height: u32) -> Result<GlContext> {
        if width == 0 || height == 0 {
            return Err(Error::msg("GlContext::new requires a non-empty size"));
        }

        // kCGLPFADoubleBuffer is deliberately absent: there is no drawable to
        // swap. Colour/depth sizes describe the (unused) default framebuffer;
        // they are requested so a renderer that cannot do 8-bit RGBA + 24-bit
        // depth is rejected here rather than at FBO-completeness time.
        let attribs: [i32; 8] = [
            K_CGL_PFA_OPENGL_PROFILE,
            K_CGL_OGLP_VERSION_3_2_CORE,
            K_CGL_PFA_ACCELERATED,
            K_CGL_PFA_COLOR_SIZE,
            32,
            K_CGL_PFA_DEPTH_SIZE,
            24,
            0,
        ];

        let mut pix: CGLPixelFormatObj = std::ptr::null_mut();
        let mut npix: i32 = 0;
        cgl_check("CGLChoosePixelFormat", unsafe {
            CGLChoosePixelFormat(attribs.as_ptr(), &mut pix, &mut npix)
        })?;
        if pix.is_null() || npix == 0 {
            return Err(Error::msg(
                "CGLChoosePixelFormat matched no accelerated OpenGL 3.2 core renderer",
            ));
        }

        let mut ctx: CGLContextObj = std::ptr::null_mut();
        let create = unsafe { CGLCreateContext(pix, std::ptr::null_mut(), &mut ctx) };
        unsafe { CGLDestroyPixelFormat(pix) };
        cgl_check("CGLCreateContext", create)?;
        if ctx.is_null() {
            return Err(Error::msg("CGLCreateContext returned a null context"));
        }

        if let Err(e) = cgl_check("CGLSetCurrentContext", unsafe { CGLSetCurrentContext(ctx) }) {
            unsafe { CGLDestroyContext(ctx) };
            return Err(e);
        }

        let mut this = GlContext {
            ctx,
            width,
            height,
            fbo: 0,
            color: 0,
            depth: 0,
        };
        // On failure `this` is dropped here, and `Drop` releases the context
        // and whatever GL objects were created before the error.
        this.build_framebuffer()?;
        Ok(this)
    }

    fn build_framebuffer(&mut self) -> Result<()> {
        let w = self.width as gl::GLsizei;
        let h = self.height as gl::GLsizei;
        unsafe {
            // Drain any error the context creation path may have left behind.
            let _ = gl::get_error();

            gl::gen_framebuffers(1, &mut self.fbo);
            gl::bind_framebuffer(gl::FRAMEBUFFER, self.fbo);

            gl::gen_textures(1, &mut self.color);
            gl::bind_texture(gl::TEXTURE_2D, self.color);
            gl::tex_image_2d(
                gl::TEXTURE_2D,
                0,
                gl::RGBA8,
                w,
                h,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gl::tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR);
            gl::tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR);
            gl::tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE);
            gl::tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE);
            gl::framebuffer_texture_2d(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                self.color,
                0,
            );

            gl::gen_renderbuffers(1, &mut self.depth);
            gl::bind_renderbuffer(gl::RENDERBUFFER, self.depth);
            gl::renderbuffer_storage(gl::RENDERBUFFER, gl::DEPTH_COMPONENT24, w, h);
            gl::framebuffer_renderbuffer(
                gl::FRAMEBUFFER,
                gl::DEPTH_ATTACHMENT,
                gl::RENDERBUFFER,
                self.depth,
            );

            gl_check("allocating the offscreen framebuffer")?;

            let status = gl::check_framebuffer_status(gl::FRAMEBUFFER);
            if status != gl::FRAMEBUFFER_COMPLETE {
                return Err(Error::msg(format!(
                    "framebuffer incomplete: glCheckFramebufferStatus = 0x{status:04X} \
                     ({}x{} RGBA8 + depth24)",
                    self.width, self.height
                )));
            }

            gl::viewport(0, 0, w, h);
            gl::clear_color(0.0, 0.0, 0.0, 1.0);
            gl::clear(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT);
            gl_check("clearing the offscreen framebuffer")?;
        }
        Ok(())
    }

    /// Fails loudly if this context is not current on the calling thread,
    /// rather than letting GL calls land on someone else's context (or none)
    /// and return silent garbage.
    fn ensure_current(&self) -> Result<()> {
        if unsafe { CGLGetCurrentContext() } == self.ctx {
            Ok(())
        } else {
            Err(Error::msg(
                "this GlContext is not current on the calling thread; \
                 call GlContext::make_current first",
            ))
        }
    }

    /// Makes this context current on the calling thread.
    pub fn make_current(&self) -> Result<()> {
        cgl_check("CGLSetCurrentContext", unsafe {
            CGLSetCurrentContext(self.ctx)
        })
    }

    /// The GL name of the offscreen framebuffer to pass to
    /// `ffi::Instance::render_frame_fbo`.
    pub fn fbo(&self) -> u32 {
        self.fbo
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The GL proc loader to hand to `ffi::Instance::new`.
    pub fn loader(&self) -> LoadProc {
        gl_load_proc
    }

    /// `GL_VERSION` and `GL_RENDERER` of the live context, for diagnostics.
    pub fn renderer_info(&self) -> (String, String) {
        (gl_string(gl::VERSION), gl_string(gl::RENDERER))
    }

    /// Reads the colour attachment back as tightly packed RGBA8.
    ///
    /// Rows come back **bottom-up**: `glReadPixels` origin is the lower-left
    /// corner, so row 0 of the returned buffer is the bottom scanline of the
    /// image. The encoder (Task 12) must either flip or tell the sink.
    pub fn read_rgba(&self) -> Result<Vec<u8>> {
        self.ensure_current()?;
        let len = self.width as usize * self.height as usize * 4;
        let mut buf = vec![0u8; len];
        unsafe {
            let _ = gl::get_error();
            gl::bind_framebuffer(gl::READ_FRAMEBUFFER, self.fbo);
            gl::pixel_store_i(gl::PACK_ALIGNMENT, 1);
            gl::read_pixels(
                0,
                0,
                self.width as gl::GLsizei,
                self.height as gl::GLsizei,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                buf.as_mut_ptr() as *mut c_void,
            );
        }
        gl_check("glReadPixels from the offscreen framebuffer")?;
        Ok(buf)
    }

    fn destroy(&mut self) {
        unsafe {
            if !self.ctx.is_null() {
                let _ = CGLSetCurrentContext(self.ctx);
                if self.depth != 0 {
                    gl::delete_renderbuffers(1, &self.depth);
                }
                if self.color != 0 {
                    gl::delete_textures(1, &self.color);
                }
                if self.fbo != 0 {
                    gl::delete_framebuffers(1, &self.fbo);
                }
                let _ = CGLSetCurrentContext(std::ptr::null_mut());
                CGLDestroyContext(self.ctx);
            }
        }
        self.depth = 0;
        self.color = 0;
        self.fbo = 0;
        self.ctx = std::ptr::null_mut();
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        self.destroy();
    }
}
