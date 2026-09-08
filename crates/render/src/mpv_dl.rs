//! libmpv, loaded on demand.
//!
//! Linking libmpv pulls its whole dependency chain in at process start — the
//! NVIDIA driver, LLVM, ffmpeg — which measured 66 MB of resident memory in a
//! daemon that may never play a video. The daemon is the process that stays up
//! all session, so it should carry nothing it is not using.
//!
//! Only the dozen entry points hyprwpe actually calls are resolved. Anything
//! missing means an mpv too old to drive, and is reported as such rather than
//! crashing later on a null pointer.

use anyhow::{Context, Result};
use libloading::{Library, Symbol};
use libmpv_sys as mpv;
use std::ffi::{c_char, c_double, c_int, c_void};

/// Tried in order. The versioned name first, since that is what a package
/// installs; the bare name covers a self-built mpv.
const CANDIDATES: [&str; 2] = ["libmpv.so.2", "libmpv.so"];

type CreateFn = unsafe extern "C" fn() -> *mut mpv::mpv_handle;
type SetOptionStringFn =
    unsafe extern "C" fn(*mut mpv::mpv_handle, *const c_char, *const c_char) -> c_int;
type InitializeFn = unsafe extern "C" fn(*mut mpv::mpv_handle) -> c_int;
type CommandFn = unsafe extern "C" fn(*mut mpv::mpv_handle, *mut *const c_char) -> c_int;
type WaitEventFn = unsafe extern "C" fn(*mut mpv::mpv_handle, c_double) -> *mut mpv::mpv_event;
type ErrorStringFn = unsafe extern "C" fn(c_int) -> *const c_char;
type DestroyFn = unsafe extern "C" fn(*mut mpv::mpv_handle);
type TerminateDestroyFn = unsafe extern "C" fn(*mut mpv::mpv_handle);
type RenderCreateFn = unsafe extern "C" fn(
    *mut *mut mpv::mpv_render_context,
    *mut mpv::mpv_handle,
    *mut mpv::mpv_render_param,
) -> c_int;
type RenderSetUpdateCbFn = unsafe extern "C" fn(
    *mut mpv::mpv_render_context,
    Option<unsafe extern "C" fn(*mut c_void)>,
    *mut c_void,
);
type RenderFn =
    unsafe extern "C" fn(*mut mpv::mpv_render_context, *mut mpv::mpv_render_param) -> c_int;
type ReportSwapFn = unsafe extern "C" fn(*mut mpv::mpv_render_context);
type RenderFreeFn = unsafe extern "C" fn(*mut mpv::mpv_render_context);

pub struct Mpv {
    /// Kept alive: every function pointer below points into this library.
    _lib: Library,
    pub create: CreateFn,
    pub set_option_string: SetOptionStringFn,
    pub initialize: InitializeFn,
    pub command: CommandFn,
    pub wait_event: WaitEventFn,
    pub error_string: ErrorStringFn,
    pub destroy: DestroyFn,
    pub terminate_destroy: TerminateDestroyFn,
    pub render_context_create: RenderCreateFn,
    pub render_context_set_update_callback: RenderSetUpdateCbFn,
    pub render_context_render: RenderFn,
    pub render_context_report_swap: ReportSwapFn,
    pub render_context_free: RenderFreeFn,
}

/// Resolve one symbol, naming it if it is absent.
unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T> {
    let symbol: Symbol<T> = lib
        .get(name)
        .with_context(|| format!("libmpv has no {}", String::from_utf8_lossy(name)))?;
    Ok(*symbol)
}

impl Mpv {
    pub fn load() -> Result<Self> {
        let mut last = None;
        for name in CANDIDATES {
            match unsafe { Library::new(name) } {
                Ok(lib) => return unsafe { Self::bind(lib) },
                Err(e) => last = Some((name, e)),
            }
        }
        let (name, e) = last.expect("at least one candidate");
        Err(anyhow::anyhow!(
            "video wallpapers need mpv installed ({name}: {e})"
        ))
    }

    unsafe fn bind(lib: Library) -> Result<Self> {
        Ok(Mpv {
            create: sym(&lib, b"mpv_create\0")?,
            set_option_string: sym(&lib, b"mpv_set_option_string\0")?,
            initialize: sym(&lib, b"mpv_initialize\0")?,
            command: sym(&lib, b"mpv_command\0")?,
            wait_event: sym(&lib, b"mpv_wait_event\0")?,
            error_string: sym(&lib, b"mpv_error_string\0")?,
            destroy: sym(&lib, b"mpv_destroy\0")?,
            terminate_destroy: sym(&lib, b"mpv_terminate_destroy\0")?,
            render_context_create: sym(&lib, b"mpv_render_context_create\0")?,
            render_context_set_update_callback: sym(
                &lib,
                b"mpv_render_context_set_update_callback\0",
            )?,
            render_context_render: sym(&lib, b"mpv_render_context_render\0")?,
            render_context_report_swap: sym(&lib, b"mpv_render_context_report_swap\0")?,
            render_context_free: sym(&lib, b"mpv_render_context_free\0")?,
            _lib: lib,
        })
    }
}
