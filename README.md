# hyprwpe

Wallpaper Engine wallpapers on Hyprland — and ordinary wallpapers too, in the
same place.

> **Status: Active & Tested.** The daemon, Wayland layer surface manager, hardware-accelerated video renderer (`libmpv`),
> GLSL fragment shader renderer (Shadertoy), Wallpaper Engine 2D scene renderer (`scene.pkg`),
> GTK4 Libadwaita picker (`hyprwpe-gui`), and Quickshell desktop shell integration are fully working and verified.
> See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the architecture and design.

## What it is

Most Linux wallpaper tools are *setters*: hand one an image, it puts it up, it
stops thinking. That model breaks down for Wallpaper Engine wallpapers, which are
renderers that run continuously — holding hundreds of megabytes and burning CPU/GPU cycles,
even when covered by windows or during lockscreen.

hyprwpe treats the desktop background as a managed resource instead:
- **One daemon** owns the Wayland `background` layer surfaces across all outputs.
- **In-process renderers:** No child processes spawned or killed on wallpaper changes; transitions are atomic function calls.
- **Zero-overhead suspension:** Hyprland workspace and occlusion events automatically pause render loops when fully covered, locked, or during DPMS off (0% CPU/GPU).
- **Independent multi-monitor:** Each display output (`eDP-1`, `DP-2`, etc.) can render completely different wallpaper types (e.g., a video on one monitor, a GLSL shader on another).

## Supported Formats

| Format | Renderer Backend | Status | Features & Notes |
| :--- | :--- | :---: | :--- |
| **Static Images** (`.png`, `.jpg`, `.webp`) | Wayland SHM | **Complete** | Memory-efficient CPU blit with zero continuous GPU usage. Supports `fill`, `fit`, `stretch`, `center`. |
| **Videos** (`.mp4`, `.webm`, `.mkv`) | OpenGL ES 3.0 via `libmpv` | **Complete** | Hardware-accelerated playback with seamless looping. `libmpv` is loaded dynamically (`dlopen`) only when video wallpapers are set. |
| **GLSL Shaders** (`.frag`) | OpenGL ES 3.0 | **Complete** | Shadertoy-compatible inputs: `iResolution`, `iTime`, `iTimeDelta`, `iFrame`, `iFrameRate`, `iDate`. |
| **2D Scenes** (`scene.pkg`) | OpenGL ES 3.0 | **Supported** | Parses `scene.pkg` containers and `scene.json` trees; extracts and decompresses `.tex` textures (DXT1, DXT5, RGBA); renders 2D transformed quads with alpha blending. |
| **Web Wallpapers** (`index.html`) | - | *Unsupported* | Listed in catalog with `(unsupported)` flag to prevent dragging an entire multi-hundred-megabyte browser engine into the desktop background. |

## Quick Start

### 1. Build
```bash
cargo build --release
```
Binaries produced in `target/release/`:
* `hyprwpe`: daemon and command-line controller.
* `hyprwpe-gui`: GTK4 / Libadwaita wallpaper picker.

### 2. Run the Daemon
Start the daemon in the background or add it to your Hyprland configuration:
```bash
# In ~/.config/hypr/hyprland.conf:
exec-once = hyprwpe daemon
```

### 3. Set Wallpapers
```bash
# Set wallpaper across all monitors (accepts file path, workshop folder, or catalog ID):
hyprwpe set 3132826255
hyprwpe set /path/to/video.mp4
hyprwpe set examples/rainbow.frag

# Set per-monitor independent wallpapers:
hyprwpe set --output eDP-1 3132826255
hyprwpe set --output DP-2 examples/rainbow.frag --scaling fill

# List available wallpapers discovered from Steam Workshop & Pictures:
hyprwpe list

# Check live daemon state, outputs, scaling, and resident memory:
hyprwpe status

# Pause / resume rendering manually:
hyprwpe pause
hyprwpe resume

# Stop daemon:
hyprwpe stop
```

### 4. Open the GUI Picker
Launch `hyprwpe-gui` for a visual grid picker with filterable categories (All, Images, Scenes, Videos) and per-output target selection.

## Integrations

- **Hyprland:** Layer-shell integration on the `background` layer. See [`integration/hypr/README.md`](integration/hypr/README.md).
- **Quickshell (`end4`, `dots-chjwoo`):** Clean socket-based handover so desktop widgets float above animated wallpapers without layer conflicts. See [`integration/quickshell/README.md`](integration/quickshell/README.md).

## Wallpaper Engine content

hyprwpe reads Wallpaper Engine wallpapers you already own, from your own Steam
Workshop directory or any path you point it at. It does not download, bundle or
redistribute wallpapers, and it is not affiliated with Wallpaper Engine or Valve.

## Independence

hyprwpe implements the Wallpaper Engine formats from scratch under a clean-room
policy: no GPL implementation of these formats is read or copied, and every
format field is documented in [`docs/FORMATS.md`](docs/FORMATS.md) alongside how
it was determined. That is what keeps this project MIT-licensed. The policy is
written out in full under *Clean-room policy* in the architecture document.

## Credits

Building this is only practical because of work other people have already done
and shared:

- **[linux-wallpaperengine](https://github.com/Almamu/linux-wallpaperengine)** —
  proved these wallpapers can run on Linux at all. It is not a dependency, and
  its source is deliberately not read; the debt is to the demonstration that this
  is feasible, not to any of its implementation.
- **RenderDoc** — frame capture and graphics debugging.
- **RePKG and the wider reverse-engineering community** — public write-ups on the
  Wallpaper Engine container and texture formats.

Additional credits will be added as tools and references are actually used.

## Licence

MIT. See [`LICENSE`](LICENSE).
