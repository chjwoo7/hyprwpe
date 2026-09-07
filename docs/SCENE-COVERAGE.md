# Scene renderer — coverage and build order

The scene renderer is the largest piece of work in hyprwpe and the one with no
obvious finish line. This document replaces guesswork about that "long tail" with
a measurement, so the build order follows evidence rather than instinct.

Numbers come from a personal library of **75 scene wallpapers, 1450 objects**.
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
build the object tree, draw image layers with transforms. 6 wallpapers render
correctly on this alone (image-only, no effects), and all 75 render *something*
if unsupported features are skipped.

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
