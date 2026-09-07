//! Static image wallpaper on a `wlr-layer-shell` surface.
//!
//! A still image needs no GPU context and no render loop: decode once, write one
//! shm buffer per output, commit, and then sit idle. The compositor keeps the
//! buffer and reuses it every frame, so the wallpaper costs nothing to keep on
//! screen. This is the floor the rest of hyprwpe is measured against.

use anyhow::{Context, Result};
use image::{imageops::FilterType, DynamicImage, RgbaImage};
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
use std::path::Path;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use crate::scaling::{place, Scaling};

/// The layer to anchor to.
///
/// `Background` sits below everything, including a shell's own desktop panel.
/// That matters here: a shell drawing its wallpaper on `Bottom` would otherwise
/// cover this surface, and a wallpaper on `Bottom` would cover the shell's
/// desktop widgets. Neither is recoverable by z-order alone, so the default is
/// the bottom-most layer and the shell is expected to stop painting.
pub const WALLPAPER_LAYER: Layer = Layer::Background;

struct OutputLayer {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    /// Logical size from the compositor's configure.
    logical: (u32, u32),
    /// Output scale factor; the buffer is this many times larger.
    scale: i32,
    /// Set once the current size has been painted, so repeat configures with
    /// unchanged dimensions do not redraw.
    painted: Option<(u32, u32, i32)>,
}

/// Where the pixels come from.
///
/// A decoded 4K image is tens of megabytes, and after a surface is painted the
/// compositor holds the only copy that matters. `Reloadable` therefore drops the
/// decoded pixels between draws and decodes again on demand — draws happen on
/// configure, scale change and hotplug, which are rare. `Fixed` is for callers
/// that handed us pixels with no path to reload from.
enum ImageSource {
    Reloadable {
        path: std::path::PathBuf,
        cached: Option<RgbaImage>,
    },
    Fixed(RgbaImage),
}

impl ImageSource {
    fn load(&mut self) -> Result<&RgbaImage> {
        match self {
            ImageSource::Fixed(img) => Ok(img),
            ImageSource::Reloadable { path, cached } => {
                if cached.is_none() {
                    let img = image::open(&*path)
                        .with_context(|| format!("decoding {}", path.display()))?;
                    *cached = Some(img.to_rgba8());
                }
                Ok(cached.as_ref().expect("just decoded"))
            }
        }
    }

    /// Release the decoded pixels if they can be recovered later.
    fn release(&mut self) {
        if let ImageSource::Reloadable { cached, .. } = self {
            *cached = None;
        }
    }
}

pub struct ImageWallpaper {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    source: ImageSource,
    scaling: Scaling,
    background: [u8; 4],
    layers: Vec<OutputLayer>,
    exit: bool,
}

impl ImageWallpaper {
    /// Decode `path` and show it on every output until the connection ends.
    pub fn run(path: &Path, scaling: Scaling) -> Result<()> {
        // Decode once up front so an unreadable file fails immediately rather
        // than at the first configure.
        let probe = image::open(path).with_context(|| format!("decoding {}", path.display()))?;
        drop(probe);
        Self::start(
            ImageSource::Reloadable {
                path: path.to_path_buf(),
                cached: None,
            },
            scaling,
        )
    }

    pub fn run_image(source: DynamicImage, scaling: Scaling) -> Result<()> {
        Self::start(ImageSource::Fixed(source.to_rgba8()), scaling)
    }

    fn start(source: ImageSource, scaling: Scaling) -> Result<()> {
        let conn = Connection::connect_to_env()
            .context("connecting to the Wayland compositor (is WAYLAND_DISPLAY set?)")?;
        let (globals, mut queue) = registry_queue_init(&conn).context("initialising registry")?;
        let qh: QueueHandle<Self> = queue.handle();

        let shm = Shm::bind(&globals, &qh).context("compositor does not support wl_shm")?;
        let pool = SlotPool::new(1, &shm).context("creating shm pool")?;

        let mut state = ImageWallpaper {
            compositor: CompositorState::bind(&globals, &qh)
                .context("compositor does not support wl_compositor")?,
            layer_shell: LayerShell::bind(&globals, &qh)
                .context("compositor does not support wlr-layer-shell")?,
            output_state: OutputState::new(&globals, &qh),
            registry_state: RegistryState::new(&globals),
            shm,
            pool,
            source,
            scaling,
            background: [0, 0, 0, 255],
            layers: Vec::new(),
            exit: false,
        };

        while !state.exit {
            queue
                .blocking_dispatch(&mut state)
                .context("wayland dispatch")?;
        }
        Ok(())
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

        self.layers.push(OutputLayer {
            output,
            layer,
            logical: (0, 0),
            scale,
            painted: None,
        });
    }

    fn draw(&mut self, index: usize) -> Result<()> {
        let (logical, scale) = {
            let l = &self.layers[index];
            (l.logical, l.scale.max(1))
        };
        let (lw, lh) = logical;
        if lw == 0 || lh == 0 {
            return Ok(());
        }

        // Allocate in device pixels and tell the compositor the scale, so a
        // HiDPI output gets a sharp wallpaper instead of an upscaled one.
        let width = lw * scale as u32;
        let height = lh * scale as u32;
        let stride = width as i32 * 4;

        // Destructured so the pool and the image are borrowed as disjoint
        // fields; `canvas` borrows the pool for as long as it is written to.
        let Self {
            pool,
            source,
            scaling,
            background,
            layers,
            ..
        } = self;

        let (buffer, canvas) = pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .context("allocating shm buffer")?;

        paint(canvas, width, height, source.load()?, *scaling, *background);
        // The compositor now owns a copy; ours is recoverable from disk.
        source.release();

        let l = &mut layers[index];
        let surface: &wl_surface::WlSurface = l.layer.wl_surface();
        surface.set_buffer_scale(scale);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(surface).context("attaching buffer")?;
        surface.commit();
        l.painted = Some((lw, lh, scale));
        Ok(())
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
    {
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
}

impl CompositorHandler for ImageWallpaper {
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

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        // Nothing animates; no frame callbacks are requested.
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

impl OutputHandler for ImageWallpaper {
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
        if let Some(i) = self.layers.iter().position(|l| l.output == output) {
            if self.layers[i].scale != scale {
                self.layers[i].scale = scale;
                let _ = self.draw(i);
            }
        }
    }

    /// A monitor was unplugged. Dropping its layer surface is all that is
    /// needed; the remaining outputs are untouched.
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.layers.retain(|l| l.output != output);
    }
}

impl LayerShellHandler for ImageWallpaper {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.layers.retain(|l| &l.layer != layer);
        if self.layers.is_empty() {
            self.exit = true;
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
        let key = (w, h, self.layers[i].scale);
        if self.layers[i].painted == Some(key) {
            return;
        }
        if let Err(e) = self.draw(i) {
            eprintln!("hyprwpe: {e:#}");
        }
    }
}

impl ShmHandler for ImageWallpaper {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for ImageWallpaper {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_registry!(ImageWallpaper);

// smithay-client-toolkit 0.21 replaced the per-protocol delegate macros with one
// blanket impl covering every protocol the toolkit handles.
smithay_client_toolkit::delegate_dispatch2!(ImageWallpaper);

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
