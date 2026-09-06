# hyprwpe

Wallpaper Engine wallpapers on Hyprland — and ordinary wallpapers too, in the
same place.

> **Status: design stage.** Nothing is implemented yet. The architecture is
> written up in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); the code starts
> after it settles. Do not expect a working build from this repository yet.

## What it is

Most Linux wallpaper tools are *setters*: hand one an image, it puts it up, it
stops thinking. That model breaks down for Wallpaper Engine wallpapers, which are
renderers that run forever — holding hundreds of megabytes and burning several
percent of a core, whether or not anyone can see them.

hyprwpe treats the background as a managed resource instead. One daemon owns the
layer surfaces, knows which outputs are actually visible, and stops paying for
wallpapers nobody is looking at.

## Goals

- **Light.** Suspend the wallpaper when every output is covered; unload it while
  the session is locked or idle; fall back to a still image on battery.
- **One tool for every wallpaper.** Wallpaper Engine scenes, plain images, video
  files and GLSL shaders share one catalog, one GUI and one config.
- **One package.** Every renderer is first-party, so there is no separate
  wallpaper runtime to install and configure.
- **Fits a rice.** Designed to sit alongside quickshell-based setups (end4,
  end4-pC, dots-chjwoo) instead of fighting them for the background layer.

## Planned shape

```
hyprwpe        daemon + CLI    hyprwpe daemon | set | list | status | pause
hyprwpe-gui    GTK4 front-end
```

Written in Rust. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the
design, the measurements behind it, and the roadmap.

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
