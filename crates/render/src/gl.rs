//! EGL context and per-output GL surfaces.
//!
//! Only animated wallpapers need this. A still image goes through the shm path
//! in [`crate::image_layer`], which needs no GPU context at all and costs no CPU
//! once committed; dragging a GL context into that would be pure overhead.
//!
//! Video and, later, Wallpaper Engine scenes are a different matter: colour
//! conversion, scaling and shader effects belong on the GPU. mpv's own
//! documentation calls its software renderer "very slow" and single-threaded,
//! which at 2560x1440 and 4096x2560 would burn a core continuously — the
//! opposite of what this project exists for.

use anyhow::{Context, Result};
use khronos_egl as egl;
use std::sync::Arc;
use wayland_client::{protocol::wl_surface::WlSurface, Proxy};

type Egl = egl::DynamicInstance<egl::EGL1_4>;

/// Load libEGL at runtime rather than linking it, so a build on a machine
/// without EGL headers still produces a working binary for image wallpapers.
pub fn load_egl() -> Result<Arc<Egl>> {
    let lib = unsafe { Egl::load_required() }.context("loading libEGL")?;
    Ok(Arc::new(lib))
}

/// A GL surface bound to one output's layer surface.
pub struct GlSurface {
    /// Kept alive for as long as the EGL surface: destroying it first would
    /// leave EGL pointing at freed memory.
    window: wayland_egl::WlEglSurface,
    surface: egl::Surface,
    size: (i32, i32),
}

impl GlSurface {
    pub fn size(&self) -> (i32, i32) {
        self.size
    }

    pub fn resize(&mut self, width: i32, height: i32) {
        if self.size == (width, height) || width <= 0 || height <= 0 {
            return;
        }
        self.window.resize(width, height, 0, 0);
        self.size = (width, height);
    }

    pub fn raw(&self) -> egl::Surface {
        self.surface
    }

    pub fn wl_egl(&self) -> &wayland_egl::WlEglSurface {
        &self.window
    }
}

/// Everything needed to draw: the EGL bits plus the GL function loader.
pub struct Renderer {
    egl: Arc<Egl>,
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    pub gl: glow::Context,
}

impl Renderer {
    /// Bind EGL to the compositor's display and create a GLES 3 context.
    ///
    /// # Safety
    /// `display_ptr` must be a live `wl_display` belonging to a connection that
    /// outlives this renderer.
    pub unsafe fn new(display_ptr: *mut std::ffi::c_void) -> Result<Self> {
        let egl = load_egl()?;

        let display = unsafe { egl.get_display(display_ptr) }
            .context("EGL has no display for this Wayland connection")?;
        egl.initialize(display).context("initialising EGL")?;
        egl.bind_api(egl::OPENGL_ES_API)
            .context("binding the OpenGL ES API")?;

        // Alpha is requested because a wallpaper may letterbox, and the bars
        // should be the colour we paint rather than whatever was behind.
        let attributes = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
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
            .choose_first_config(display, &attributes)
            .context("choosing an EGL config")?
            .context("no EGL config supports GLES 3 on a window surface")?;

        let context_attributes = [egl::CONTEXT_MAJOR_VERSION, 3, egl::NONE];
        let context = egl
            .create_context(display, config, None, &context_attributes)
            .context("creating a GLES 3 context")?;

        // glow resolves GL entry points through EGL, so the context must be
        // current first or the loader hands back null pointers.
        egl.make_current(display, None, None, Some(context))
            .context("making the EGL context current")?;
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name)
                    .map(|p| p as *const std::ffi::c_void)
                    .unwrap_or(std::ptr::null())
            })
        };

        Ok(Renderer {
            egl,
            display,
            config,
            context,
            gl,
        })
    }

    /// Attach a GL surface to a layer surface.
    pub fn create_surface(
        &self,
        wl_surface: &WlSurface,
        width: i32,
        height: i32,
    ) -> Result<GlSurface> {
        let width = width.max(1);
        let height = height.max(1);
        let window = wayland_egl::WlEglSurface::new(wl_surface.id(), width, height)
            .context("creating a wl_egl_window")?;

        let surface = unsafe {
            self.egl.create_window_surface(
                self.display,
                self.config,
                window.ptr() as egl::NativeWindowType,
                None,
            )
        }
        .context("creating an EGL window surface")?;

        Ok(GlSurface {
            window,
            surface,
            size: (width, height),
        })
    }

    pub fn make_current(&self, surface: &GlSurface) -> Result<()> {
        self.egl
            .make_current(
                self.display,
                Some(surface.raw()),
                Some(surface.raw()),
                Some(self.context),
            )
            .context("making a GL surface current")?;
        let _ = self.egl.swap_interval(self.display, 0);
        Ok(())
    }

    pub fn swap_buffers(&self, surface: &GlSurface) -> Result<()> {
        self.egl
            .swap_buffers(self.display, surface.raw())
            .context("swapping buffers")
    }

    /// Present without waiting for vblank. A wallpaper has no reason to block
    /// the thread that also serves the control socket.
    pub fn set_nonblocking_present(&self) {
        let _ = self.egl.swap_interval(self.display, 0);
    }

    pub fn destroy_surface(&self, surface: GlSurface) {
        let _ = self.egl.destroy_surface(self.display, surface.raw());
        drop(surface.window);
    }

    /// A GL entry-point loader for libraries that resolve their own symbols.
    ///
    /// It holds its own reference to libEGL, so mpv can keep calling it for as
    /// long as it lives without depending on the renderer's lifetime.
    pub fn resolver(&self) -> ProcResolver {
        ProcResolver {
            egl: Arc::clone(&self.egl),
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

/// Resolves GL entry points through EGL for a foreign library.
pub struct ProcResolver {
    egl: Arc<Egl>,
}

impl ProcResolver {
    pub fn get(&self, name: &str) -> *mut std::ffi::c_void {
        self.egl
            .get_proc_address(name)
            .map(|p| p as *mut std::ffi::c_void)
            .unwrap_or(std::ptr::null_mut())
    }
}
