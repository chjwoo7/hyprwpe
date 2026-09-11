# Scene renderer — coverage and build order

The scene renderer is the largest piece of work in hyprwpe and the one with no
obvious finish line. This document replaces guesswork about that "long tail" with
a measurement, so the build order follows evidence rather than instinct.

Numbers come from a personal library of **75 scene packages, 1450 objects** —
every `scene.pkg` in a 95-item Workshop library (76 items declare `type: scene`;
one of them ships no package).
It is one library, not a representative sample of the Workshop — treat the shape
of the curve as informative and the exact percentages as local.

Method: parse every `scene.pkg`, extract `scene.json`, count object kinds and
effect references. See [`FORMATS.md`](FORMATS.md) for the container layout.

## Object kinds

| Kind | Objects | Wallpapers using it |
| --- | --- | --- |
| `image` | 763 | 75/75 |
| `particle` | 429 | **52/75** |
| `text` | 125 | 18/75 |
| `sound` | 81 | 41/75 |
| other (shape/solid) | 46 | 4/75 |
| `light` | 6 | 2/75 |

The result that changes the plan: **particles are not a late-stage feature.**
They appear in 69% of wallpapers, second only to images. Treating them as an
advanced extra would leave two thirds of a library visibly wrong.

`light` is the opposite — 6 objects across 2 wallpapers. It can wait
indefinitely.

## Effects

91 distinct effects appear in the corpus. The most common:

| Wallpapers | Objects | Effect |
| --- | --- | --- |
| 31/75 | 183 | `waterwaves` |
| 30/75 | 47 | `waterripple` |
| 28/75 | 118 | `shake` |
| 23/75 | 36 | `waterflow` |
| 19/75 | 109 | `opacity` |
| 17/75 | 43 | `pulse` |
| 14/75 | 48 | `foliagesway` |
| 12/75 | 13 | `godrays` |
| 10/75 | 51 | `tint` |
| 10/75 | 14 | `iris` |
| 9/75 | 16 | `blur` |
| 8/75 | 18 | `shine` |
| 8/75 | 23 | `blurprecise` |
| 7/75 | 8 | `filmgrain` |
| 6/75 | 37 | `scroll` |

Every effect is a `.json` plus shaders carried inside the wallpaper's own
package, so "implementing an effect" means supporting the parameters it binds —
not reimplementing a Wallpaper Engine built-in.

## Coverage curve

A wallpaper counts as covered only when **every** effect it uses is supported:

| Effects implemented | Wallpapers fully covered |
| --- | --- |
| 0 | 10/75 (13%) |
| 3 | 17/75 (22%) |
| 5 | 20/75 (26%) |
| 10 | 31/75 (41%) |
| 15 | 37/75 (49%) |
| 20 | 43/75 (57%) |
| 30 | 48/75 (64%) |
| 50 | 62/75 (82%) |
| 91 | 75/75 (100%) |

**There is no 80/20 here.** The curve is close to linear: the ten most common
effects buy 41%, and reaching 82% takes fifty. Anyone expecting a small number of
effects to unlock most of a library should look at this table first.

That is a genuinely useful negative result. It says the scene renderer will be a
long grind of individually small pieces rather than a few decisive ones — which
is exactly why it sits at P5, after hyprwpe is already a complete wallpaper
daemon.

## Build order

Ordered by wallpapers unlocked per unit of work.

**1. Container, scene graph, image objects.** Parse `scene.pkg` and `scene.json`,
build the object tree, draw image layers with transforms.

*Status (measured 2026-09, 75 packages, 723 image objects):* the image-object
path is implemented end to end.
- Texture-chain resolution (object -> `models/*.json` -> `materials/*.json`
  -> `materials/<name>.tex`) resolves **413/723** image references.
- Of those, **366 decode** to RGBA (embedded PNG/JPEG inside the `TEXV0005`
  container decodes via the `image` crate; raw-BC payloads are best-effort).
- **44/75 scenes have every image object decode**; **73/75** have at least one
  image object decoding (so they show their background rather than nothing).

The layout model validated against real scenes: object `origin` is the quad
centre, quad extent is `size * scale` in design units, `general.
orthogonalprojection {width,height}` names the design canvas, and object
`parent` references compose into a world transform (405/1450 objects). The
canvas is mapped onto the output with the **per-output scaling mode** the user
chose (fill/fit/stretch/center), projecting the **design rectangle the output
covers** rather than the canvas' extent, and `general.clearcolor` /
`clearenabled` paints the background behind the layers. See
[`FORMATS.md`](FORMATS.md#transform-semantics-validated-against-the-corpus) for
the evidence behind each rule.

Verification without a wallpaper: `tools/mkscene.py` builds a synthetic
`scene.pkg` whose every object has a known position (a canvas-sized colour grid
plus sized markers and a parented child), and
`cargo run -p hyprwpe-render --example scenecompose -- <pkg> out.png W H [mode] [time]`
software-composites it through the *same* transform module the GPU renderer
uses. A framing defect then shows up as a pixel in the wrong place, not a
judgement call. `tools/mkanim.py` is the same idea for animation: a marker on a
known path, so a sampled time either lands where the keyframes say or it does
not.

**1b. Property animation.** 6/75 wallpapers, and implemented: an object
property's keyframe tracks are sampled on the render clock and applied to
`origin`, `scale`, `angles` and `alpha`, including through `parent` chains
(`crates/core/src/animation.rs` + `scene_transform::animated_*`). Verified by
rendering the synthetic marker scene at fixed times — (400,400) at t=0,
(1900,1100) at t=1, (3400,1800) at t=2 — and live, where the centroid advances
between screenshots.

**1c. Puppet models (MDLV).** 12/75 wallpapers reference a `…_puppet.mdl`,
which replaces the quad with a deforming mesh. The parse is implemented in
`crates/core/src/mdlv.rs` and is **version driven**: sections are found by their
`MD??NNNN` tags and each one's end comes from the next-section offset the file
stores itself, so nothing indexes a specific wallpaper's bytes. It reads the mesh
(positions, UVs, triangle indices), the skeleton (`MDLS`: bone parents + 4x4 bind
matrices), attachments (`MDAT`) and animations (`MDLA`: name, mode, fps, length
and per-bone keyframes of position/rotation/scale).

Verified across the whole corpus with
`cargo run -p hyprwpe-core --example validate_mdlv -- <dir>`:
**48/48 files parse with a usable mesh**, spanning all five revisions
(`MDLV0013` 19, `0014` 3, `0016` 11, `0017` 1, `0023` 14) — e.g. the Akali body
1061 vertices / 1771 triangles / 33 bones / 12 animations, and the butterfly
48 / 81 / 1 / 1.

The per-vertex **skin weights** (two four-slot blocks just before the UV pair, the
first holding raw `u32` bone indices and the second their weights) parse too, and
**47/47 skinned files** satisfy both invariants: weights sum to 1.0 and every used
index is below the bone count, with zero violations over ~40k vertices.

**Skinning is implemented and validated** (`crates/render/src/skin.rs`). A bone's
stored matrix is its *local* transform and an animation keyframe is the same local
transform, so the per-bone skin is `animated_world * inverse(rest_world)` and each
vertex is the weighted blend of its influences. Two facts were pinned from geometry
rather than assumed: the matrices compose along the parent chain (composed, each
vertex's dominant bone lands ~3x closer than if read as absolute), and frame 0 is
*usually* the bind pose but not always (`Car_puppet` starts away from rest), so the
pose always comes from the keyframes. Offline proof:
`cargo run -p hyprwpe-render --example skinpreview -- --corpus <dir>` reports
**46 skinned models, 0 failures** — every pose across every animation stays finite
and bounded, and Akali's loop returns exactly to rest at `t = length / fps`.

The mesh is drawn in the scene renderer (`MeshRenderer` + per-frame CPU skinning,
`scene_layer::PuppetLayer`), with the model's `cropoffset` honoured; verified live
on both outputs and offline by the software rasteriser in `scenecompose`. Still to
come: attachments (`MDAT`) and the texture-channel / blend-rule tracks (both parsed
but not applied).

**2. Particles.** 52/75 wallpapers. The largest single jump available — and
implemented.

Particle definitions are **declarative data, not code**: an object names a
`particle` file whose `emitters`, `initializers`, `operators` and `renderers`
configure a closed vocabulary (2 emitter kinds, 10 initializers, 12 operators,
4 renderers across the corpus), with parameters looked up **by name** rather than
by offset, so a version that adds a parameter degrades to ignoring it. Because
every scene is therefore a combination of the same primitives, one simulator
serves all of them: `crates/render/src/particle.rs` runs it on the CPU with a
deterministic RNG, so a frame at a given time is reproducible and testable
without a GPU.

Two real bugs came out of building it: `alphafade` was compounding per frame
(multiplying alpha every step, so sprites vanished) — state is now derived from
an `alpha_base` each frame; and a long stall (a paused daemon) must not spawn a
burst, so a frame's `dt` is clamped.

Particles need the engine's own assets, which the user's Wallpaper Engine
installation already provides: **22/130 particle textures ship inside the
wallpaper package, the other 108 reference `materials/particle/*` in the engine's
`assets/` directory** (shaders, particle textures and fonts all live there).
`crates/core/src/assets.rs` resolves a name package-first then engine-assets, so
nothing is redistributed and a machine without Wallpaper Engine installed simply
loses those sprites rather than failing.

Those asset textures were the last blocker: they are not `TEXV0005` containers
but a `TEXB` block whose payload is **LZ4**, and whose sizes are 16.16 fixed
point (`>> 8`), with the mip count at `+16` and each mip's level stored in the
block. `TEXB0003` is single-level; `TEXB0004` carries mip levels. Decoding both
made the particle chain render: `scenecompose <pkg> out.png 1920 1080 fill 6`
draws **19 particles** where it previously drew none.

**3. The effect pipeline itself**, then effects in frequency order. The pipeline
is the hard part; individual effects after it are incremental.

An effect is not a built-in: the creator ships `effects/<name>/effect.json`
pointing at a material whose shader is ordinary GLSL inside the package. So the
host side of the contract is what has to be right, and it is implemented in
`crates/core/src/effect.rs`: the chain
`effect.json -> passes[].material -> material passes[].shader -> shaders/<name>.{frag,vert}`,
`#include` expansion, the engine prelude (these shaders rely on built-ins that
appear nowhere in the file — `texSample2D`, `mul`, `saturate`, the `CAST`
helpers), and uniform bindings read from the JSON comment each declaration
carries (`uniform float g_Scale; // {"material":"…ripple_scale","default":1}`).

Measured over the library with
`cargo run -p hyprwpe-core --example validate_effects`:
**91 distinct effects, 2896 shader stages prepared, 0 failures**, 17485 uniform
declarations of which 9681 bind to a material constant. Two lookup rules each
gated the entire library: includes resolve both bare and under `shaders/` (the
engine keeps `common*.h` there and every effect includes at least one), and
`usershadervalues` binds a uniform to a user property.

Assembling a source is not compiling it, and the difference matters.
`cargo run -p hyprwpe-render --example fxcompile` gets a real GLES 3 context
through **surfaceless EGL** — no window, no compositor, nothing on the user's
screen — and compiles every stage of every effect in the library. The driver's
own log is the only thing that can tell you whether shader *source* is real
GLSL, and it found bugs no string-level check could see.

Before the translation layer existed, **0 stages compiled**. Now:

| | before | after |
| --- | --- | --- |
| shader stages compiling | 0 | **2216** |
| effects compiling completely | 0 of 91 | **69 of 91** |

Getting there needed `crates/core/src/hlsl.rs`, a narrow HLSL→GLSL layer, because
the scene shaders are written in HLSL-flavoured GLSL and the engine's compiler
was lenient where GLSL ES is not. Every rule in it was forced by a measured
failure, not guessed:

- **Integer literals in float positions** — `3 * amt`, `pointer * 2 - 1`,
  `max(0, colour)`, `smoothstep(1 - g_Rough, 1, t)`. The largest class by far.
  Promotion is suppressed where an integer is *required*: inside a subscript,
  on a preprocessor line, and anywhere in a statement that declares an `int`
  (so `for (int i = 0; i < 10; i++)` is left intact).
- **`sample` as an identifier** — reserved in GLSL ES 3.0, used as a local by
  several effects.
- **`fmod`** — HLSL's name; GLSL has `mod`.
- **A macro defined twice** — two of the engine's headers define the `FORMAT_*`
  constants, and GLSL rejects *any* redefinition, so the repeat is guarded.
- **A uniform the material marks `"int": true`** while the shader declares it
  `float`, which is what makes `for (int i = u_Min; i < u_Max; i++)` legal.
- **The prelude itself was wrong four times.** The driver caught what no amount
  of reading could: it redefined `hsv2rgb`, `rgb2hsv`, `rotateVec2` and
  `greyscale`, which the engine's own `common.h` defines as functions
  ("function is already defined"); `frac` is HLSL; combo guards (`#if KERNEL ==
  0`) named undefined macros, and GLSL ES rejects an `#if` over an undefined
  name where HLSL read it as 0; and `CASTn` is a *broadcast* needing int and
  float overloads, while a shader's `#include`d helper can *use* `g_Texture0`
  before the shader declares it (the host now injects the sampler set ahead of
  the body and drops the shader's own duplicate, since a duplicate uniform is
  itself an error).

One bug is worth recording because it was self-inflicted and subtle: defaulting
every identifier in a `#if` to `0` also defaulted `FORMAT_DXT1`, *poisoning the
header's own definition* into a redefinition and failing every effect in the
`lightshafts` family. A name the source `#define`s is a macro, not a combo.

**What is left is 22 effects**, and they need a real answer rather than more
string shims: HLSL's implicit int↔float and int↔uint conversion on *variables*
(`sampleCount - 1.0`, `i / sampleDrop`, `RESOLUTION` used as a float), vec4→vec2
implicit truncation on assignment, and scalar↔vector broadcasting in builtins
(`max(0.0, albedo.rgb)`). Those are a typing pass, and until it exists those
effects are reported as unsupported rather than rendered wrongly — which is what
the honesty rule below requires. A handful of workshop effects also reference
engine-provided uniforms (`g_AudioSpectrum*`) that the engine injects but never
ships in `assets/shaders/`; their array sizes are not guessable, so they are not
being invented.

The same harness also links each pass's two stages into a program, because a
render pass needs a program, not two separate stages — and linking catches an
interface the stages disagree about (a `varying` never written, a mismatched
type) that compiling one stage at a time cannot. Measured: **1104 of 1105 pass
programs link**, so the chain is ready to drive real passes: render an object
off-screen, run the passes with framebuffer ping-pong, then draw the result.
Only the single effect that also fails to compile does not link.


The GL side runs too. `crates/render/src/effect_pass.rs` applies a chain as a
framebuffer ping-pong - a fullscreen quad per pass, reading the previous result -
with the values resolved in the engine's own order: the scene object's
`effects[].passes[].constantshadervalues`, then the material's, then the
`default` in the uniform's JSON comment. `RenderLayer` carries the chains, so an
object that stacks several effects gets them applied in order.

`cargo run -p hyprwpe-render --example scenerender -- <scene.pkg> out.png 1920 1080 fill`
renders a wallpaper through `ScenePlayer` on the surfaceless context and reports
coverage, mean colour and alpha, so the whole path is checkable without a
monitor; `--no-effects` and `--set key=value` make it a measurement rather than a
screenshot. On a real wallpaper (`1906757512`, `tint` as its final pass):

| run | covered | mean rgb |
| --- | --- | --- |
| default | 100.00% | 255.0, 248.0, 30.0 |
| `--set color=1 0 0` | 100.00% | 255.0, 0.0, 0.0 |
| `--no-effects` | 30.12% | 174.7, 177.7, 177.4 |

### The passes are a render graph, not a chain

An effect's passes do not simply feed each other. Each names the buffer it writes
(`target`) and which buffer feeds which sampler (`bind`), with `previous` meaning
the chain's input — a small render graph. `godrays` shows why it matters: its
cast pass writes a ray mask, and its final `combine` pass reads **both** the mask
and the untouched input, at two different slots. Feeding every pass the previous
result (the first implementation) lost the image completely and made those
wallpapers render **blank**. 17 of the 91 effects need this.

A pass also carries **`combos`**: compile-time branch selectors, not values.
`VERTICAL` decides whether the same `blur` shader is a horizontal or a vertical
pass, so a pass built with the wrong branch is not merely different, it can render
nothing. 45 of the 133 scene effect passes carry at least one; the scene's
selection overrides the material's, since one material may be used in several
configurations.

### Whole-library measurement

`tools/measure_scenes.sh` renders every scene headlessly (480x270, `--json` one
line each) and is the number this document trusts:

| | no effects | effects (after the render graph) |
| --- | --- | --- |
| scenes fully covered | 48 / 75 | **55 / 73** |
| scenes under 10% covered | 11 | **5** |

The effect path is now better than no effects at all, which is the bar worth
holding it to. The scenes that remain low are ones whose layers are mostly
particles, puppets or SceneScript-driven text rather than effect failures.

### Bugs the first run exposed

Two bugs came out of it, both invisible to the isolated pass test: uniforms were
being written **without binding the program** (a `glUniform*` call goes to the
currently bound program, so every value silently stayed at its default), and an
effect pass **leaves no VAO bound**, which made every later draw - the layer
composite, every puppet, every particle - render nothing and left the frame one
flat colour. The renderer now restores program, VAO, blending and depth state
after a chain.

**4. Text (18/75) and sound (41/75).** Sound is common but not visual — silence
is a much smaller defect than a missing layer, so it ranks below anything that
affects the image.

**5. Light.** 2/75. Only for completeness.

## User properties — the settings

A wallpaper's settings are declared, never invented: the creator lists them in
`general.properties` (in `project.json`, and inside the package's `scene.json`),
and they are what makes two installs of one wallpaper differ. Nine types cover
the whole library — `bool`, `slider`, `color`, `combo`, `textinput`,
`texture`/`scenetexture`, `usershortcut`, plus the layout-only `group` and
`text` — and an unknown type is preserved rather than dropped, so a newer engine
version degrades to "shown, value passed through".

Measured with `cargo run -p hyprwpe-core --example validate_properties`:
**597 properties across all 95 items** (570 editable — bool 218, color 147,
slider 137, combo 33, textinput 22, group 16, text 11, texture 4, and 9 with no
type), of which **373 are wired to a field** in their scene. The validator also
reports the **425 SceneScript bindings**, which is the honest limit: those fields
are driven by JavaScript the engine runs and hyprwpe does not, so `hyprwpe
properties` says so instead of letting a setting look broken.

The binding is **generic** — any field may carry one, so there is no per-field
table:

```jsonc
"alpha":  {"user":"bladesopacity","value":1.0}                  // mirrors the property
"visible":{"user":{"name":"eye","condition":"1"},"value":true}  // shown while it equals "1"
"origin": {"script":"…js…","value":"0 0 0"}                     // SceneScript: counted, skipped
```

Conditions **gate** rather than invert, and that is read from the data: the
library authors `clocklocation` as six objects, `true` for option 1 and `false`
for options 2..5, and `timeofday` as ten each for options 3 and 4. Inverting on a
non-match — the first implementation — would put every clock location on screen
at once.

End to end: `State::set_property` validates a value against the wallpaper's own
declaration and returns what actually stuck (a `99` on a `0..1` slider is stored
as `1`), merging rather than replacing so setting one value does not reset the
rest. `WallpaperSpec` carries the resolved set so the render loop never reads
disk. Changed values are visible as pixels through the shared transform path:

```
scenecompose <2387296214/scene.pkg> out.png 1920 1080 fill 4 --set crtfilter=false
```

drops the frame from 12 layers to 11 and changes **98.92%** of pixels (mean luma
32.97 → 16.13); `2441947759`'s `flygononoff` changes 0.207%.

## Honesty rule

Strict coverage is the metric that matters, but partial rendering is still worth
shipping: a wallpaper missing one effect usually still looks close to right. The
catalog must then say so.

A wallpaper hyprwpe cannot render fully is **reported as partially supported**,
naming what is missing. It is never rendered wrongly and left for the user to
notice, and it is never silently refused.
