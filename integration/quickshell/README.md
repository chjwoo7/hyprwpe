# Quickshell Integration (`dots-chjwoo` / `end4` family)

This integration allows Quickshell desktop shells (`end4`, `end4-pC`, `dots-chjwoo`) to yield the desktop background surface to `hyprwpe` when the daemon is running, and automatically restore the shell's wallpaper if `hyprwpe` exits or terminates.

## Design

`hyprwpe` runs on Wayland layer-shell `Background` layer (`zwlr_layer_shell_v1::layer::background`).
Quickshell's `Background.qml` typically paints its wallpaper on the same or `Bottom` layer. If Quickshell paints an opaque background image, it completely covers the `hyprwpe` animated surface underneath.

Instead of hardcoded patches that break across upstream updates, integration uses a single declarative socket detection:

```jsonc
// In Quickshell config (e.g. ~/.config/quickshell/config.jsonc)
"background": {
  "externalWallpaperSocket": "$XDG_RUNTIME_DIR/hyprwpe.sock"
}
```

### Handover and Exit Invariant

The shell derives its active state strictly from whether the socket exists on disk:
- When `hyprwpe daemon` is running: the socket exists → Quickshell hides its background image.
- When `hyprwpe` stops, exits, or crashes: the socket vanishes → Quickshell immediately renders its wallpaper again.
- No blank screens, no stuck states, zero manual recovery needed.

## Layer Stacking & Widget Transparency

Hyprland respects Wayland layer-shell protocol ordering:
1. `Background` (lowest) -> **`hyprwpe`**
2. `Bottom` -> **Quickshell desktop widgets** (clock, system stats, workspace overview)
3. `Top` -> Status bar, notifications
4. `Overlay` (highest) -> App launcher, lock screen

By making Quickshell's background `Image` element invisible when `hyprwpe`'s socket is present, Quickshell does not allocate an opaque buffer on `Background`. As a result, all Quickshell desktop widgets continue floating cleanly on top of live `hyprwpe` videos, shaders, and 2D scenes with full alpha transparency.

## Applying the Integration

### Option A: Using the Patch on `Background.qml`

Apply `patch-background.diff` to your Quickshell configuration:

```bash
cd ~/.config/quickshell
patch -p1 < /path/to/hyprwpe/integration/quickshell/patch-background.diff
```

### Option B: Reusable `ExternalWallpaperDetector.qml`

Include `ExternalWallpaperDetector.qml` in your shell components:

```qml
import QtQuick
import Quickshell
import Quickshell.Io

Item {
    id: root

    ExternalWallpaperDetector {
        id: detector
    }

    Image {
        anchors.fill: parent
        visible: !detector.externalActive
        source: Config.background.wallpaperPath
    }
}
```
