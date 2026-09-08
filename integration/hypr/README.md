# Hyprland Integration

`hyprwpe` runs as a native Wayland layer-shell background service on Hyprland, attaching directly to the `background` layer (`zwlr_layer_shell_v1::layer::background`).

## Autostart

Add the daemon to your `~/.config/hypr/hyprland.conf`:

```ini
exec-once = hyprwpe daemon
```

Or source the bundled configuration snippet:

```ini
source = ~/.config/hypr/hyprwpe.conf
```

## Recommended Keybindings

Add keybindings in `hyprland.conf` to open the GUI picker or control playback:

```ini
# Open GTK4 graphical wallpaper picker (Super + W)
bind = $mainMod, W, exec, hyprwpe-gui

# Manual pause/resume toggles for gaming or heavy workloads
bind = $mainMod SHIFT, W, exec, hyprwpe pause
bind = $mainMod CTRL, W, exec, hyprwpe resume
```

## Multi-Monitor Configuration

`hyprwpe` natively discovers all outputs registered with Hyprland (`hyprctl monitors`). You can assign wallpapers globally or per output:

```bash
# Global (all monitors):
hyprwpe set 3132826255

# Independent per-output:
hyprwpe set --output eDP-1 1810612745 --scaling fill
hyprwpe set --output DP-2 examples/rainbow.frag --scaling fill
```

Monitor hotplugging is supported automatically: when a monitor is plugged in or unplugged, `hyprwpe` reconciles surfaces without crashing or restarting the daemon.

## Occlusion & Power Management

`hyprwpe` connects directly to Hyprland's IPC `socket2` (located at `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`) and listens for compositor events:

1. **Active Workspaces & Monitors (`workspace`, `focusedmon`):** Tracks which workspace is active on each output.
2. **Fullscreen Occlusion (`fullscreen`):** When a window enters fullscreen mode, a 1.5-second hysteresis timer arms. If the screen remains occluded, render loops suspend (0% CPU/GPU). Leaving fullscreen immediately resumes playback with zero latency.
3. **Screen Lock (`lock`):** When `hyprlock` or any session lock activates, renderers suspend immediately.
4. **Display Power Management (`dpms`):** When screens power down via DPMS or `hypridle`, rendering halts instantly.

## Hyprland Layer Rules

To prevent compositor animation overhead when the layer is first mapped:

```ini
layerrule = noanim, hyprwpe
```

