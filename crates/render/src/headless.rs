//! A GLES 3 context with no window, for tests and offline tools.
//!
//! Wallpaper Engine shaders can only be judged by a driver, and a wallpaper
//! daemon should not need a monitor to test its renderer. Mesa's surfaceless EGL
//! platform provides exactly that: a real GLES 3 context where everything is
//! drawn into framebuffers, nothing reaches the screen, and the process still
//! behaves like a normal client. `fxcompile` and `fxrender` are built on it.
//!
//! Two details are easy to get wrong and are handled here once:
//!
//! - **`eglGetPlatformDisplayEXT`, not the core 1.5 entry point.** The core
//!   `eglGetPlatformDisplay` is not exported by every `libEGL` (glvnd answers
//!   `EGL_BAD_PARAMETER` here), while the `EXT` version from
//!   `EGL_EXT_platform_base` works everywhere the extension is advertised.
//! - **`EGL_KHR_surfaceless_context`** lets `make_current` succeed with no
//!   surface at all; where a driver lacks it, a 1x1 pbuffer is created so the
//!   context still works.

use anyhow::{Context, Result};
use glow::HasContext;
use std::sync::Arc;

/// `EGL_PLATFORM_SURFACELESS_MESA`. `khronos_egl::Enum` is a `c_uint`, so the
/// constant is passed through rather than looked up.
const EGL_PLATFORM_SURFACELESS_MESA: khronos_egl::Enum = 0x31DD;

type Egl = khronos_egl::DynamicInstance<khronos_egl::EGL1_4>;

/// `eglGetPlatformDisplayEXT`
type GetPlatformDisplayExt =
    unsafe extern "system" fn(u32, *mut std::ffi::c_void, *const i32) -> *mut std::ffi::c_void;

/// A headless GLES 3 context and the EGL objects that own it.
pub struct Headless {
    egl: Arc<Egl>,
    display: khronos_egl::Display,
    context: khronos_egl::Context,
    pbuffer: Option<khronos_egl::Surface>,
    /// The GL function table. Everything is drawn into framebuffers.
    pub gl: glow::Context,
}

impl Headless {
    /// Create the context, or explain why the driver cannot.
    pub fn new() -> Result<Self> {
        use khronos_egl as egl;
        let egl = unsafe { Egl::load_required() }.context("loading libEGL")?;

        let proc = egl
            .get_proc_address("eglGetPlatformDisplayEXT")
            .ok_or_else(|| anyhow::anyhow!("this libEGL has no eglGetPlatformDisplayEXT"))?;
        let get_platform_display: GetPlatformDisplayExt = unsafe { std::mem::transmute(proc) };
        let raw = unsafe {
            get_platform_display(
                EGL_PLATFORM_SURFACELESS_MESA,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if raw.is_null() {
            let code = egl.get_error().map(|e| e as u32).unwrap_or(0);
            anyhow::bail!("no surfaceless EGL display (EGL error {code:#x})");
        }
        let display = unsafe { egl::Display::from_ptr(raw) };
        egl.initialize(display).context("initialising EGL")?;
        egl.bind_api(egl::OPENGL_ES_API)
            .context("binding the ES API")?;

        let config_attributes = [
            egl::SURFACE_TYPE,
            egl::PBUFFER_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES3_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &config_attributes)
            .context("choosing an EGL config")?
            .ok_or_else(|| anyhow::anyhow!("no GLES 3 config available"))?;

        let context_attributes = [egl::CONTEXT_MAJOR_VERSION, 3, egl::NONE];
        let context = egl
            .create_context(display, config, None, &context_attributes)
            .context("creating a GLES 3 context")?;

        let mut pbuffer = None;
        if egl
            .make_current(display, None, None, Some(context))
            .is_err()
        {
            let surface = egl
                .create_pbuffer_surface(
                    display,
                    config,
                    &[egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE],
                )
                .context("no surfaceless context and no pbuffer")?;
            egl.make_current(display, Some(surface), Some(surface), Some(context))
                .context("making the pbuffer current")?;
            pbuffer = Some(surface);
        }

        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name)
                    .map(|p| p as *const std::ffi::c_void)
                    .unwrap_or(std::ptr::null())
            })
        };
        Ok(Headless {
            egl: egl.into(),
            display,
            context,
            pbuffer,
            gl,
        })
    }

    /// `"OpenGL ES 3.2 NVIDIA 610.57.04 | OpenGL ES GLSL ES 3.20"`
    pub fn version(&self) -> String {
        unsafe {
            format!(
                "{} | {}",
                self.gl.get_parameter_string(glow::VERSION),
                self.gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION)
            )
        }
    }

    /// Read a framebuffer's pixels as top-left-origin RGBA8.
    ///
    /// GL's origin is bottom-left, so the rows are flipped here: a caller
    /// comparing against an image or a screenshot should not have to know that.
    pub fn read_framebuffer(
        &self,
        framebuffer: glow::Framebuffer,
        width: i32,
        height: i32,
    ) -> Vec<u8> {
        let gl = &self.gl;
        let n = (width * height * 4) as usize;
        let mut buf = vec![0u8; n];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.read_pixels(
                0,
                0,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut buf)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        let row = (width * 4) as usize;
        let mut flipped = vec![0u8; n];
        for y in 0..height as usize {
            let src = (height as usize - 1 - y) * row;
            flipped[y * row..(y + 1) * row].copy_from_slice(&buf[src..src + row]);
        }
        flipped
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        if let Some(surface) = self.pbuffer {
            let _ = self.egl.destroy_surface(self.display, surface);
        }
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The harness has to work where it is meant to be used, and fail with a
    /// readable reason where it is not. On a machine with no EGL at all this
    /// asserts the error path rather than panicking.
    #[test]
    fn a_headless_context_is_obtainable_or_reports_why_not() {
        match Headless::new() {
            Ok(ctx) => {
                let v = ctx.version();
                assert!(v.contains("OpenGL ES"), "unexpected GL version: {v}");
            }
            Err(e) => {
                let msg = format!("{e:#}");
                assert!(
                    msg.contains("EGL") || msg.contains("libEGL") || msg.contains("surfaceless"),
                    "a failure must name the missing capability: {msg}"
                );
            }
        }
    }
}
