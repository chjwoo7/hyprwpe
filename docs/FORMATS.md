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

See [`SCENE-COVERAGE.md`](SCENE-COVERAGE.md) for how often each kind and effect
appears, which is what drives the renderer's build order.

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
| `TEXB0003` | 891 | Embedded **PNG or JPEG** image (dominant case) |
| `TEXB0004` | 103 | Embedded PNG/JPEG |
| `TEXB0002` | 3 | Raw block-compressed texture data |

| Field | Source | Note |
| --- | --- | --- |
| `TEXV0005` magic | `bytes` | `54 45 58 56 30 30 30 35` at 0x00 |
| `TEXI0001` info | `bytes` | Fixed-width sub-block; carries width/height as 16.16 |
| `width`, `height` | `experiment` | Stored `value * 256`; 1920x1080 stores `0x1E0000` |
| embedded PNG/JPEG | `bytes` | `\x89PNG` / `\xff\xd8\xff` signature inside the payload block |
| `TEXB0003/4/2` | `bytes` | Revision byte at offset `magic+6` |

**How it is read for rendering.** When a `.tex` embeds a PNG/JPEG, the image
is decoded directly with the `image` crate (dimensions and pixels both come
from the embedded image, so the 16.16 header is informational). Raw-BC payloads
(`TEXB0002`) are decompressed with the built-in DXT1/3/5 decoders, matching the
declared dimensions.

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

