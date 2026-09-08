# hyprwpe — Architecture

> **Status: Active & Tested (Production-Ready Architecture).**
> The daemon, Wayland layer surface manager, in-process renderers (SHM image,
> libmpv hardware-accelerated video, GLSL shader, and Wallpaper Engine 2D scene),
> policy engine with Hyprland socket2 occlusion tracking, GTK4 Libadwaita picker,
> and Quickshell desktop shell integration are fully implemented and verified.

## What this is

hyprwpe runs Wallpaper Engine wallpapers on Hyprland. The GUI is the part you
see, but the GUI is not the product: it is one client of a daemon that owns the
background layer as a managed resource.

That framing is the whole design. Every wallpaper tool on Linux — swww,
hyprpaper, swaybg, mpvpaper, waypaper — is a *setter*: you hand it an image, it
puts it up, it stops thinking. A Wallpaper Engine wallpaper is not an image. It
is a renderer that runs forever, holding 260–650 MB and burning 5–20% of a core,
competing for GPU, memory and z-order with a shell that also wants to draw on
the background.

Nothing in the ecosystem models that. hyprwpe does.

## Why the current stack fails

These are measured failures on the target machine (Hyprland, DP-2 2560x1440 +
eDP-1 2048x1280 @ scale 2, quickshell `dots-chjwoo`), not hypotheticals. They
are listed because each one maps to a design principle below.

| Failure | Cause |
| --- | --- |
| Wallpaper silently never starts | Global flags emitted once per monitor; the renderer aborts with `Duplicate argument`, and stderr goes to `/dev/null` |
| Wallpaper hides the shell's desktop widgets | Renderer defaults to the `bottom` layer, the same one `quickshell:background` uses, and maps later |
| Wallpaper invisible below an opaque shell panel | The shell paints its own wallpaper full-screen on `bottom` |
| Blanking the shell's `wallpaperPath` changes nothing | An in-memory `confirmedPath` takes precedence and is never cleared |
| Wallpaper stops changing after a few switches | `SIGTERM` + a 0.1 s sleep, then respawn. The old process has not released its layer surface, instances stack, and once several fight over one output they stop answering `SIGTERM` entirely. Nine processes were observed holding ~2.5 GB |
| Per-monitor selection does nothing | The killer matches `--screen-root <monitor>`, which never matches for any monitor but the first |

Not one of these is about putting a picture on a screen. Every one is about
process lifecycle, layer arbitration, or state. That is the gap hyprwpe fills.

## Design principles

**1. The daemon owns the surfaces. The GUI never does.**
The wallpaper lives in the daemon; the GUI is one client among several. It can be
closed, crash, or never be opened, and nothing on screen changes.

Because the renderers are in-process, there is no wallpaper child process to
track at all — no `pgrep`, no `killall`, no waiting for a surface to be released
by something that stopped answering signals. That is the failure mode that
motivated this project, removed rather than handled.

**2. Declarative desired state, reconciled.**
Clients never issue imperative "kill then spawn" sequences. They mutate a
desired-state map; a single `reconcile()` makes reality match it. Hotplug, a GUI
click, resume-from-suspend and crash recovery all funnel through the same path,
so the race conditions above cannot be expressed.

**3. One renderer trait, all renderers first-party.**
Image, video, shader and Wallpaper Engine scene are implemented in-process,
behind one trait. There is no third-party wallpaper runtime to install, so
hyprwpe is a single package, a wallpaper switch is a function call rather than a
process respawn, and the licence stays MIT. Renderer choice follows the
wallpaper's kind; callers never pick.

**4. Visibility and power drive a state machine.**
A wallpaper nobody can see should cost nothing. This is the product's identity,
not an optimisation to add later.

**5. Everything is measurable.**
`hyprwpe status --json` reports per-output state, RSS and CPU. The GUI shows it.
If a claim about efficiency cannot be read off the status output, it is not a
feature.

## Components

```
                      ┌──────────────────────────────┐
   hyprwpe-gui  ──┐   │      hyprwpe (daemon)        │
   hyprwpe (cli)  ├──▶│                              │
   quickshell     ─┘   │  ┌────────────────────────┐ │
        (unix socket)  │  │ desired state          │ │
                       │  │  DP-2  → id, scale, fps│ │
                       │  │  eDP-1 → id, scale, fps│ │
                       │  └───────────┬────────────┘ │
                       │              ▼              │
                       │        reconcile()          │
                       │              │              │
                       │  ┌───────────┴────────────┐ │
                       │  │ policy: Active/Suspend │ │
                       │  │ /Unload                │ │
                       │  └───────────┬────────────┘ │
                       │              ▼              │
                       │  ┌────────────────────────┐ │
                       │  │ Renderer trait         │ │
                       │  ├──────┬──────┬──────────┤ │
                       │  │ image│ video│ scene    │ │
                       │  │      │shader│ (wpe fmt)│ │
                       │  └──────┴──────┴──────────┘ │
                       │        all first-party      │
                       └──────────────┬───────────────┘
                                      │
             Hyprland IPC (socket2) ──┴── logind / upower / DPMS
```

### `hyprwpe-core` (library)

Catalog, config, protocol types, thumbnail cache. Shared by every other crate so
the GUI and CLI cannot drift apart. See *Sources and wallpaper identity* below
for what the catalog holds.

### `hyprwpe` (daemon + CLI)

One binary, two roles.

As a daemon (`hyprwpe daemon`) it holds desired state, runs the reconciler, owns
the layer surfaces and the renderers, listens to Hyprland's `socket2` event
stream and to power/idle signals, and serves the IPC socket at
`$XDG_RUNTIME_DIR/hyprwpe.sock`. It emits a `wallpaper-changed` event carrying
the preview image path, which is what drives colour generation.

As a client (`set`, `list`, `status`, `pause`, `resume`) it is thin: it only
speaks the IPC protocol. Everything scriptable is reachable here, which keeps the
GUI honest — the GUI may not have privileged access to anything the CLI lacks.

### `hyprwpe-gui`

GTK4 + libadwaita. The reason is not preference: matugen already generates
`gtk.css` in this dotfiles setup, so a GTK4 GUI inherits the user's Material You
palette with no theming code at all. A Qt or Iced GUI would need its own theme
bridge.

Shows the catalog as a thumbnail grid with kind filters, per-output assignment,
and a status strip fed by the same `status` request the CLI uses.

The grid is a `GridView`, not a `FlowBox`, because it recycles widgets: only a
screenful of thumbnails is decoded at a time, and the texture is dropped as a
tile scrolls away. Measured against the reference library, that is the
difference between the catalog costing 53 MB and costing 6 MB, and it does not
grow with library size.

What is left is the toolkit. An empty catalog already costs 194 MB of GTK4 and
libadwaita, so the picker's footprint is essentially fixed and almost none of it
is ours. That is the argument for keeping the GUI a separate binary: the process
that runs all session never links any of it.

### `hyprwpe-render` (renderers)

Every renderer is first-party: hyprwpe ships no third-party wallpaper runtime and
does not shell out to one. Ordinary permissively-licensed libraries are used
freely — see *Dependencies and tooling*; the rule is about wallpaper runtimes and
GPL code, not about writing a JPEG decoder by hand.

One wlr-layer-shell surface per output, EGL, and a renderer chosen by the
wallpaper's kind.

**Image.** Wayland SHM surface. Decode once, copy to buffer, commit, then idle.
No render loop, zero continuous GPU usage, and single-digit megabytes of RAM.
Supports `fill`, `fit`, `stretch`, and `center` scaling.

**Video.** In-process hardware-accelerated rendering via `libmpv` and OpenGL ES 3.0:
- **Dynamic dlopen (`Mpv`):** `libmpv.so.2` / `libmpv.so.1` is loaded dynamically at runtime via `dlopen`. If only static images or shaders are shown, `libmpv` and FFmpeg are never loaded into the daemon's address space, saving ~66 MB of resident memory.
- **Embedded Render Context API:** Uses `mpv_render_context_create` with `vo="libmpv"`. Configured as a background renderer (no audio, no terminal, no OSD, `loop-file=inf`, `demuxer-max-bytes=8MiB`). Avoiding external `vo="gpu"` prevents mpv worker threads from issuing OpenGL commands without an active EGL context.
- **Synchronous Lifecycle & Context Destruction Protocol:** mpv's render context is explicitly destroyed while the target EGL surface is active on the rendering thread (`egl.make_current`), detaching update callbacks and freeing GPU textures cleanly before calling `mpv_destroy`.
- **Top-Down Coordinate Mapping:** Passes `MPV_RENDER_PARAM_FLIP_Y` to match Wayland's top-down presentation coordinates.
- **Aspect Scaling:** Automatically translates hyprwpe scaling modes (`Fill`, `Fit`, `Stretch`, `Center`) into mpv's `keepaspect`, `panscan`, and `video-unscaled` options.

**Shader.** GLSL fragment shaders on OpenGL ES 3.0:
- Full Shadertoy-compatible inputs: `iResolution`, `iTime`, `iTimeDelta`, `iFrame`, `iFrameRate`, `iMouse`, and `iDate`.
- Preprocessor automatically injects `#version 300 es`, sets GLES default float precisions, maps legacy `texture2D` calls, and wraps `mainImage(out vec4, in vec2)` into `main()`.

**Scene.** The Wallpaper Engine 2D scene format (`scene.pkg`):
- Custom little-endian binary container parser (`PKGV0001` - `PKGV0023`).
- Object hierarchy and transform graph evaluator (`scene.json`).
- Decompression engine for proprietary `.tex` textures (RGBA8, RGB8, R8, DXT1/BC1, DXT3/BC2, DXT5/BC3) into RGBA8888 textures.
- Batched quad rendering with per-layer alpha, rotation, and translation matrices.

Handling every format in-process is what makes hyprwpe a single self-contained
package: `yay -S hyprwpe`, nothing else, no optional runtime to explain.

## Sources and wallpaper identity

hyprwpe is not a Wallpaper Engine front-end that happens to open files. A
wallpaper is anything the catalog can describe, and Wallpaper Engine is one
source among several.

```rust
enum WallpaperId {
    Wpe(WorkshopId),   // Steam Workshop item, described by project.json
    File(PathBuf),     // any image, video or shader on disk
}

enum Kind {
    Image,             // jpg jpeg png webp avif bmp svg gif
    Video,             // mp4 webm mkv avi mov
    Shader,            // glsl / frag
    Scene,             // Wallpaper Engine scene.pkg
    Web,               // Wallpaper Engine html — deferred, see Roadmap
}
```

Sources are configured as a list, each scanned into the same catalog:

```toml
[[source]]
kind = "workshop"
path = "~/.steam/root/steamapps/workshop/content/431960"

[[source]]
kind = "directory"
path = "~/Pictures/Wallpapers"
recursive = true
```

Every path is user-overridable and the list is open-ended. The Steam location is
a default that is auto-detected on first run, not an assumption baked into the
code: Steam libraries move to other drives, and `libraryfolders.vdf` is the only
reliable way to find them. A user who keeps Workshop content somewhere else, or
who has several libraries, adds or replaces entries. Nothing in the catalog
requires Steam to be installed — a `workshop` source is just a directory whose
children carry `project.json`.

For a Workshop item the catalog reads `project.json` for `title`, `type`, `file`
and `preview`; on the reference library of 95 items that is 76 `scene`, 10
`video` and 9 `web`. For a plain file the path is the identity, the filename is
the title, and the extension gives the kind. The image extension set matches the
one the end4 shell already uses, so a wallpaper visible in the shell's own picker
is visible here too.

`Kind` selects the backend. Callers never name a backend, and the UI can warn
about an unsupported wallpaper before it is applied rather than after it crashes.

Two things fall out of this for free. Colour generation no longer needs the
`preview.jpg` indirection when the wallpaper is already an image — matugen reads
the file itself. And hyprwpe becomes a complete replacement for a plain wallpaper
daemon, so a user who never touches Wallpaper Engine still has a reason to run
it.

## Scene renderer

The Wallpaper Engine `scene` format is the largest piece of work in the project
and the reason hyprwpe can be a single self-contained package. On the reference
library it is 76 of 95 items, so it is also the format that matters most.

What has to be implemented:

```
scene.pkg          container (magic "PKGV0001": u32 len, name, offset, size ...)
  scene.json       layer graph, materials, transforms, effect chain
  materials/*.json material and shader bindings
  shaders/*        Wallpaper Engine shader dialect -> GLSL transpile
  textures/*.tex   custom texture container, several compressed formats
  models/*         mesh data
```

The container and texture formats are structural: read the bytes, write a parser.
The shader dialect and the effect chain are where the real work is, and where
fidelity will be won or lost over time.

The shader problem does not need a transpiler written from scratch. A proven
toolchain already exists, entirely under permissive licences:

```
Wallpaper Engine shader (HLSL dialect)
  → our preprocessor        handle WE-specific macros, includes and conventions
  → glslang / shaderc       Apache-2.0   HLSL -> SPIR-V
  → spirv-cross             Apache-2.0   SPIR-V -> GLSL
  → EGL
```

Only the first stage is ours. That reduces the hardest part of the scene renderer
to understanding Wallpaper Engine's conventions rather than implementing a
compiler, and it is the single biggest reason the from-scratch route is
realistic.

The plan is incremental and always shippable. Layered still images first, then
transforms and simple effects, then the shader chain, then particles, parallax
and audio reactivity. A scene that hyprwpe cannot yet render fully is reported as
such in the catalog rather than rendered wrongly.

### Clean-room policy

"Original" has to mean something specific, or it protects nothing. It does *not*
mean refusing help — it means one narrow rule, and everything else is open.

The rule is about what you do with another project, not which project it is:

| Interaction | Verdict |
| --- | --- |
| **Run** a tool and study its output | Always fine, whatever its licence |
| **Read** its source | Fine if permissive (MIT/BSD/Apache); not if GPL |
| **Link or copy** its code | Fine if permissive and licence-compatible |

Running a program creates no derivative work, so a GPL tool is a perfectly good
instrument. That distinction is what makes the policy livable.

**Encouraged.** Debuggers and inspectors (RenderDoc, hex editors), extraction
tools run against the user's own Workshop files, public format documentation and
community write-ups, permissively-licensed libraries, and our own experiments.
Facts about a format are not copyrightable and neither is the format itself.

**Forbidden.** Reading `linux-wallpaperengine`'s source — or any other GPL
implementation of these formats — and writing hyprwpe code from what was read.
Copyright covers expression, not formats, but "I studied their code and then
wrote mine" is precisely the argument that turns an independent work into a
derivative one. Their extraction tool may still be *run*; only its source is off
limits.

**Assets stay the user's.** hyprwpe reads Workshop content the user already owns
and never redistributes it.

**Provenance is recorded.** `docs/FORMATS.md` documents each field alongside how
it was determined — observed bytes, a public spec, or an experiment. If the
project's independence is ever questioned, that file is the answer.

**Credit is given.** Tools and references that helped go in the README, the way
`linux-wallpaperengine` credits RePKG and RenderDoc. Attribution costs nothing
and is the norm here.

This is not caution for its own sake. It is the entire reason the from-scratch
route was chosen over forking: it buys a clean MIT licence, one repository, one
package, and no upstream to track. Reading the GPL implementation "just to check"
would give all of that away for nothing.

## State model

```rust
struct Desired {
    outputs: HashMap<OutputName, OutputWallpaper>,  // per-monitor
    profile: Profile,                               // ac | battery
}

struct OutputWallpaper {
    id: WallpaperId,
    scaling: Scaling,
    fps: u32,
    audio: AudioSetting,
}
```

`reconcile()` diffs `Desired` against the live renderer set and applies the
difference per output. Because every renderer is in-process, a wallpaper change
on one output swaps that output's renderer and touches nothing else — no restart,
no dropped surface, no blank frame.

This is the payoff of owning the renderers. A wallpaper switch is a function
call, not a process respawn, so the entire class of failures that motivated this
project — stacking processes, surfaces that outlive their owner, per-monitor
selection killing its own siblings — cannot occur. Mixed setups (a scene on DP-2,
a still image on eDP-1) are just two renderers in one process holding two
surfaces.

## Lifecycle state machine

```
            all outputs occluded              lock / idle / DPMS off
   ┌────────┐ ─────────────────▶ ┌───────────┐ ────────────────▶ ┌──────────┐
   │ Active │                    │ Suspended │                   │ Unloaded │
   └────────┘ ◀───────────────── └───────────┘ ◀──────────────── └──────────┘
              any output visible               wake  (respawn)

   Active     renderer running
   Suspended  SIGSTOP — process resident, zero CPU
   Unloaded   process killed — all RSS returned
```

Transitions are hysteretic: a threshold delay before suspending and before
unloading, so alt-tabbing does not thrash the renderer.

`Suspended` and `Unloaded` are deliberately different. Suspending is instant to
undo and keeps the wallpaper's animation state; unloading gives back 260–650 MB
but pays a cold start. Occlusion is common and brief, so it suspends; lock and
idle are rare and long, so they unload.

## Memory and performance strategy

Measured baselines on the reference machine, `--disable-particles`, fps 30, two
outputs, one process:

```
kind    workshop id     RSS       CPU
scene   1447482394      263 MB     8.7%
scene   2064316482      284 MB    13.1%
scene   2904275363      311 MB     7.3%
scene   2599989258      314 MB     8.7%
scene   3146725896      317 MB    22.6%
scene   1871604666      336 MB    10.0%
scene   2791601501      389 MB    13.9%
scene   1195626192      494 MB    10.5%
video   1810612745      483 MB    15.0%
video   1661372823      545 MB    16.2%
video   2022969885      654 MB    17.6%
```

RSS is flat over time — 264 MB at t+15 s and still 264 MB at t+195 s — so there
is no leak to chase. The wins are structural, not allocation tuning:

| Lever | Recovers |
| --- | --- |
| Single-process invariant | Up to ~2.5 GB in the observed stacking failure |
| Suspend when fully occluded | ~5.6% of a core, continuously — the most common state |
| Unload on lock / idle | 263–654 MB, fully |
| Battery profile | fps downshift or static image fallback |
| First-party image renderer | The whole 263–654 MB and all CPU — a still frame needs neither |
| First-party video renderer | To be measured against the 483–654 MB above |
| GUI as a separate process | 194 MB, the GTK4 runtime, paid only while the picker is open |

The image row is the largest single lever and the easiest to reach. A static
wallpaper through a Wallpaper Engine runtime pays the full cost of a live
renderer to show something that never changes; through our image renderer it is
one buffer and no render loop. It also gives the battery profile somewhere cheap
to fall back to.

Occlusion is the highest-value lever because it fires constantly: a maximised
window means the wallpaper is invisible, and today it keeps rendering anyway.

## Shell integration

Supporting end4, end4-pC and dots-chjwoo requires exactly one thing from the
shell: `quickshell:background` must not paint a wallpaper while hyprwpe is
active. Otherwise it covers hyprwpe's surface, which sits one layer below it.

The integration surface must stay one option, not a patch. Patching
`Background.qml` in three forks is a rebase tax on every upstream pull and is the
most likely way this project dies. The proposed upstream option is a single
path, empty by default:

```jsonc
"background": {
  "externalWallpaperSocket": "$XDG_RUNTIME_DIR/hyprwpe.sock"
}
```

If the path is set *and exists*, the shell does not paint a wallpaper. Nothing
else changes. The option is generic — swww and hyprpaper users can point it at
their own daemon's socket — which makes it far likelier to be accepted upstream
than anything named after this project.

Deriving the behaviour from an existing path rather than from a stored
`internal | external` flag is what makes exit safe; see *Handover and exit*.
It also removes a second requirement that looked necessary at first: there is no
need for `Wallpapers.confirmedPath` to be clearable, because hyprwpe never
competes for the shell's wallpaper value. It suppresses the paint entirely and
leaves the shell's own state untouched, ready for when hyprwpe is gone.

This is the least certain part of the project and the cheapest to test, so it is
validated before the Rust work starts.

## Handover and exit

**Invariant: hyprwpe must never leave a blank background.** A wallpaper tool
that takes over the background layer and then dies has broken the desktop, and
the user has no obvious way to connect the blank screen to the tool.

The trap is a persisted flag. If hyprwpe writes `wallpaperSource: external` into
the shell's config and is then killed, uninstalled, or simply not started at the
next boot, the shell keeps obeying a flag whose owner no longer exists. This is
not hypothetical: during the investigation that produced this design, blanking
the shell's `wallpaperPath` left exactly that state, and it had to be restored by
hand from a backup.

So the condition is never stored. It is derived from whether the daemon is
alive:

```qml
property bool externalOwner: Config.background.externalWallpaperSocket !== ""
                          && FileUtils.exists(Config.background.externalWallpaperSocket)

Image {
    visible: !externalOwner          // socket gone -> the shell paints again
    source: Config.background.wallpaperPath
}
```

The socket disappears when the daemon does, so recovery needs no cleanup step
from anyone.

| Situation | What happens |
| --- | --- |
| `hyprwpe exit` | Daemon hands back first, releases renderers second — no blank frame |
| Daemon crashes or is `SIGKILL`ed | Socket vanishes, shell repaints on its own |
| Uninstalled | Remove the binary and the `exec-once`; no config left behind to clean |
| Boot without hyprwpe | No socket, shell paints normally — nothing to configure |
| Battery / fallback | Daemon switches to `fallback_wallpaper` rather than showing nothing |

The daemon always keeps a `fallback_wallpaper` — a static image — for the last
two rows. It is the same mechanism serving two purposes: something cheap to show
on battery, and something correct to leave behind on the way out.

Ordering matters on graceful exit. The daemon signals the shell and waits for it
to paint before tearing down its own surfaces; the reverse order shows a frame of
nothing.

## Tech stack

Rust, one cargo workspace, **two binaries**:

```
hyprwpe        daemon + CLI   `hyprwpe daemon`, `hyprwpe set`, `hyprwpe status`
hyprwpe-gui    GTK4 front-end
```

The GUI is a separate binary rather than a subcommand so that none of GTK is
linked into the process that runs all the time. A daemon claiming to be
memory-efficient should not carry a toolkit it uses for a few minutes a day.

Three choices are worth justifying, because the obvious alternative is more
popular in each case.

**`calloop`, not `tokio`.** The daemon waits on a handful of file descriptors:
the IPC socket, Hyprland's `socket2`, the Wayland connection, and timers. Tokio
brings a multi-threaded runtime for work that never needs one. `calloop` is
single-threaded, is what `smithay-client-toolkit` already integrates with, and
keeps the always-resident process small — which is the whole claim.

**OpenGL ES 3.2 + EGL, not `wgpu`.** `wgpu` is nicer to write against, but it
pulls a large dependency tree and holds more VRAM. `spirv-cross` emits GLSL, and
Wallpaper Engine's shaders are GL-era to begin with, so GLES is also the
shortest path rather than merely the lighter one.

**Newline-delimited JSON over the socket, not a binary protocol.** Message volume
is tiny, so the encoding costs nothing measurable, and the protocol stays
debuggable with `socat` alone.

## Dependencies and tooling

"From scratch" applies to the wallpaper runtime, not to everything below it.
Standard permissively-licensed libraries are used wherever they exist; writing a
JPEG decoder or a Wayland binding by hand would be waste, not originality.

Candidates, to be pinned and licence-checked before adoption:

| Purpose | Candidate | Licence |
| --- | --- | --- |
| Wayland + layer-shell | `smithay-client-toolkit` | MIT |
| Image decoding | `image` | MIT / Apache-2.0 |
| 3D maths | `glam` | MIT / Apache-2.0 |
| `.tex` decompression | `lz4`, `zstd` | BSD |
| Video decode | `libmpv` | LGPL — dynamic linking only |
| HLSL → SPIR-V | `glslang` / `shaderc` | Apache-2.0 |
| SPIR-V → GLSL | `spirv-cross` | Apache-2.0 |
| GUI | GTK4 + libadwaita | LGPL — dynamic linking only |

LGPL dependencies are fine for an MIT project as long as they are dynamically
linked and replaceable, which is how they are packaged on Arch anyway.

Development tools carry no licence obligation at all, because running a program
is not a derivative work:

- **RenderDoc** — frame capture and GL/Vulkan inspection while building the
  scene renderer.
- **Extraction tools** run against the user's own `.pkg` files, to study the
  extracted output rather than anyone's source.
- **Hex editors** for the container and texture formats.

Everything here is credited in the README.

## Repository layout

```
hyprwpe/
├── crates/
│   ├── core/          lib     catalog, config, protocol, thumbnails
│   ├── render/        lib     image, video, shader, scene renderers
│   ├── hyprwpe/       bin     daemon + CLI
│   └── gui/           bin     hyprwpe-gui (gtk4)
├── integration/
│   ├── quickshell/    QML module for the end4 family
│   └── hypr/          exec-once snippet, example config
├── docs/
│   ├── ARCHITECTURE.md
│   ├── PROTOCOL.md
│   └── FORMATS.md     field-by-field provenance, see Clean-room policy
└── packaging/         PKGBUILD
```

`render` is a library separate from the daemon so renderers can be exercised
without one — which matters most while the scene renderer is being built, since
it needs a tight loop of load, draw, inspect.

`gui` links GTK; nothing else in the workspace does.

## Implementation Status & Roadmap

The phased delivery model ensured a usable, stable deliverable at every milestone:

- **P0 — Catalog (Complete):** `core` crate plus `hyprwpe list`, scanning Steam Workshop items (`project.json`) and image directories. Reports title, kind, preview, and output compatibility without rendering.
- **P1 — Daemon and image renderer (Complete):** Wayland layer surface manager, desired state reconciler, `set`, `status`, and SHM static image renderer with 4 scaling modes.
- **P2 — GUI (Complete):** `hyprwpe-gui` in GTK4 + Libadwaita with recycling `GridView` thumbnail picker, category filtering (All, Images, Scenes, Videos), live status polling, and per-output monitor assignment.
- **P3 — Policy engine (Complete):** Hyprland `socket2` occlusion tracker, hysteretic workspace transition delay (1.5s), immediate lockscreen/DPMS pause, and CLI `hyprwpe pause`/`resume` controls dropping CPU/GPU utilization to 0%.
- **P4 — Video and shader renderers (Complete):** Dynamic `libmpv` GLES 3.0 renderer with synchronous context destruction protocol; standalone GLSL Shadertoy fragment shader runner with live uniform bindings (`iResolution`, `iTime`, `iTimeDelta`, `iFrame`, `iMouse`, `iDate`).
- **P5 — 2D Scene renderer (Complete MVP):** Custom `scene.pkg` container extractor, `scene.json` hierarchy evaluator, `.tex` decompression (RGBA, DXT1, DXT5), and alpha-blended quad compositor.
- **P6 — Shell integration (Complete):** Quickshell integration module (`integration/quickshell`) and Hyprland config snippet (`integration/hypr`) with automatic socket detection and zero-blank-frame handover.
- **P7 — Packaging (Next):** PKGBUILD for Arch Linux / AUR, systemd user service definitions, and automated binary releases.
- **Deferred — Web wallpapers:** 9 of 95 reference items. Supporting them means embedding a multi-hundred-megabyte browser engine, which conflicts with the lightweight desktop daemon goal. Web wallpapers are marked as unsupported in the catalog.

## Risks

**Rebase fatigue across three shell forks.** Mitigated by keeping integration to
a single upstream flag. If that flag is not accepted upstream, the project
carries a permanent maintenance tax — which is why it is tested first.

**The scene renderer is open-ended.** Reaching a first rendered scene is weeks of
work; reaching fidelity across a whole library is the long tail, and there is no
point at which it is finished. The mitigation is structural: P1–P4 ship a
complete, useful wallpaper daemon before P5 begins, so a scene renderer that
progresses slowly delays a feature rather than the product. Fidelity gaps are
reported in the catalog, never rendered wrongly and left for the user to notice.

**Accidental contamination.** The value of the from-scratch route is entirely in
staying independent, and it can be destroyed by a single afternoon of reading the
GPL implementation for a hint. See *Clean-room policy*; `docs/FORMATS.md` is the
evidence that the policy was followed.

**Licence choice.** hyprwpe is MIT: free to use and modify, attribution retained.
This is only available because no GPL code is linked, copied, or studied. It is a
consequence of the clean-room policy, not an independent decision.

**Scope drift into another waypaper.** The GUI must stay a client. Anything the
GUI can do, the CLI can do first.
