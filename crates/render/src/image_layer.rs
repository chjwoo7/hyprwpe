//! Static image wallpapers on `wlr-layer-shell` surfaces, one per output.
//!
//! A still image needs no GPU context and no render loop: decode once, write one
//! shm buffer per output, commit, and go idle. The compositor keeps the buffer
//! and reuses it every frame, so the wallpaper costs nothing to keep on screen.
//! This is the floor the rest of hyprwpe is measured against.
//!
//! [`Wallpapers`] holds desired state and does not own an event loop, so the
//! daemon can drive it alongside its IPC socket. [`Wallpapers::run_standalone`]
//! wraps it in a loop for one-shot use.

use anyhow::{Context, Result};
use glow::HasContext;
use hyprwpe_core::protocol::OutputStatus;
use hyprwpe_core::settings::{Assignment, State};
use image::{imageops::FilterType, RgbaImage};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use wayland_client::{
    globals::{registry_queue_init, GlobalList},
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use std::sync::Arc;

use crate::gl::{GlSurface, ProcResolver, Renderer};
use crate::mpv_dl::Mpv;
use crate::scaling::{place, Scaling};
use crate::scene_layer::ScenePlayer;
use crate::shader_layer::ShaderPlayer;
use crate::video_layer::VideoPlayer;
use hyprwpe_core::Kind;
use smithay_client_toolkit::compositor::FrameCallbackData;

/// The layer to anchor to.
///
/// `Background` sits below everything, including a shell's own desktop panel.
/// That matters: a wallpaper on `Bottom` covers a shell's desktop widgets, and a
/// shell painting its own wallpaper on `Bottom` covers a wallpaper below it.
/// Neither is recoverable by z-order alone, so hyprwpe takes the bottom-most
/// layer and expects the shell to stop painting while it is running.
pub const WALLPAPER_LAYER: Layer = Layer::Background;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperSpec {
    pub path: PathBuf,
    pub scaling: Scaling,
    pub kind: Kind,
}

/// Which outputs a change applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Every output, including ones connected later. Also clears per-output
    /// pins, so "set everything" means what it says.
    All,
    Output(String),
}

/// Identifies a painted result, so an unchanged configure does not repaint.
/// How one output is being drawn.
///
/// A still image goes through shm and then sits idle forever. Video needs a GL
/// surface and a decoder, and is driven by frame callbacks. Keeping them as
/// separate variants means the cheap case stays cheap: an image wallpaper never
/// allocates a GL surface.
enum Backend {
    Idle,
    /// Painted at this size and scale; an unchanged configure does not repaint.
    Image {
        painted: Option<(u32, u32, i32)>,
    },
    Video {
        surface: GlSurface,
        player: VideoPlayer,
        /// Set while a frame callback is outstanding, so one is never requested
        /// twice for the same surface.
        awaiting_frame: bool,
    },
    Shader {
        surface: GlSurface,
        player: ShaderPlayer,
        awaiting_frame: bool,
    },
    Scene {
        surface: GlSurface,
        player: ScenePlayer,
        awaiting_frame: bool,
    },
}

struct OutputLayer {
    output: wl_output::WlOutput,
    name: String,
    layer: LayerSurface,
    /// Logical size from the compositor's configure.
    logical: (u32, u32),
    /// Output scale factor; the buffer is this many times larger.
    scale: i32,
    /// What the backend was built for. A change here tears it down and rebuilds.
    current: Option<WallpaperSpec>,
    backend: Backend,
}

pub struct Wallpapers {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    /// Applies to any output without its own entry.
    default_spec: Option<WallpaperSpec>,
    per_output: HashMap<String, WallpaperSpec>,
    /// Decoded pixels, held only between load and draw. A 4K image is tens of
    /// megabytes and the compositor owns the copy that matters once committed,
    /// so it is released after every draw and decoded again on demand.
    cache: Option<(PathBuf, RgbaImage)>,
    background: [u8; 4],
    layers: Vec<OutputLayer>,
    /// Created the first time a video is shown, then kept. Sessions that only
    /// ever use images never build a GL context at all.
    gl: Option<Renderer>,
    /// Handed to mpv, which keeps the pointer, so it must outlive every player.
    resolver: Option<Box<ProcResolver>>,
    /// Loaded on first video use, then shared across players. Sessions that
    /// never show a video never load libmpv at all.
    mpv: Option<Arc<Mpv>>,
    connection: Connection,
    qh: QueueHandle<Self>,
    exit: bool,
    paused: bool,
}

impl Wallpapers {
    pub fn new(
        connection: &Connection,
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
    ) -> Result<Self> {
        let shm = Shm::bind(globals, qh).context("compositor does not support wl_shm")?;
        let pool = SlotPool::new(1, &shm).context("creating shm pool")?;
        Ok(Wallpapers {
            compositor: CompositorState::bind(globals, qh)
                .context("compositor does not support wl_compositor")?,
            layer_shell: LayerShell::bind(globals, qh)
                .context("compositor does not support wlr-layer-shell")?,
            output_state: OutputState::new(globals, qh),
            registry_state: RegistryState::new(globals),
            shm,
            pool,
            default_spec: None,
            per_output: HashMap::new(),
            cache: None,
            background: [0, 0, 0, 255],
            layers: Vec::new(),
            gl: None,
            resolver: None,
            mpv: None,
            connection: connection.clone(),
            qh: qh.clone(),
            exit: false,
            paused: false,
        })
    }

    /// Show `spec` on `target`. Decoding is deferred to the next draw, so an
    /// unreadable file surfaces there rather than here.
    pub fn set(&mut self, target: Target, spec: WallpaperSpec) {
        match target {
            Target::All => {
                self.default_spec = Some(spec);
                self.per_output.clear();
            }
            Target::Output(name) => {
                self.per_output.insert(name, spec);
            }
        }
        self.redraw_all();
    }

    /// Desired state in a form that survives a restart.
    ///
    /// Derived from what the renderer already holds rather than tracked
    /// separately, so the file on disk cannot drift from what is on screen.
    pub fn snapshot(&self) -> State {
        State {
            default: self.default_spec.as_ref().map(assignment),
            outputs: self
                .per_output
                .iter()
                .map(|(name, spec)| (name.clone(), assignment(spec)))
                .collect(),
        }
    }

    /// Apply a saved state. Per-output entries are applied after the default
    /// because setting the default deliberately clears them.
    pub fn restore(&mut self, state: &State) {
        if let Some(a) = &state.default {
            if let Some(spec) = spec_from(a) {
                self.set(Target::All, spec);
            }
        }
        for (name, a) in &state.outputs {
            if let Some(spec) = spec_from(a) {
                self.set(Target::Output(name.clone()), spec);
            }
        }
    }

    /// Names of the outputs currently known, for validating a client's target.
    pub fn output_names(&self) -> Vec<String> {
        self.layers.iter().map(|l| l.name.clone()).collect()
    }

    fn spec_for(&self, name: &str) -> Option<&WallpaperSpec> {
        self.per_output.get(name).or(self.default_spec.as_ref())
    }

    fn redraw_all(&mut self) {
        for i in 0..self.layers.len() {
            if let Err(e) = self.draw(i) {
                eprintln!("hyprwpe: {e:#}");
            }
        }
    }

    pub fn status(&self) -> Vec<OutputStatus> {
        self.layers
            .iter()
            .map(|l| {
                let spec = self.spec_for(&l.name);
                OutputStatus {
                    name: l.name.clone(),
                    width: l.logical.0,
                    height: l.logical.1,
                    scale: l.scale,
                    wallpaper: spec.map(|s| s.path.clone()),
                    scaling: spec.map(|s| s.scaling.as_str().to_string()),
                }
            })
            .collect()
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn request_exit(&mut self) {
        self.exit = true;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Suspend wallpaper rendering. Video decoders and shader animations stop
    /// and frame callbacks cease, dropping CPU and GPU usage to zero.
    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused = true;
        for layer in &mut self.layers {
            match &mut layer.backend {
                Backend::Video { player, .. } => player.set_paused(true),
                Backend::Shader { player, .. } => player.set_paused(true),
                Backend::Scene { player, .. } => player.set_paused(true),
                _ => {}
            }
        }
    }

    /// Resume wallpaper rendering.
    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        self.paused = false;
        for i in 0..self.layers.len() {
            match &mut self.layers[i].backend {
                Backend::Video {
                    player,
                    awaiting_frame,
                    ..
                } => {
                    player.set_paused(false);
                    if !*awaiting_frame {
                        let _ = self.render_video(i);
                    }
                }
                Backend::Shader {
                    player,
                    awaiting_frame,
                    ..
                } => {
                    player.set_paused(false);
                    if !*awaiting_frame {
                        let _ = self.render_shader(i);
                    }
                }
                Backend::Scene {
                    player,
                    awaiting_frame,
                    ..
                } => {
                    player.set_paused(false);
                    if !*awaiting_frame {
                        let _ = self.render_scene(i);
                    }
                }
                _ => {}
            }
        }
    }

    /// Show one image on every output, driving an event loop until interrupted.
    pub fn run_standalone(path: &Path, scaling: Scaling) -> Result<()> {
        // Decode once up front so an unreadable file fails immediately rather
        // than at the first configure.
        image::open(path).with_context(|| format!("decoding {}", path.display()))?;

        let conn = Connection::connect_to_env()
            .context("connecting to the Wayland compositor (is WAYLAND_DISPLAY set?)")?;
        let (globals, mut queue) = registry_queue_init(&conn).context("initialising registry")?;
        let qh: QueueHandle<Self> = queue.handle();

        let mut state = Wallpapers::new(&conn, &globals, &qh)?;
        state.set(
            Target::All,
            WallpaperSpec {
                path: path.to_path_buf(),
                scaling,
                kind: Kind::Image,
            },
        );

        while !state.exit {
            queue
                .blocking_dispatch(&mut state)
                .context("wayland dispatch")?;
        }
        Ok(())
    }

    fn output_name(&self, output: &wl_output::WlOutput) -> String {
        self.output_state
            .info(output)
            .and_then(|i| i.name)
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn add_output(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            WALLPAPER_LAYER,
            Some("hyprwpe"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        // A wallpaper must never shrink the area available to windows, and must
        // never take input.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();

        let scale = self
            .output_state
            .info(&output)
            .map(|i| i.scale_factor)
            .unwrap_or(1);
        let name = self.output_name(&output);

        self.layers.push(OutputLayer {
            output,
            name,
            layer,
            logical: (0, 0),
            scale,
            current: None,
            backend: Backend::Idle,
        });
    }

    /// Decoded pixels for `path`, reusing the cache when it already holds them.
    fn load(&mut self, path: &Path) -> Result<()> {
        let stale = match &self.cache {
            Some((cached, _)) => cached != path,
            None => true,
        };
        if stale {
            let img = image::open(path)
                .with_context(|| format!("decoding {}", path.display()))?
                .to_rgba8();
            self.cache = Some((path.to_path_buf(), img));
        }
        Ok(())
    }

    /// Bring one output up to date with its desired wallpaper.
    fn draw(&mut self, index: usize) -> Result<()> {
        let (logical, scale, name) = {
            let l = &self.layers[index];
            (l.logical, l.scale.max(1), l.name.clone())
        };
        let (lw, lh) = logical;
        if lw == 0 || lh == 0 {
            return Ok(());
        }
        let Some(spec) = self.spec_for(&name).cloned() else {
            return Ok(());
        };

        // A different wallpaper means a different backend. Tearing down first
        // keeps the two paths from ever holding resources at once.
        if self.layers[index].current.as_ref() != Some(&spec) {
            self.teardown(index);
            self.layers[index].current = Some(spec.clone());
        }

        let width = lw * scale as u32;
        let height = lh * scale as u32;

        match spec.kind {
            Kind::Video => self.draw_video(index, &spec, width, height, scale),
            Kind::Shader => self.draw_shader(index, &spec, width, height, scale),
            Kind::Scene => self.draw_scene(index, &spec, width, height, scale),
            _ => self.draw_image(index, &spec, width, height, scale),
        }
    }

    /// Release whatever the backend held. Called before switching wallpapers and
    /// when an output goes away.
    fn teardown(&mut self, index: usize) {
        let backend = std::mem::replace(&mut self.layers[index].backend, Backend::Idle);
        match backend {
            Backend::Video {
                surface, player, ..
            } => {
                if let Some(gl) = &self.gl {
                    let _ = gl.make_current(&surface);
                }
                // The player must go before the surface it renders into.
                drop(player);
                if let Some(gl) = &self.gl {
                    gl.destroy_surface(surface);
                }
            }
            Backend::Shader {
                surface, player, ..
            } => {
                if let Some(gl) = &self.gl {
                    player.destroy(&gl.gl);
                    gl.destroy_surface(surface);
                }
            }
            Backend::Scene {
                surface, player, ..
            } => {
                if let Some(gl) = &self.gl {
                    player.destroy(&gl.gl);
                    gl.destroy_surface(surface);
                }
            }
            _ => {}
        }
    }

    fn draw_image(
        &mut self,
        index: usize,
        spec: &WallpaperSpec,
        width: u32,
        height: u32,
        scale: i32,
    ) -> Result<()> {
        if let Backend::Image { painted: Some(key) } = &self.layers[index].backend {
            if *key == (width, height, scale) {
                return Ok(());
            }
        }

        let stride = width as i32 * 4;
        self.load(&spec.path)?;

        // Destructured so the pool and the cached image are borrowed as disjoint
        // fields; `canvas` borrows the pool for as long as it is written to.
        let Self {
            pool,
            cache,
            background,
            layers,
            ..
        } = self;
        let source = &cache.as_ref().expect("loaded above").1;

        let (buffer, canvas) = pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .context("allocating shm buffer")?;

        paint(canvas, width, height, source, spec.scaling, *background);

        let l = &mut layers[index];
        let surface: &wl_surface::WlSurface = l.layer.wl_surface();
        surface.set_buffer_scale(scale);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(surface).context("attaching buffer")?;
        surface.commit();
        l.backend = Backend::Image {
            painted: Some((width, height, scale)),
        };

        // The compositor now owns a copy; ours is recoverable from disk.
        self.cache = None;
        Ok(())
    }

    /// Ensure the GL context is initialized, creating it on first use.
    fn ensure_gl(&mut self) -> Result<()> {
        if self.gl.is_some() {
            return Ok(());
        }
        let ptr = self.connection.backend().display_ptr() as *mut std::ffi::c_void;
        // Safe: the connection is owned by this struct and outlives the renderer.
        let renderer = unsafe { Renderer::new(ptr) }?;
        renderer.set_nonblocking_present();
        self.resolver = Some(Box::new(renderer.resolver()));
        self.gl = Some(renderer);
        Ok(())
    }

    /// Ensure libmpv is dynamically loaded for video playback.
    fn ensure_mpv(&mut self) -> Result<()> {
        self.ensure_gl()?;
        if self.mpv.is_none() {
            self.mpv = Some(Arc::new(Mpv::load()?));
        }
        Ok(())
    }

    fn draw_video(
        &mut self,
        index: usize,
        spec: &WallpaperSpec,
        width: u32,
        height: u32,
        scale: i32,
    ) -> Result<()> {
        self.ensure_mpv()?;

        if let Backend::Video { surface, .. } = &mut self.layers[index].backend {
            surface.resize(width as i32, height as i32);
        } else {
            let gl = self.gl.as_ref().expect("created above");
            let wl_surface = self.layers[index].layer.wl_surface().clone();
            wl_surface.set_buffer_scale(scale);

            let surface = gl.create_surface(&wl_surface, width as i32, height as i32)?;
            gl.make_current(&surface)?;

            let resolver = self.resolver.as_ref().expect("created above");
            let mpv = Arc::clone(self.mpv.as_ref().expect("created above"));
            let player = VideoPlayer::new(&spec.path, spec.scaling, resolver, mpv)
                .with_context(|| format!("playing {}", spec.path.display()))?;
            if self.paused {
                player.set_paused(true);
            }

            self.layers[index].backend = Backend::Video {
                surface,
                player,
                awaiting_frame: false,
            };
        }

        self.render_video(index)
    }

    fn draw_shader(
        &mut self,
        index: usize,
        spec: &WallpaperSpec,
        width: u32,
        height: u32,
        scale: i32,
    ) -> Result<()> {
        self.ensure_gl()?;

        if let Backend::Shader { surface, .. } = &mut self.layers[index].backend {
            surface.resize(width as i32, height as i32);
        } else {
            let gl = self.gl.as_ref().expect("created above");
            let wl_surface = self.layers[index].layer.wl_surface().clone();
            wl_surface.set_buffer_scale(scale);

            let surface = gl.create_surface(&wl_surface, width as i32, height as i32)?;
            gl.make_current(&surface)?;

            let mut player = ShaderPlayer::new(&spec.path, &gl.gl)
                .with_context(|| format!("loading shader {}", spec.path.display()))?;
            if self.paused {
                player.set_paused(true);
            }

            self.layers[index].backend = Backend::Shader {
                surface,
                player,
                awaiting_frame: false,
            };
        }

        self.render_shader(index)
    }

    /// Draw one shader frame and request a frame callback for the next.
    fn render_shader(&mut self, index: usize) -> Result<()> {
        let Some(gl) = &self.gl else { return Ok(()) };
        let qh = self.qh.clone();

        let Backend::Shader {
            surface,
            player,
            awaiting_frame,
        } = &mut self.layers[index].backend
        else {
            return Ok(());
        };

        let (w, h) = surface.size();
        gl.make_current(surface)?;
        player.render(&gl.gl, w, h)?;
        gl.swap_buffers(surface)?;

        if !self.paused && !*awaiting_frame {
            let wl_surface = self.layers[index].layer.wl_surface();
            wl_surface.frame(&qh, FrameCallbackData(wl_surface.clone()));
            wl_surface.commit();
            if let Backend::Shader { awaiting_frame, .. } = &mut self.layers[index].backend {
                *awaiting_frame = true;
            }
        }
        Ok(())
    }

    fn draw_scene(
        &mut self,
        index: usize,
        spec: &WallpaperSpec,
        width: u32,
        height: u32,
        scale: i32,
    ) -> Result<()> {
        self.ensure_gl()?;

        if let Backend::Scene { surface, .. } = &mut self.layers[index].backend {
            surface.resize(width as i32, height as i32);
        } else {
            let gl = self.gl.as_ref().expect("created above");
            let wl_surface = self.layers[index].layer.wl_surface().clone();
            wl_surface.set_buffer_scale(scale);

            let surface = gl.create_surface(&wl_surface, width as i32, height as i32)?;
            gl.make_current(&surface)?;

            let mut player = ScenePlayer::new(&spec.path, &gl.gl, spec.scaling)
                .with_context(|| format!("loading scene {}", spec.path.display()))?;
            if self.paused {
                player.set_paused(true);
            }

            self.layers[index].backend = Backend::Scene {
                surface,
                player,
                awaiting_frame: false,
            };
        }

        self.render_scene(index)
    }

    /// Draw one scene frame and request a frame callback for the next.
    fn render_scene(&mut self, index: usize) -> Result<()> {
        let Some(gl) = &self.gl else { return Ok(()) };
        let qh = self.qh.clone();

        let Backend::Scene {
            surface,
            player,
            awaiting_frame,
        } = &mut self.layers[index].backend
        else {
            return Ok(());
        };

        let (w, h) = surface.size();
        gl.make_current(surface)?;
        if std::env::var_os("HYPRWPE_DEBUG_SCENE").is_some() {
            if let Some((ew, eh)) = gl.surface_egl_size(surface) {
                eprintln!("scene-egl wl_size={}x{} egl_size={}x{}", w, h, ew, eh);
            }
        }
        player.render(&gl.gl, w, h)?;
        if let Some(dir) = std::env::var_os("HYPRWPE_DUMP_FB") {
            let n = (w as usize) * (h as usize) * 4;
            let mut buf = vec![0u8; n];
            unsafe {
                gl.gl.read_pixels(
                    0,
                    0,
                    w,
                    h,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut buf)),
                );
            }
            // GL origin is bottom-left; flip rows for a top-left PNG.
            let row_bytes = w as usize * 4;
            let mut flipped = vec![0u8; n];
            for y in 0..h as usize {
                let src = (h as usize - 1 - y) * row_bytes;
                flipped[y * row_bytes..(y + 1) * row_bytes]
                    .copy_from_slice(&buf[src..src + row_bytes]);
            }
            if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, flipped) {
                let path = std::path::Path::new(&dir).join(format!("fb_{}x{}.png", w, h));
                let _ = img.save(&path);
                eprintln!("dump-fb wrote {}", path.display());
            }
        }
        gl.swap_buffers(surface)?;

        if !self.paused && !*awaiting_frame {
            let wl_surface = self.layers[index].layer.wl_surface();
            wl_surface.frame(&qh, FrameCallbackData(wl_surface.clone()));
            wl_surface.commit();
            if let Backend::Scene { awaiting_frame, .. } = &mut self.layers[index].backend {
                *awaiting_frame = true;
            }
        }
        Ok(())
    }

    /// Draw one video frame and ask the compositor to tell us when to draw the
    /// next.
    ///
    /// Frame callbacks are what makes this cheap: a compositor stops sending
    /// them for a surface nobody can see, so an occluded video stops decoding
    /// without hyprwpe having to detect anything.
    fn render_video(&mut self, index: usize) -> Result<()> {
        let Some(gl) = &self.gl else { return Ok(()) };
        let qh = self.qh.clone();

        let Backend::Video {
            surface,
            player,
            awaiting_frame,
        } = &mut self.layers[index].backend
        else {
            return Ok(());
        };

        player.pump_events();

        let (w, h) = surface.size();
        gl.make_current(surface)?;
        player.render(0, w, h)?;
        gl.swap_buffers(surface)?;
        player.report_swap();

        if !self.paused && !*awaiting_frame {
            let wl_surface = self.layers[index].layer.wl_surface();
            wl_surface.frame(&qh, FrameCallbackData(wl_surface.clone()));
            wl_surface.commit();
            if let Backend::Video { awaiting_frame, .. } = &mut self.layers[index].backend {
                *awaiting_frame = true;
            }
        }
        Ok(())
    }
}

fn assignment(spec: &WallpaperSpec) -> Assignment {
    Assignment {
        path: spec.path.clone(),
        scaling: spec.scaling.as_str().to_string(),
        kind: Some(spec.kind.as_str().to_string()),
    }
}

/// An assignment whose scaling name is not recognised is dropped rather than
/// guessed: a state file written by a newer version should degrade quietly.
fn spec_from(a: &Assignment) -> Option<WallpaperSpec> {
    Some(WallpaperSpec {
        path: a.path.clone(),
        scaling: Scaling::parse(&a.scaling)?,
        kind: a
            .kind
            .as_deref()
            .and_then(kind_from_str)
            .unwrap_or(Kind::Image),
    })
}

fn kind_from_str(s: &str) -> Option<Kind> {
    match s {
        "image" => Some(Kind::Image),
        "video" => Some(Kind::Video),
        "shader" => Some(Kind::Shader),
        "scene" => Some(Kind::Scene),
        "web" => Some(Kind::Web),
        _ => None,
    }
}

/// Fill `canvas` with the scaled image. `canvas` is ARGB8888, which is BGRA in
/// memory order on little-endian hosts.
pub(crate) fn paint(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    source: &RgbaImage,
    scaling: Scaling,
    // `background` is RGBA, like the image; both are swapped to the buffer's
    // BGRA on write, so this function has one colour convention at its edge.
    background: [u8; 4],
) {
    let bg = [background[2], background[1], background[0], background[3]];
    let p = place(scaling, source.width(), source.height(), width, height);

    if p.width == 0 || p.height == 0 {
        for px in canvas.as_chunks_mut::<4>().0 {
            *px = bg;
        }
        return;
    }

    if p.leaves_gaps(width, height) {
        for px in canvas.as_chunks_mut::<4>().0 {
            *px = bg;
        }
    }

    let scaled = image::imageops::resize(
        source,
        p.width,
        p.height,
        // Lanczos on a one-shot decode is worth the milliseconds; it is paid
        // once per output and never again.
        FilterType::Lanczos3,
    );

    for row in 0..p.height as i64 {
        let dy = p.y + row;
        if dy < 0 || dy >= height as i64 {
            continue;
        }
        for col in 0..p.width as i64 {
            let dx = p.x + col;
            if dx < 0 || dx >= width as i64 {
                continue;
            }
            let src = scaled.get_pixel(col as u32, row as u32).0;
            let off = ((dy as usize) * width as usize + dx as usize) * 4;
            // RGBA -> BGRA
            canvas[off] = src[2];
            canvas[off + 1] = src[1];
            canvas[off + 2] = src[0];
            canvas[off + 3] = src[3];
        }
    }
}

impl CompositorHandler for Wallpapers {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if let Some(i) = self
            .layers
            .iter()
            .position(|l| l.layer.wl_surface() == surface)
        {
            if self.layers[i].scale != new_factor {
                self.layers[i].scale = new_factor;
                let _ = self.draw(i);
            }
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    /// The compositor is ready for another frame.
    ///
    /// Animated wallpapers (video, shaders) ask for these. A compositor stops
    /// sending them for a surface nobody can see, so an occluded animation stops
    /// drawing on its own — the cheapest possible form of suspend, with zero overhead.
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let Some(index) = self
            .layers
            .iter()
            .position(|l| l.layer.wl_surface() == surface)
        else {
            return;
        };

        let is_video = matches!(&self.layers[index].backend, Backend::Video { .. });
        let is_shader = matches!(&self.layers[index].backend, Backend::Shader { .. });
        let is_scene = matches!(&self.layers[index].backend, Backend::Scene { .. });

        if let Backend::Video { awaiting_frame, .. } = &mut self.layers[index].backend {
            *awaiting_frame = false;
        } else if let Backend::Shader { awaiting_frame, .. } = &mut self.layers[index].backend {
            *awaiting_frame = false;
        } else if let Backend::Scene { awaiting_frame, .. } = &mut self.layers[index].backend {
            *awaiting_frame = false;
        } else {
            return;
        }
        if self.paused {
            return;
        }
        if is_video {
            if let Err(e) = self.render_video(index) {
                eprintln!("hyprwpe: {e:#}");
            }
        } else if is_shader {
            if let Err(e) = self.render_shader(index) {
                eprintln!("hyprwpe: {e:#}");
            }
        } else if is_scene {
            if let Err(e) = self.render_scene(index) {
                eprintln!("hyprwpe: {e:#}");
            }
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Wallpapers {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.add_output(qh, output);
    }

    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let scale = self
            .output_state
            .info(&output)
            .map(|i| i.scale_factor)
            .unwrap_or(1);
        let name = self.output_name(&output);
        if let Some(i) = self.layers.iter().position(|l| l.output == output) {
            self.layers[i].name = name;
            if self.layers[i].scale != scale {
                self.layers[i].scale = scale;
                let _ = self.draw(i);
            }
        }
    }

    /// A monitor was unplugged. Dropping its layer surface is all that is
    /// needed; the remaining outputs are untouched. The per-output spec is kept
    /// so plugging the monitor back in restores what it had.
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(index) = self.layers.iter().position(|l| l.output == output) {
            // Release the GL surface and decoder before dropping the layer;
            // otherwise an unplugged monitor leaks a player that keeps decoding.
            self.teardown(index);
            self.layers.remove(index);
        }
    }
}

impl LayerShellHandler for Wallpapers {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(index) = self.layers.iter().position(|l| &l.layer == layer) {
            self.teardown(index);
            self.layers.remove(index);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(i) = self.layers.iter().position(|l| &l.layer == layer) else {
            return;
        };
        let (w, h) = configure.new_size;
        if w == 0 || h == 0 {
            return;
        }
        self.layers[i].logical = (w, h);
        if let Err(e) = self.draw(i) {
            eprintln!("hyprwpe: {e:#}");
        }
    }
}

impl ShmHandler for Wallpapers {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Wallpapers {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_registry!(Wallpapers);

// smithay-client-toolkit 0.21 replaced the per-protocol delegate macros with one
// blanket impl covering every protocol the toolkit handles.
smithay_client_toolkit::delegate_dispatch2!(Wallpapers);
#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(rgba))
    }

    fn px(canvas: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let o = (y as usize * w as usize + x as usize) * 4;
        [canvas[o], canvas[o + 1], canvas[o + 2], canvas[o + 3]]
    }

    /// The wire format is ARGB8888, which on a little-endian host means the
    /// bytes run B, G, R, A. Getting this backwards swaps red and blue, which
    /// is easy to miss on a greyscale test image.
    #[test]
    fn writes_bgra_not_rgba() {
        let src = solid(4, 4, [255, 0, 0, 255]); // pure red
        let mut canvas = vec![0u8; 4 * 4 * 4];
        paint(&mut canvas, 4, 4, &src, Scaling::Stretch, [0, 0, 0, 255]);
        assert_eq!(
            px(&canvas, 4, 2, 2),
            [0, 0, 255, 255],
            "red must land in the third byte"
        );
    }

    #[test]
    fn stretch_covers_every_pixel() {
        let src = solid(2, 2, [10, 20, 30, 255]);
        let mut canvas = vec![0u8; 8 * 6 * 4];
        paint(
            &mut canvas,
            8,
            6,
            &src,
            Scaling::Stretch,
            [255, 255, 255, 255],
        );
        for y in 0..6 {
            for x in 0..8 {
                assert_eq!(px(&canvas, 8, x, y), [30, 20, 10, 255], "gap at {x},{y}");
            }
        }
    }

    /// `Fit` letterboxes, so the bars must be the background colour rather than
    /// whatever the buffer happened to contain.
    #[test]
    fn fit_paints_the_letterbox_bars() {
        let src = solid(4, 1, [10, 20, 30, 255]); // very wide
        let mut canvas = vec![0xAAu8; 8 * 8 * 4]; // pre-filled with junk
        paint(&mut canvas, 8, 8, &src, Scaling::Fit, [1, 2, 3, 255]);
        assert_eq!(
            px(&canvas, 8, 0, 0),
            [3, 2, 1, 255],
            "top bar must be repainted"
        );
        assert_eq!(
            px(&canvas, 8, 7, 7),
            [3, 2, 1, 255],
            "bottom bar must be repainted"
        );
        assert_eq!(
            px(&canvas, 8, 4, 4),
            [30, 20, 10, 255],
            "image must be in the middle"
        );
    }

    /// `Fill` covers the surface, so no background should show and every pixel
    /// must come from the image.
    #[test]
    fn fill_leaves_no_background_visible() {
        let src = solid(4, 1, [10, 20, 30, 255]);
        let mut canvas = vec![0u8; 8 * 8 * 4];
        paint(&mut canvas, 8, 8, &src, Scaling::Fill, [255, 0, 255, 255]);
        for y in 0..8 {
            for x in 0..8 {
                assert_ne!(
                    px(&canvas, 8, x, y),
                    [255, 0, 255, 255],
                    "background at {x},{y}"
                );
            }
        }
    }

    /// A source larger than the surface must be clipped, not written past the
    /// end of the buffer.
    #[test]
    fn oversized_source_does_not_overflow_the_buffer() {
        let src = solid(64, 64, [1, 2, 3, 255]);
        let mut canvas = vec![0u8; 4 * 4 * 4];
        paint(&mut canvas, 4, 4, &src, Scaling::Center, [0, 0, 0, 255]);
        assert_eq!(px(&canvas, 4, 3, 3), [3, 2, 1, 255]);
    }

    #[test]
    fn degenerate_source_falls_back_to_background() {
        let src = RgbaImage::new(0, 0);
        let mut canvas = vec![0u8; 2 * 2 * 4];
        paint(&mut canvas, 2, 2, &src, Scaling::Fill, [9, 8, 7, 255]);
        // background is RGBA in, BGRA out
        assert_eq!(px(&canvas, 2, 1, 1), [7, 8, 9, 255]);
    }
}
