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

## Sample files

Findings should cite a sample so they can be re-checked.

| Ref | File | Notes |
| --- | --- | --- |
| — | — | Populate as work starts |

## `scene.pkg` container

Magic `PKGV0001` observed at offset 0x04 of a sample scene package, preceded by a
4-byte little-endian value of `08` — consistent with a length-prefixed version
string. Entries follow as length-prefixed names with offset and size fields.

| Offset | Field | Type | Meaning | Source |
| --- | --- | --- | --- | --- |
| 0x00 | version_len | u32 le | `08`, length of the version string | `bytes` |
| 0x04 | version | char[8] | `PKGV0001` | `bytes` |
| 0x0C | ... | | Entry table — to be confirmed | |

Everything below the version string is provisional until parsed properly and
re-verified against several packages.

## `.tex` textures

Not started.

## `scene.json`

Not started.

## Shader dialect

Not started. The transpile chain is intended to be
`our preprocessor → glslang/shaderc → spirv-cross`, so what is documented here is
Wallpaper Engine's own conventions — macros, includes, uniform naming — rather
than HLSL itself.
