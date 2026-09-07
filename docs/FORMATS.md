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

See [`SCENE-COVERAGE.md`](SCENE-COVERAGE.md) for how often each kind and effect
appears, which is what drives the renderer's build order.

## `.tex` textures

Not started.

## `scene.json`

Not started.

## Shader dialect

Not started. The transpile chain is intended to be
`our preprocessor → glslang/shaderc → spirv-cross`, so what is documented here is
Wallpaper Engine's own conventions — macros, includes, uniform naming — rather
than HLSL itself.
