# Wallpaper Engine formats — provenance

This file is the evidence that hyprwpe's format support was derived
independently. Every field hyprwpe parses is recorded here together with **how it
was determined**, before or as the parsing code is written — never reconstructed
afterwards, which would defeat the purpose.

See *Clean-room policy* in [`ARCHITECTURE.md`](ARCHITECTURE.md) for the rules
this file exists to demonstrate.

## Hard rules

Four rules. Breaking any one of them is what would turn a low-risk project into a
real problem, and all four are easy to break by accident.

**1. Never commit Wallpaper Engine content.** No `.pkg`, no `.tex`, no
`project.json`, no `preview.jpg`, not even one small file "just for a test". Those
are other people's copyrighted work, and committing one turns hyprwpe from
something that *reads* content the user owns into something that *distributes*
content it does not. This is the single most likely way this project gets a
takedown, and the easiest mistake to make while writing unit tests.

`.gitignore` blocks these extensions, but `git add -f` defeats it, so the rule
matters more than the safeguard.

**2. Test fixtures are authored by us.** Write a minimal container with our own
tooling and our own images. A hand-built 3-entry `.pkg` is a better regression
test than a real 89 MB wallpaper anyway.

**3. Never read a GPL implementation of these formats.** Not
`linux-wallpaperengine`, not any fork of it, not "just to check one struct". This
is what keeps hyprwpe MIT-licensed, and it cannot be undone once done. Running
such a tool and studying its *output* is fine; opening its source is not.

**4. No trademarks.** hyprwpe is not "Wallpaper Engine for Linux". Do not use
Wallpaper Engine or Valve branding, logos, or names that imply endorsement.
Describing what the software reads — "Wallpaper Engine wallpapers" — is
nominative fair use and is fine.

### Why this is otherwise low risk

File formats are not copyrightable; copyright protects the expression of code,
not the layout of bytes. Reverse-engineering a format for interoperability has
settled support — *Sega v. Accolade* and *Sony v. Connectix* in the US, and
Article 6 of the EU Software Directive, which also voids contractual bans on it.

Anti-circumvention law (DMCA §1201 and equivalents) does not apply here because
nothing is being circumvented: `scene.pkg` is an unencrypted container with a
plaintext `PKGV0001` magic and readable entry names.

None of this is legal advice, and it is not a guarantee. It is the reasoning the
project's boundaries were drawn from, recorded so they can be checked.

## How to record a finding

Add a row when you parse a new field. Keep it short, but make the source
checkable by someone else.

| Source tag | Means |
| --- | --- |
| `bytes` | Read directly from a file, with the offset and the sample noted |
| `experiment` | Changed an input, observed the output, inferred the meaning |
| `public-doc` | A published spec, wiki page, or write-up — link it |
| `tool-output` | Observed the output of a tool run against our own files |

`tool-output` means the tool was **run**, not read. Running a program is not a
derivative work regardless of its licence; reading a GPL implementation's source
is what the policy forbids.

Never `read-source` on a GPL implementation. There is no tag for it because it
must not happen.

## Sample corpus

Findings below were checked against a personal library of 75 `scene.pkg` files.
The library holds 95 Workshop items in total: 76 `scene`, 10 `video` and 9
`web`. 75 of the 76 scene items carry a `scene.pkg`; the odd one out declares
`type: scene` with no package, which is why the catalog reports declared type
and rendering support separately.

The files themselves are not in this repository and never will be; only the
conclusions are.

## `scene.pkg` container — confirmed

All integers are little-endian `u32`. Strings are length-prefixed and not
NUL-terminated.

```
u32    version_len
char[] version                  e.g. "PKGV0001"
u32    entry_count
entry_count ×
    u32    name_len
    char[] name                 e.g. "scene.json", "materials/x.json"
    u32    offset               relative to the end of the entry table
    u32    size
<blob>                          entry data, addressed by offset + size
```

| Field | Source | Note |
| --- | --- | --- |
| `version_len` | `bytes` | `08` at 0x00, matching the 8-byte version that follows |
| `version` | `bytes` | `PKGV####`; 20 distinct values seen, see below |
| `entry_count` | `experiment` | Read as a count, then verified structurally |
| `name_len`, `name` | `bytes` | Readable ASCII paths immediately follow each length |
| `offset` | `experiment` | Relative to the end of the table, not to file start |
| `size` | `experiment` | |

**How it was verified.** If the layout is right, the table must end exactly where
the data begins, so `table_end + max(offset + size)` must equal the file size
precisely. It does, for all 75 files — an off-by-one anywhere would break the
identity. As a second check, the entry named `scene.json` was extracted from
every file and parsed as JSON: 75 of 75 succeeded.

**Version variants.** The version string varies (`PKGV0001` … `PKGV0023`, 20
distinct values in the corpus) but the entry-table layout above parses all of
them. Whether later versions change the *contents* is a separate question; the
container does not appear to differ.

## Package contents

Extension counts across the corpus, useful for knowing what a renderer must
eventually handle:

| Extension | Count | |
| --- | --- | --- |
| `.json` | 2269 | scene, materials, effects, particle definitions |
| `.tex` | 997 | textures |
| `.vert` / `.frag` | 468 each | shader pairs |
| `.mdl` | 48 | models |
| `.mp3` / `.ogg` / `.wav` | 90 | audio |
| `.otf` / `.ttf` | 18 | fonts |

**Effects are self-contained.** Every one of 1171 effect references in the corpus
resolves to a file *inside its own package* — none point at a shared Wallpaper
Engine effect library. A renderer therefore never needs Wallpaper Engine's
built-in effects: each wallpaper carries the effect definitions and shaders it
uses. This materially reduces the scope of the scene renderer.

## `scene.json`

Top-level keys, with the number of files containing each:

| Key | Files | |
| --- | --- | --- |
| `camera` | 75/75 | |
| `general` | 75/75 | |
| `objects` | 75/75 | the scene graph |
| `version` | 57/75 | absent in older packages |

Objects per scene: 1 minimum, 8 median, 271 maximum; 1450 in total.

An object's kind is implied by which key it carries — `image`, `particle`,
`sound`, `text` or `light` — rather than by a type field. Common transform and
appearance keys (`origin`, `scale`, `angles`, `visible`, `parallaxDepth`,
`alpha`, `color`, `effects`, `parent`) are shared across kinds.

### User properties and value bindings — confirmed

Beyond the geometry, `general.properties` declares the settings the creator
exposes (`project.json` carries the panel metadata; the packed `scene.json`
carries them too). Nine `type` values cover the whole library — `bool`,
`slider`, `color`, `combo`, `textinput`, `texture`/`scenetexture`,
`usershortcut`, and the layout-only `group` and `text` — with `min`/`max`/`step`/
`precision`/`fraction` on a slider, `options[{label,value}]` on a combo, and
`text`/`order`/`index` for the panel. 597 declarations across 95 items.

The mechanism that makes them apply is a **binding that may replace any field's
value** — not a fixed set of bindable keys, which is why a renderer must resolve
them generically rather than field by field:

```jsonc
// plain: the field mirrors the property
"alpha":  {"user": "bladesopacity", "value": 1.0}

// combo: the field is shown only while the property equals `condition`
"visible": {"user": {"name": "clocklocation", "condition": "1"}, "value": true}

// SceneScript: per-frame JavaScript. The static `value` is what a host that
// does not run JS can use; 425 of these exist in the library.
"origin": {"script": "'use strict';…", "value": "0 0 0"}
```

A `condition` **gates**; it does not invert. That is settled by the data, not by
preference: `clocklocation` binds six objects with `true` for option 1 and
`false` for options 2..5, and `timeofday` binds ten for option 3 (all `true`)
and ten for option 4 (all `true`). Reading a non-match as an inversion would
display every clock location simultaneously.

Saved user values are keyed by the wallpaper and hold `{key: {"value": v}}`,
the same wrapping the declarations use.

### Transform semantics (validated against the corpus)

- `general.orthogonalprojection {width, height}` names the **design canvas**,
  and object coordinates live in that space: `(0, 0)` is the canvas'
  **bottom-left** corner and **+Y points up** (proved by scenes whose sky sits
  at high Y and whose ground debris sits at negative Y).
- An object's `origin` is the **centre of its quad** — proved by full-screen
  objects that carry `size == canvas` and `origin == canvas centre`.
- The quad spans `size * scale` design units. `size` is the quad's own extent,
  not the texture's; for the 473 objects that rely on model `autosize` the two
  agree in every observed case.
- `parent` references another object's `id`; **405 of 1450 objects (28%)** use
  it, and a child's transform is composed with its ancestors' — a child
  `origin` is relative to the parent, in the parent's scaled and rotated frame.
- The canvas is mapped onto the output with the output's scaling mode. The
  **orthographic window is the design-space rectangle the output covers, not the
  canvas' extent**: with `Fill` on a mismatched aspect the canvas overflows the
  output, so only its visible middle is drawn and the viewport crops it.
  Projecting the canvas' own extent instead leaves the overflow margin unpainted
  and `clearcolor` shows through as bands. This was isolated by rendering a
  synthetic scene (a canvas-sized background, red `clearcolor`) on a 16:10
  output: a whole-canvas window left **29% of the frame bare**, the output-sized
  window leaves none. `tools/mkscene.py` builds that scene.

## Property animation

Six of the 75 local scenes animate an object *property* rather than the mesh.
An animatable property may be a bare value (`"alpha": 1`) or an object carrying
both a base value and its keyframes:

```json
"origin": { "value": "400 400 0", "animation": {
    "c0": [ {"frame": 0, "value": 400}, {"frame": 60, "value": 3400} ],
    "c1": [ {"frame": 0, "value": 400}, {"frame": 60, "value": 1800} ],
    "options": { "fps": 30, "length": 120, "mode": "loop" }
} }
```

`c0`/`c1`/`c2` are one track per component (scalars use `c0` only, vectors use
three). `options.mode` is `single` (hold the last value), `loop` (wrap) or
`mirror` (ping-pong); timing is `frame / fps` with linear interpolation between
keys, and `options.length` defaults to the last keyframe. Animatable properties
seen in the corpus: `origin` (10 objects), `alpha` (10), `angles` (4), `scale`
(4), `light.intensity` (1).

`startpaused` animations (a media-play icon, say) only advance when a scene
*script* triggers them; hyprwpe has no scene scripting, so it holds the first
frame rather than inventing motion.

**Puppet models.** A `models/*.json` may name a `"puppet": "…_puppet.mdl"`
next to its `material`; 12 of 75 scenes reference one. Those are
**versioned, self-describing** model files.

The corpus (48 `.mdl` across 12 scenes) holds **five format versions**:

| Version | Files | Sections |
| --- | --- | --- |
| `MDLV0013` | 19 | `MDLS0001`, `MDLA0001` |
| `MDLV0014` | 3 | `MDLS0002`, `MDLA0002` |
| `MDLV0016` | 11 | `MDLS0002`, `MDLA0003` |
| `MDLV0017` | 1 | `MDLS0002`, `MDAT0001`, `MDLA0004` |
| `MDLV0023` | 14 | `MDLS0004`, `MDAT0001`, `MDLA0006` |

The first eight bytes are the version tag; the file then holds
`MDLS####` (skeleton / control points), an optional `MDAT####`
(attachments), `MDLA####` (animation), an optional `MDLE####` (end), and the
mesh. **A parser must be version-agnostic**: locating sections by their tags
gives the expected order `MDLV → MDLS → [MDAT] → MDLA → [MDLE]` for **48/48
files**, and each section's own numeric suffix (`0001`…`0006`) says how to read
it — so support for a new version is a table entry, never a per-wallpaper
branch. Reading one specific file's offsets would cover 14/48 at best.

### MDLV sections — confirmed

**Section framing** (every `MD??NNNN` section, all five versions):

```
char[8]  tag                e.g. "MDLS0004"; the last four digits are the revision
u8       0
u32      next               absolute offset of the following section
u16      count              element count for this section
u16      0                  purpose unknown; zero in every file seen
```

A section ends where the next one begins — taken from the file, never assumed.
The final section's `next` is `filesize - 1`, so the last byte belongs to nothing.

**Mesh.** Not a tagged section, so it is located by validating invariants rather
than by offset: after the material path, the first candidate whose `u32
vertexBytes` divides by the record count, whose record stride divides
`vertexBytes` exactly, and whose position triple **and** trailing UV pair both
produce consistent triangle winding. Then:

```
u32 vertexBytes             records follow, tightly packed
u32 indexBytes              u16 (or u32) indices follow
stride = vertexBytes / (max index + 1)        20 / 52 / 80 bytes observed
```

**Vertex record** (`nf = stride / 4` floats):

| Slots | Meaning |
| --- | --- |
| `f[0..2]` | position xyz |
| `f[3..8]`, `f[9]` | stride-80 only: normal, tangent (both unit), one constant `1.0` |
| `f[nf-10..nf-6]` | **4 bone indices**, stored as raw `u32` bit patterns |
| `f[nf-6..nf-2]` | **4 skin weights** (`f32`), summing to 1.0 |
| `f[nf-2..nf-1]` | UV |

The skin block is **two contiguous four-slot blocks immediately before the UV
pair**, so a record too short for them (5 floats: position + UV only) simply has
none. A zero weight means the slot is unused and its index is meaningless. This
holds for **47/47 skinned corpus files, ~40k vertices, zero violations** of
"weights sum to 1.0" and "every used index is below the bone count".

**Skeleton** (`MDLS`): `count` bones, each

```
u8[5]  lead                 fixed per bone (0x00 01 00 00 00 in MDLS0004)
i32    parent               -1 for a root
u32    blockBytes           size of the matrix block (64 observed)
f32[16] matrix              bind transform, row-major, translation in the last row
zstring params
```

followed by an undecoded per-vertex array whose length is `section_end - pos`.

**Attachments** (`MDAT`): `u16 kind`, `zstring name`, `f32[16]` matrix.

**Animation** (`MDLA`): per animation `u32, u32, zstring name, zstring mode,
f32 fps, u32 length, u32, u32 bone_count`; then per bone `u32, u32 bytes` of
keyframes; then a short trailer whose length is recovered from the file (the
value that makes the list end exactly on the section end).

**Keyframe** (36 bytes): `f32 position.xyz`, `u32`, `u32`, `f32 rotation`,
`f32 scale.xyz`. `keyframe_count == length + 1` held for **674/674 corpus
tracks**.

### Puppet skinning — confirmed

Two facts make skinning exact, both verified from the bytes and geometry:

1. **A bone's stored matrix is its local transform**, relative to its parent.
   Composing along the parent chain gives the rest world matrix. Verified
   geometrically: composed, each vertex's dominant bone lands about **3x** closer
   to it than if the stored matrix were read as absolute (Akali mean distance
   199 vs 659 model units).
2. **An animation keyframe is the same local transform**, and frame 0 *usually*
   equals the bind pose (Butterfly matches to 7.8e-9). This is a convention, not
   a guarantee — `Car_puppet` authors a first key away from rest — so the pose is
   always taken from the keyframes, never from the authored mesh.

Then, per bone, `skin = animated_world * inverse(rest_world)` and each vertex is
the weighted blend of its influences (linear blend skinning). Validated offline
by `cargo run -p hyprwpe-render --example skinpreview -- --corpus <dir>`:
**46 skinned models, 0 failures** — every pose across every animation stays
finite and bounded. Akali's loop returns exactly to rest at `t = length / fps`
(drift 0.0), and its frame-0 offset is 0.0.

The mesh replaces the quad and is drawn `translucent` + `nocull` with depth test
off; a model may carry a `cropoffset`. `tools/mdl_corpus.py` extracts every
`.mdl` for cross-version testing; `tools/extract_pkg.py` pulls a single entry
from a package.

## `.tex` textures — confirmed

All integers are little-endian `u32`.

### Container layout
Can be prefixed with a 4-byte length (`0x08, 0x00, 0x00, 0x00`) or appear bare:

```
[optional u32 len]
char[8] magic                   "TEXV0001" or "TEXB0001"
u32    format_id                texture format (RGBA8, DXT1, DXT3, DXT5, etc.)
u32    width                    image width in pixels
u32    height                   image height in pixels
[optional u32 extra]            mipmap / flags
<blob>                          pixel / block data
```

| Field | Source | Note |
| --- | --- | --- |
| `magic` | `bytes` | `TEXV####` or `TEXB####` |
| `format_id` | `experiment` | Standard format enumeration matching DXGI / D3D constants |
| `width`, `height` | `bytes` | Positive dimensions matching layer aspect ratios |
| `data` | `bytes` | Raw RGBA8 or BC1/BC2/BC3 blocks |

### `TEXV0005` container — confirmed

Modern scenes ship every `.tex` as `TEXV0005` (997/997 in the corpus). The
layout differs from the legacy direct `.tex` above:

```
char[8]  magic                 "TEXV0005"
char[1]  0x00
char[8]  info magic            "TEXI0001"
u32      flags                 usually 0
u32      ?                     often 512
u32      width  (16.16 fixed)  width  * 256
u32      height (16.16 fixed)  height * 256
u32      ?                     often width  * 256 again
u32      ?                     often height * 256 again
char[8]  payload magic         "TEXB0003" / "TEXB0004" / "TEXB0002"
...
```

The `TEXB####` payload block's revision selects the encoding:

| Revision | Count | Payload |
| --- | --- | --- |
| `TEXB0002` | 3 | **LZ4 block** of pixel data |
| `TEXB0003` | 812 | **LZ4 block** of pixel data |
| `TEXB0004` | 100 | **LZ4 block**; one header word longer again |

A file may additionally embed a PNG/JPEG, but that is not what the revision
means: an embedded image is recognised by its signature and takes precedence
when it decodes.

**Payload layout**, measured on files where the decoded LZ4 block length equals
the declared size exactly (offsets from the `TEXB####` magic):

```text
TEXB0002                  TEXB0003                  TEXB0004
+9   u32 format           +9   u32 format           +9   u32 format
+13  u32 flags            +13  u32 flags            +13  u32 flags
+17  u32 width  = blk-20  +17  u32 reserved         +17  u32 reserved
+21  u32 height = blk-16  +21  u32 width  = blk-20  +21  u32 reserved
+25  u32 reserved         +25  u32 height = blk-16  +25  u32 width  = blk-20
+29  u32 uncomp = blk- 8  +29  u32 reserved         +29  u32 height = blk-16
+33  u32 comp   = blk- 4  +33  u32 uncomp = blk- 8  +33  u32 reserved
+37  ..  LZ4 block        +37  u32 comp   = blk- 4  +37  u32 uncomp = blk- 8
                         +41  ..  LZ4 block        +41  u32 comp   = blk- 4
                                                   +45  ..  LZ4 block
```

Every revision's header is one word longer than the one before it, and the block
moves with it - so the block offset is taken from the revision while the field
offsets are stated *relative to the block*, which is the form that holds across
all three. A mip chain may follow the block; only the first level is decoded, so
the block length, not the remaining bytes, is the decoder's input.

**The format is not a field.** The block never names its pixel format; it is
identified by the declared uncompressed size, which must equal what a format
occupies at those dimensions. Block formats are tried first because that is what
these payloads are: a grayscale mask whose dimensions are both multiples of four
is `DXT5`, whose 16 bytes per 4x4 block equal `width * height` - exactly the byte
count `R8` would have. Decoding such a mask as `R8` renders noise instead of a
smooth mask, verified by measuring chroma both ways (0.0-1.1 out of 255 as
`DXT5`, i.e. grayscale).

`compressed == uncompressed` means the block was stored as-is rather than
compressed; small masks and phase textures are written that way, and decoding
such a block as LZ4 fails on an invalid header.

**How it is read for rendering.** An embedded PNG/JPEG, when present and
decodable, supplies both dimensions and pixels. Otherwise the payload block is
read by the layout above, LZ4-decoded (or taken raw), and the bytes are expanded
with the DXT1/3/5 decoders or used directly for `R8`/`Rg8`/`Rgb8`/`Rgba8`.

Measured over 915 unique textures from 75 packages: **all 915 decode**, by
revision 3, 2 and 4 in full.

Dimensions are never invented: when no payload layout matches, the texture is
reported as an error. An earlier fallback took the first plausible pair of header
words, which produced textures claiming to be `512x25600` from a 10 KB file - and
made a real 39% decode rate read as 88%, which is the failure mode a plausible
number hides.

### Engine asset textures are `TEXB` + **LZ4** — confirmed

The textures Wallpaper Engine ships in its own `assets/` directory (what particles
reference) do **not** follow the container above, and this cost real time:

```
"TEXB0003" / "TEXB0004"
u32  flags
u32  format                       (same enumeration as elsewhere)
u32  width   (16.16 fixed, >> 8)
u32  height  (16.16 fixed, >> 8)
u32  ?                            usually width  * 256 again
u32  ?                            usually height * 256 again
u32  mipcount                     (at +16 from the magic)
...   per-mip table
LZ4   block payload               starts at magic + 41
```

Three details, each of which made a first attempt fail:

- **The sizes are 16.16 fixed point**, exactly like `TEXV0005`. Reading them raw
  gives a width 256× too large, and the DXT decode then fails on a size mismatch.
- **The payload is an LZ4 block**, not zlib, zstd-frame or lzma. For
  `materials/particle/halo_*.tex` the LZ4 block begins at `magic + 41` and
  consumes the remainder of the file exactly.
- **`TEXB0003` is single-level; `TEXB0004` carries `mipcount` levels** with a
  per-level table, so a decoder that assumes one level reads mip data as pixels.

Verified against the engine's own assets: decoded, `halo` is the glow sprite it
should be (centre `ffffffeb`, corners alpha 0, mean alpha 38.8). `lz4_flex` does
the block decode and is the crate's only new dependency for this.

### Engine assets — resolved at runtime, never redistributed

Wallpaper Engine's installation carries what a wallpaper legitimately expects to
exist but does not ship itself: `assets/shaders/**` (including the `common*.h`
preludes every effect shader includes), `assets/materials/particle/**` (108 of
the 130 particle textures the corpus references), `assets/particles/presets/**`
and `assets/fonts/**` (for text objects). `crates/core/src/assets.rs` resolves a
name **package first, then the engine's `assets/` directory**, so nothing is
redistributed with hyprwpe and a machine without Wallpaper Engine installed loses
only those sprites and fonts rather than failing to render.

### Materials reference textures relative to `materials/`

The `textures` array inside a material (`passes[].textures` or a top-level
`textures`) holds **paths relative to the package `materials/` directory**, with
no leading `materials/` and, for the dominant form, no extension:

| Material | `textures[0]` | Resolved entry |
| --- | --- | --- |
| `materials/akalibackground2.json` | `akalibackground2` | `materials/akalibackground2.tex` |
| `materials/workshop/3518164866/背景2.json` | `workshop/3518164866/背景2` | `materials/workshop/3518164866/背景2.tex` |
| `materials/sky.json` | `grid.png` | `materials/grid.png` |

Resolution (verified across 75 packages): try `materials/{name}` with each
candidate extension (`.tex`, `.png`, `.jpg`, `.jpeg`, `.tga`, `.bmp`); a name
that already carries an extension is used verbatim, rooted at `materials/`.
This single rule correctly resolves 475 references on the corpus (the 247 that
fail are particle/preset textures that reference an unexported runtime asset,
not an image the renderer needs).

| Field | Source | Note |
| --- | --- | --- |
| `textures[]` root | `bytes` | Bare names resolve under `materials/`, corpus-verified |
| extension candidates | `experiment` | `.tex` first, then common image formats |

### Format Enumeration
- `0`, `1`, `28`: RGBA8 (uncompressed 32-bit `width * height * 4` bytes)
- `2`, `29`: RGB8 (uncompressed 24-bit)
- `3`, `61`: R8 (single-channel 8-bit)
- `4`, `10`, `71`: DXT1 / BC1 (8 bytes per 4x4 block, 1-bit alpha)
- `5`, `11`, `74`: DXT3 / BC2 (16 bytes per 4x4 block, explicit 4-bit alpha)
- `6`, `12`, `77`: DXT5 / BC3 (16 bytes per 4x4 block, interpolated 8-bit alpha)

All compressed formats are decompressed into 32-bit RGBA8888 for universal OpenGL ES 3.0 compatibility.

## Video Wallpapers — confirmed

Workshop items with `type: video` declare a relative path to a video container in `project.json` (under `"file"`):
- Containers observed: `.mp4` (H.264 / AVC, AAC audio), `.webm` (VP8 / VP9), `.mkv`.
- Playback engine: `libmpv` loaded dynamically via `dlopen`.
- Options configured for wallpaper use:
  - `vo`: `"libmpv"` (uses embedded OpenGL render context; avoids external VO threads that crash on TLS dispatch tables).
  - `loop-file`: `"inf"` (seamless hardware looping).
  - `audio`: `"no"` (silent background rendering).
  - `cache`: `"no"`, `demuxer-max-bytes`: `"8MiB"`, `demuxer-max-back-bytes`: `"0"` (bounds memory footprint for looping short clips).
- Aspect ratio and scaling mapping:
  - `Fill`: `keepaspect=yes`, `panscan=1.0`, `video-unscaled=no`
  - `Fit`: `keepaspect=yes`, `panscan=0.0`, `video-unscaled=no`
  - `Stretch`: `keepaspect=no`, `panscan=0.0`, `video-unscaled=no`
  - `Center`: `keepaspect=yes`, `panscan=0.0`, `video-unscaled=yes`

## Shader dialect — confirmed
Standalone GLSL fragment shaders (`.glsl`, `.frag`) support standard Shadertoy uniforms:
`iResolution`, `iTime`, `iTimeDelta`, `iFrameRate`, `iFrame`, `iMouse`, `iDate`, and `#define texture2D texture`.
The preprocessor auto-injects `#version 300 es`, precision qualifiers, and wraps `mainImage` into `main()`.

## Web Wallpapers — intentionally unsupported
Workshop items with `type: web` contain an `index.html` file designed to be run in Chromium/CEF.
- Observed count: 9/95 in reference library.
- Status: Flagged as `(unsupported)` in the catalog.
- Rationale: Embedding a full browser engine (Chromium/WebKit) incurs hundreds of megabytes of RAM and massive binary bloat, violating the core lightweight daemon invariant. Users are informed directly in the catalog rather than encountering silent failure.

