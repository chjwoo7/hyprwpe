# Wallpaper Engine designer reference (for the scene renderer)

The official designer documentation describes **what the editor lets a creator
author** and **what those features mean**. That is the specification side of the
format: it tells us which semantics a runtime must implement, without reading any
runtime's source. Use it to decide *what* to build; use `docs/FORMATS.md` for the
byte layout we measured ourselves.

Source: <https://docs.wallpaperengine.io/en/scene/overview.html>

## How a wallpaper comes to exist

A creator never writes the binary format. The **Wallpaper Engine editor** is the
authoring tool, and it exports a fixed, engine-defined package: `project.json`
(which declares the `type`) plus assets. The runtime replays that package. The
common factor across every creator's wallpaper is therefore *the format itself* —
one correct implementation of the format covers all of them. This is why the
parser must be driven by the file's own versions and counts, never by a specific
wallpaper's offsets.

## Wallpaper types

Measured across the local 95-item library:

| `project.json` `type` | Items | Notes |
| --- | --- | --- |
| `scene` / `Scene` | 76 | editor-authored, the large majority |
| `video` | 10 | a pre-rendered `.mp4` |
| `web` / `Web` | 9 | an `index.html` web wallpaper |

The capitalisation is inconsistent between items (`Scene` vs `scene`), so the type
must be compared case-insensitively (`catalog::Kind::from_project_type` does).

A plain static image is authored through the editor and packed as a **scene**
with a single image layer, so "static" is not a separate packed type here. WE
also has an application (`.exe`) type, absent from this library.

## The scene data model (what the editor can author)

- **Assets**: image layers, text layers, particle systems, sounds, 3D models
  (puppets), light shafts, effects. This matches the object kinds we already
  parse (image / particle / text / sound / light).
- **Transforms**: each asset has `Origin` (position), `Angle` (rotation) and
  `Scale` (size) — exactly the `origin`/`angles`/`scale` we render, and a negative
  X scale is how the editor flips an asset.
- **Asset hierarchy**: any asset can parent another; moving/rotating/scaling the
  parent moves the children — the `parent` chain we compose.
- **Timeline animation**: keyframed properties — the property animation we
  implemented (`crates/core/src/animation.rs`).

## Puppet warp (character animation)

Documented process: cutout → geometry (mesh) → skeleton (bones) → weights →
reference pose → animate → place in the scene. Its data model:

| Editor concept | Meaning | Corresponds to |
| --- | --- | --- |
| Geometry / mesh | triangles over the cutout | mesh section of the `.mdl` |
| Skeleton | named bones in a parent/child hierarchy | `MDLS####` |
| Weights | per-vertex bone influence, painted into textures | mesh vertex attribute(s) |
| Reference pose | the assembled starting pose | part of `MDLS` |
| Attachments | named points on a bone other assets can follow | `MDAT####` |
| Animation | per-frame values per bone, plus non-bone tracks | `MDLA####` |
| Texture channels | extra same-resolution textures blended in by an animated opacity (eye blinking) | tracks inside `MDLA` |
| Blend rules | animate a bone's parent from one bone to another | tracks inside `MDLA` |

Related pages worth knowing, not yet implemented: blend shapes, bone constraints,
inverse kinematics, clipping masks, perspective (2.5D depth), animation mixing,
extending a puppet later, interactive puppet settings.

<https://docs.wallpaperengine.io/en/scene/puppet-warp/introduction.html>
<https://docs.wallpaperengine.io/en/scene/puppet-warp/charactersheet.html>
<https://docs.wallpaperengine.io/en/scene/puppet-warp/attachments.html>
<https://docs.wallpaperengine.io/en/scene/puppet-warp/blendrules.html>
<https://docs.wallpaperengine.io/en/scene/puppet-warp/texturechannels.html>

## Consequence for the implementation

Implement the format once, driven by the version tags the file carries, and every
creator's wallpaper follows. For puppets that means: parse `MDLV` version-aware →
mesh (positions, UVs, indices, weights) → `MDLS` skeleton → `MDLA` animation →
linear-blend skinning per frame → draw with the puppet material (translucent,
double-sided). Validate against all 48 `.mdl` files in the corpus, which span five
format versions (`MDLV0013/14/16/17/23`).
