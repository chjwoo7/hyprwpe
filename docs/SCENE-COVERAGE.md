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
come: attachments (`MDAT`), the texture-channel / blend-rule tracks (parsed but not
applied), particle objects, and the effect pipeline.

**2. Particles.** 52/75 wallpapers. The largest single jump available.

**3. The effect pipeline itself**, then effects in frequency order. The pipeline
is the hard part; individual effects after it are incremental.

**4. Text (18/75) and sound (41/75).** Sound is common but not visual — silence
is a much smaller defect than a missing layer, so it ranks below anything that
affects the image.

**5. Light.** 2/75. Only for completeness.

## Honesty rule

Strict coverage is the metric that matters, but partial rendering is still worth
shipping: a wallpaper missing one effect usually still looks close to right. The
catalog must then say so.

A wallpaper hyprwpe cannot render fully is **reported as partially supported**,
naming what is missing. It is never rendered wrongly and left for the user to
notice, and it is never silently refused.
