//! The overlay on the GPU, for the preview window. [`Overlay`] pre-rasterises
//! premultiplied tiles for the render's CPU composite; here the same tiles are
//! uploaded once as textures and drawn as blended quads over projectM's frame,
//! so a frame costs two draw calls and no readback. Same tiles, same anchors
//! ([`Overlay::block_layer`], [`Overlay::mark_layers`]), same bass-to-tile map
//! ([`Overlay::mark_index`]) as the render.

use crate::ffi::{Error, Result};
use crate::render::{Layer, Overlay};
use std::ffi::{c_char, c_void, CString};

#[allow(non_camel_case_types)]
mod gl {
    use std::ffi::{c_char, c_void};
    pub type GLenum = u32;
    pub type GLuint = u32;
    pub type GLint = i32;
    pub type GLsizei = i32;

    pub const TEXTURE_2D: GLenum = 0x0DE1;
    pub const TEXTURE0: GLenum = 0x84C0;
    pub const RGBA: GLenum = 0x1908;
    pub const RGBA8: GLint = 0x8058;
    pub const UNSIGNED_BYTE: GLenum = 0x1401;
    pub const TEXTURE_MIN_FILTER: GLenum = 0x2801;
    pub const TEXTURE_MAG_FILTER: GLenum = 0x2800;
    pub const TEXTURE_WRAP_S: GLenum = 0x2802;
    pub const TEXTURE_WRAP_T: GLenum = 0x2803;
    pub const NEAREST: GLint = 0x2600;
    pub const CLAMP_TO_EDGE: GLint = 0x812F;
    pub const BLEND: GLenum = 0x0BE2;
    pub const ONE: GLenum = 1;
    pub const ONE_MINUS_SRC_ALPHA: GLenum = 0x0303;
    pub const TRIANGLE_STRIP: GLenum = 5;
    pub const VERTEX_SHADER: GLenum = 0x8B31;
    pub const FRAGMENT_SHADER: GLenum = 0x8B30;
    pub const COMPILE_STATUS: GLenum = 0x8B81;
    pub const LINK_STATUS: GLenum = 0x8B82;

    unsafe extern "C" {
        #[link_name = "glCreateShader"] pub fn create_shader(ty: GLenum) -> GLuint;
        #[link_name = "glShaderSource"] pub fn shader_source(s: GLuint, n: GLsizei, src: *const *const c_char, len: *const GLint);
        #[link_name = "glCompileShader"] pub fn compile_shader(s: GLuint);
        #[link_name = "glGetShaderiv"] pub fn get_shaderiv(s: GLuint, p: GLenum, out: *mut GLint);
        #[link_name = "glGetShaderInfoLog"] pub fn get_shader_info_log(s: GLuint, max: GLsizei, len: *mut GLsizei, log: *mut c_char);
        #[link_name = "glDeleteShader"] pub fn delete_shader(s: GLuint);
        #[link_name = "glCreateProgram"] pub fn create_program() -> GLuint;
        #[link_name = "glAttachShader"] pub fn attach_shader(p: GLuint, s: GLuint);
        #[link_name = "glLinkProgram"] pub fn link_program(p: GLuint);
        #[link_name = "glGetProgramiv"] pub fn get_programiv(p: GLuint, n: GLenum, out: *mut GLint);
        #[link_name = "glGetProgramInfoLog"] pub fn get_program_info_log(p: GLuint, max: GLsizei, len: *mut GLsizei, log: *mut c_char);
        #[link_name = "glDeleteProgram"] pub fn delete_program(p: GLuint);
        #[link_name = "glUseProgram"] pub fn use_program(p: GLuint);
        #[link_name = "glGetUniformLocation"] pub fn get_uniform_location(p: GLuint, name: *const c_char) -> GLint;
        #[link_name = "glUniform4f"] pub fn uniform4f(loc: GLint, a: f32, b: f32, c: f32, d: f32);
        #[link_name = "glUniform2f"] pub fn uniform2f(loc: GLint, a: f32, b: f32);
        #[link_name = "glUniform1i"] pub fn uniform1i(loc: GLint, v: GLint);
        #[link_name = "glGenVertexArrays"] pub fn gen_vertex_arrays(n: GLsizei, ids: *mut GLuint);
        #[link_name = "glBindVertexArray"] pub fn bind_vertex_array(v: GLuint);
        #[link_name = "glDeleteVertexArrays"] pub fn delete_vertex_arrays(n: GLsizei, ids: *const GLuint);
        #[link_name = "glGenTextures"] pub fn gen_textures(n: GLsizei, ids: *mut GLuint);
        #[link_name = "glDeleteTextures"] pub fn delete_textures(n: GLsizei, ids: *const GLuint);
        #[link_name = "glBindTexture"] pub fn bind_texture(t: GLenum, id: GLuint);
        #[link_name = "glActiveTexture"] pub fn active_texture(t: GLenum);
        #[link_name = "glTexParameteri"] pub fn tex_parameteri(t: GLenum, p: GLenum, v: GLint);
        #[link_name = "glTexImage2D"]
        pub fn tex_image_2d(t: GLenum, level: GLint, internal: GLint, w: GLsizei, h: GLsizei, border: GLint, format: GLenum, ty: GLenum, px: *const c_void);
        #[link_name = "glEnable"] pub fn enable(cap: GLenum);
        #[link_name = "glDisable"] pub fn disable(cap: GLenum);
        #[link_name = "glBlendFunc"] pub fn blend_func(s: GLenum, d: GLenum);
        #[link_name = "glDrawArrays"] pub fn draw_arrays(mode: GLenum, first: GLint, count: GLsizei);
    }
}

// A quad from `gl_VertexID`, so there is no buffer: `rect` is x, y, w, h in
// pixels from the top-left, `view` the frame size.
const VERTEX: &str = "#version 150
uniform vec4 rect;
uniform vec2 view;
out vec2 uv;
void main() {
    vec2 c = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    uv = c;
    vec2 p = rect.xy + c * rect.zw;
    gl_Position = vec4(p.x / view.x * 2.0 - 1.0, 1.0 - p.y / view.y * 2.0, 0.0, 1.0);
}
";
const FRAGMENT: &str = "#version 150
uniform sampler2D tex;
in vec2 uv;
out vec4 color;
void main() { color = texture(tex, uv); }
";

fn compile(ty: gl::GLenum, src: &str) -> Result<gl::GLuint> {
    let c = CString::new(src).expect("shader source has no NUL");
    unsafe {
        let s = gl::create_shader(ty);
        gl::shader_source(s, 1, &c.as_ptr(), std::ptr::null());
        gl::compile_shader(s);
        let mut ok = 0;
        gl::get_shaderiv(s, gl::COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut log = vec![0u8; 1024];
            let mut n = 0;
            gl::get_shader_info_log(s, log.len() as i32, &mut n, log.as_mut_ptr() as *mut c_char);
            gl::delete_shader(s);
            return Err(Error::msg(format!("overlay shader: {}", String::from_utf8_lossy(&log[..n.max(0) as usize]))));
        }
        Ok(s)
    }
}

struct Tex {
    id: gl::GLuint,
}

fn upload(l: &Layer<'_>) -> Tex {
    let mut id = 0;
    unsafe {
        gl::gen_textures(1, &mut id);
        gl::active_texture(gl::TEXTURE0);
        gl::bind_texture(gl::TEXTURE_2D, id);
        // Drawn 1:1, so no filtering: a tile pixel is a frame pixel.
        gl::tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST);
        gl::tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST);
        gl::tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE);
        gl::tex_parameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE);
        gl::tex_image_2d(gl::TEXTURE_2D, 0, gl::RGBA8, l.w as i32, l.h as i32, 0, gl::RGBA, gl::UNSIGNED_BYTE, l.px.as_ptr() as *const c_void);
        gl::bind_texture(gl::TEXTURE_2D, 0);
    }
    Tex { id }
}

/// Uploaded overlay tiles and the program that draws them. Every method needs
/// the context it was created in to be current.
pub struct GlOverlay {
    program: gl::GLuint,
    vao: gl::GLuint,
    rect: gl::GLint,
    view: gl::GLint,
    block: Option<Tex>,
    marks: Vec<Tex>,
}

impl GlOverlay {
    /// Uploads `overlay`'s tiles as laid out for a `w`*`h` frame.
    pub fn new(overlay: &Overlay, w: u32, h: u32) -> Result<GlOverlay> {
        let (vs, fs) = (compile(gl::VERTEX_SHADER, VERTEX)?, compile(gl::FRAGMENT_SHADER, FRAGMENT)?);
        unsafe {
            let program = gl::create_program();
            gl::attach_shader(program, vs);
            gl::attach_shader(program, fs);
            gl::link_program(program);
            gl::delete_shader(vs);
            gl::delete_shader(fs);
            let mut ok = 0;
            gl::get_programiv(program, gl::LINK_STATUS, &mut ok);
            if ok == 0 {
                let mut log = vec![0u8; 1024];
                let mut n = 0;
                gl::get_program_info_log(program, log.len() as i32, &mut n, log.as_mut_ptr() as *mut c_char);
                gl::delete_program(program);
                return Err(Error::msg(format!("overlay program: {}", String::from_utf8_lossy(&log[..n.max(0) as usize]))));
            }
            let mut vao = 0;
            gl::gen_vertex_arrays(1, &mut vao);
            gl::use_program(program);
            gl::uniform1i(gl::get_uniform_location(program, c"tex".as_ptr()), 0);
            gl::use_program(0);
            Ok(GlOverlay {
                program,
                vao,
                rect: gl::get_uniform_location(program, c"rect".as_ptr()),
                view: gl::get_uniform_location(program, c"view".as_ptr()),
                block: overlay.block_layer(w, h).map(|l| upload(&l)),
                marks: overlay.mark_layers(w, h).iter().map(upload).collect(),
            })
        }
    }

    /// Draws the title block and the watermark tile `bass` selects onto the
    /// bound framebuffer, which is `w`*`h`. Leaves the GL state projectM uses as it found it.
    pub fn draw(&self, overlay: &Overlay, w: u32, h: u32, bass: f32) {
        let quad = |tex: &Tex, l: &Layer<'_>| unsafe {
            gl::uniform4f(self.rect, l.x as f32, l.y as f32, l.w as f32, l.h as f32);
            gl::bind_texture(gl::TEXTURE_2D, tex.id);
            gl::draw_arrays(gl::TRIANGLE_STRIP, 0, 4);
        };
        unsafe {
            gl::use_program(self.program);
            gl::bind_vertex_array(self.vao);
            gl::uniform2f(self.view, w as f32, h as f32);
            gl::active_texture(gl::TEXTURE0);
            gl::enable(gl::BLEND);
            // The tiles are premultiplied.
            gl::blend_func(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);
        }
        if let (Some(tex), Some(l)) = (&self.block, overlay.block_layer(w, h)) {
            quad(tex, &l);
        }
        let idx = overlay.mark_index(bass);
        if let Some(tex) = self.marks.get(idx) {
            quad(tex, &overlay.mark_layer(idx, w, h));
        }
        unsafe {
            gl::bind_texture(gl::TEXTURE_2D, 0);
            gl::disable(gl::BLEND);
            gl::bind_vertex_array(0);
            gl::use_program(0);
        }
    }

    /// Frees the GL objects; the context must be current.
    pub fn delete(&mut self) {
        unsafe {
            for t in self.block.iter().chain(&self.marks) {
                gl::delete_textures(1, &t.id);
            }
            gl::delete_vertex_arrays(1, &self.vao);
            gl::delete_program(self.program);
        }
        self.block = None;
        self.marks.clear();
    }
}
