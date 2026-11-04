//! Safe wrapper over the libprojectM 4.x C API (vendored, statically linked).

#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code)]
#[allow(clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use std::ffi::{c_char, c_void};

/// OpenGL symbol loader: given a NUL-terminated name, returns the function pointer.
pub type LoadProc = unsafe extern "C" fn(name: *const c_char, user: *mut c_void) -> *mut c_void;

#[derive(Debug)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
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

/// RAII owner of a `projectm_handle`. Requires a current OpenGL context at
/// creation and for every later call that renders.
pub struct Instance {
    handle: sys::projectm_handle,
}

impl Instance {
    pub fn new(load_proc: LoadProc) -> Result<Instance> {
        let handle = unsafe {
            sys::projectm_create_with_opengl_load_proc(Some(load_proc), std::ptr::null_mut())
        };
        if handle.is_null() {
            Err(Error("projectm_create_with_opengl_load_proc returned null"))
        } else {
            Ok(Instance { handle })
        }
    }

    #[allow(dead_code)]
    pub(crate) fn raw(&self) -> sys::projectm_handle {
        self.handle
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { sys::projectm_destroy(self.handle) };
    }
}
