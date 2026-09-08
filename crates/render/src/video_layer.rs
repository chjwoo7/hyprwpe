//! Video wallpapers, decoded by libmpv and drawn through OpenGL ES.
//!
//! mpv is used as a decoder and renderer library, not as a wallpaper runtime:
//! hyprwpe owns the surface, the context and the frame timing, and mpv only
//! fills a framebuffer when asked. Nothing is spawned, so the process lifecycle
//! problems that motivated this project cannot reappear here.
//!
//! The GPU path is not an optimisation. mpv's own documentation calls its
//! software renderer "very slow" and single-threaded, which at these output
//! sizes would burn a core continuously.
//!
//! libmpv is loaded at runtime so a daemon that only ever shows still images
//! never pulls in mpv, ffmpeg, or the GPU driver stack — measured at 66 MB of
//! resident memory that would otherwise be wasted for the life of the session.

use anyhow::{bail, Context, Result};
use libmpv_sys as mpv;
use std::ffi::{c_void, CStr, CString};
use std::path::Path;
use std::sync::Arc;

use crate::gl::ProcResolver;
use crate::mpv_dl::Mpv;
use crate::scaling::Scaling;
use std::ptr;

/// A libmpv instance rendering one video into a GL framebuffer.
pub struct VideoPlayer {
    lib: Arc<Mpv>,
    handle: *mut mpv::mpv_handle,
    render: *mut mpv::mpv_render_context,
    /// Set by mpv's update callback when a new frame is ready. Read and cleared
    /// by the event loop, which is the only thing that draws.
    ///
    /// The callback runs on mpv's own thread, so this is the one piece of
    /// shared state and it is deliberately the simplest possible: a flag.
    wakeup: Box<WakeupFlag>,
}

pub struct WakeupFlag(std::sync::atomic::AtomicBool);

impl WakeupFlag {
    fn take(&self) -> bool {
        self.0.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

/// mpv calls this from its render thread when a frame is ready.
unsafe extern "C" fn on_update(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    let flag = &*(ctx as *const WakeupFlag);
    flag.0.store(true, std::sync::atomic::Ordering::Release);
}

/// mpv resolves GL entry points through this, using the EGL loader we hand it.
unsafe extern "C" fn get_proc_address(
    ctx: *mut c_void,
    name: *const std::os::raw::c_char,
) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return ptr::null_mut();
    }
    let resolver = &*(ctx as *const ProcResolver);
    let Ok(name) = CStr::from_ptr(name).to_str() else {
        return ptr::null_mut();
    };
    resolver.get(name)
}

impl VideoPlayer {
    /// Start playing `path`, rendering through the caller's current GL context.
    ///
    /// `resolver` must outlive the player: mpv keeps the pointer and calls it
    /// whenever it needs another GL function.
    pub fn new(
        path: &Path,
        scaling: Scaling,
        resolver: &ProcResolver,
        lib: Arc<Mpv>,
    ) -> Result<Self> {
        unsafe {
            let handle = (lib.create)();
            if handle.is_null() {
                bail!("mpv_create failed");
            }

            // A wallpaper is not a media player: no window, no OSD, no input,
            // no config file that might contradict any of that.
            for (key, value) in [
                ("config", "no"),
                ("terminal", "no"),
                ("osc", "no"),
                ("input-default-bindings", "no"),
                ("input-vo-keyboard", "no"),
                ("osd-level", "0"),
                ("loop-file", "inf"),
                ("audio", "no"),
                ("vo", "libmpv"),
                ("hwdec", "no"),
                // Decode ahead just enough to keep playing; a wallpaper has no
                // reason to hold seconds of frames in memory.
                ("cache", "no"),
                ("vd-lavc-threads", "2"),
                // A wallpaper loops a short clip; there is no seeking and no
                // reason to hold megabytes of demuxed packets.
                ("demuxer-max-bytes", "8MiB"),
                ("demuxer-max-back-bytes", "0"),
            ] {
                let k = CString::new(key)?;
                let v = CString::new(value)?;
                (lib.set_option_string)(handle, k.as_ptr(), v.as_ptr());
            }

            // mpv's default is letterbox, which would ignore the mode the user
            // asked for. These are the same four modes the image renderer has.
            for (key, value) in match scaling {
                Scaling::Fill => [
                    ("keepaspect", "yes"),
                    ("panscan", "1.0"),
                    ("video-unscaled", "no"),
                ],
                Scaling::Fit => [
                    ("keepaspect", "yes"),
                    ("panscan", "0.0"),
                    ("video-unscaled", "no"),
                ],
                Scaling::Stretch => [
                    ("keepaspect", "no"),
                    ("panscan", "0.0"),
                    ("video-unscaled", "no"),
                ],
                Scaling::Center => [
                    ("keepaspect", "yes"),
                    ("panscan", "0.0"),
                    ("video-unscaled", "yes"),
                ],
            } {
                let k = CString::new(key)?;
                let v = CString::new(value)?;
                (lib.set_option_string)(handle, k.as_ptr(), v.as_ptr());
            }

            if (lib.initialize)(handle) < 0 {
                (lib.destroy)(handle);
                bail!("mpv_initialize failed");
            }

            let mut api = CString::new("opengl")?.into_raw();
            let mut init_params = mpv::mpv_opengl_init_params {
                get_proc_address: Some(get_proc_address),
                get_proc_address_ctx: resolver as *const ProcResolver as *mut c_void,
                extra_exts: ptr::null(),
            };

            let mut params = [
                mpv::mpv_render_param {
                    type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                    data: api as *mut c_void,
                },
                mpv::mpv_render_param {
                    type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                    data: &mut init_params as *mut _ as *mut c_void,
                },
                mpv::mpv_render_param {
                    type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];

            let mut render: *mut mpv::mpv_render_context = ptr::null_mut();
            let rc =
                (lib.render_context_create)(&mut render, handle, params.as_mut_ptr());
            drop(CString::from_raw(api));
            api = ptr::null_mut();
            let _ = api;
            if rc < 0 {
                (lib.destroy)(handle);
                bail!("mpv_render_context_create failed ({rc})");
            }

            let wakeup = Box::new(WakeupFlag(std::sync::atomic::AtomicBool::new(true)));
            (lib.render_context_set_update_callback)(
                render,
                Some(on_update),
                wakeup.as_ref() as *const WakeupFlag as *mut c_void,
            );

            let file = CString::new(path.as_os_str().as_encoded_bytes())
                .context("video path contains a NUL byte")?;
            let loadfile = CString::new("loadfile")?;
            let mut cmd = [
                loadfile.as_ptr(),
                file.as_ptr(),
                ptr::null(),
            ];
            let rc = (lib.command)(handle, cmd.as_mut_ptr());
            if rc < 0 {
                (lib.render_context_free)(render);
                (lib.destroy)(handle);
                bail!("loading {} failed ({rc})", path.display());
            }

            Ok(VideoPlayer {
                lib,
                handle,
                render,
                wakeup,
            })
        }
    }

    /// Whether mpv has a new frame since the last draw.
    pub fn frame_pending(&self) -> bool {
        self.wakeup.take()
    }

    /// Draw the current frame into the bound framebuffer.
    ///
    /// `fbo` is 0 for the default framebuffer, which is what an EGL window
    /// surface gives us.
    pub fn render(&self, fbo: i32, width: i32, height: i32) -> Result<()> {
        unsafe {
            let mut fbo_param = mpv::mpv_opengl_fbo {
                fbo,
                w: width,
                h: height,
                internal_format: 0,
            };
            // Wayland surfaces are top-down; mpv defaults to OpenGL's bottom-up
            // convention, so without this the video plays upside down.
            let mut flip: std::os::raw::c_int = 1;
            let mut params = [
                mpv::mpv_render_param {
                    type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                    data: &mut fbo_param as *mut _ as *mut c_void,
                },
                mpv::mpv_render_param {
                    type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
                    data: &mut flip as *mut _ as *mut c_void,
                },
                mpv::mpv_render_param {
                    type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];
            let rc = (self.lib.render_context_render)(self.render, params.as_mut_ptr());
            if rc < 0 {
                bail!("mpv render failed ({rc})");
            }
        }
        Ok(())
    }

    /// Tell mpv the frame reached the screen, so its timing stays honest.
    pub fn report_swap(&self) {
        unsafe { (self.lib.render_context_report_swap)(self.render) }
    }

    /// Pause or resume decoding and playback. When paused, mpv stops advancing
    /// its clock and burns 0% CPU.
    pub fn set_paused(&self, paused: bool) {
        unsafe {
            if let (Ok(set), Ok(prop), Ok(val)) = (
                CString::new("set"),
                CString::new("pause"),
                CString::new(if paused { "yes" } else { "no" }),
            ) {
                let mut cmd = [set.as_ptr(), prop.as_ptr(), val.as_ptr(), ptr::null()];
                let _ = (self.lib.command)(self.handle, cmd.as_mut_ptr());
            }
        }
    }

    /// Drain mpv's event queue. Errors are surfaced; everything else is noise
    /// for a wallpaper.
    pub fn pump_events(&self) {
        unsafe {
            loop {
                let event = (self.lib.wait_event)(self.handle, 0.0);
                if event.is_null() {
                    return;
                }
                match (*event).event_id {
                    mpv::mpv_event_id_MPV_EVENT_NONE => return,
                    mpv::mpv_event_id_MPV_EVENT_END_FILE => {
                        let data = (*event).data as *mut mpv::mpv_event_end_file;
                        if !data.is_null() && (*data).error < 0 {
                            let msg =
                                CStr::from_ptr((self.lib.error_string)((*data).error));
                            eprintln!("hyprwpe: video ended: {}", msg.to_string_lossy());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        unsafe {
            // First detach the update callback so no more render notifications are queued.
            (self.lib.render_context_set_update_callback)(
                self.render,
                None,
                ptr::null_mut(),
            );

            // Free the render context while the OpenGL context is current.
            // mpv_render_context_free disables video, stops vo_libmpv and cleans up GL objects.
            (self.lib.render_context_free)(self.render);

            // Destroy the mpv core.
            (self.lib.destroy)(self.handle);
        }
    }
}
