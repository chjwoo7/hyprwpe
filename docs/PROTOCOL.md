# hyprwpe IPC Protocol

`hyprwpe` clients (the CLI, `hyprwpe-gui`, scripts, and third-party tools) communicate with the daemon over a UNIX domain socket using newline-delimited JSON (NDJSON).

## Socket Location

The socket is created by the daemon on startup at:
```
$XDG_RUNTIME_DIR/hyprwpe.sock
```
If `$XDG_RUNTIME_DIR` is not set, it defaults to:
```
/tmp/hyprwpe.sock
```

When the daemon stops gracefully or exits, this socket file is automatically removed.

## Wire Format

- **Transport:** UNIX domain stream socket (`AF_UNIX`, `SOCK_STREAM`).
- **Framing:** One JSON object per line terminated by `\n`.
- **Exchange:** Request-response. The client connects, sends exactly one JSON request line, waits for one JSON response line, and the daemon closes the connection.

---

## Requests

All requests contain a `"command"` discriminator field.

### 1. `set`
Set a wallpaper on one or all display outputs.

```json
{
  "command": "set",
  "path": "/home/user/Pictures/wallpaper.png",
  "target": "all",
  "scaling": "fill"
}
```

Or for a specific output:
```json
{
  "command": "set",
  "path": "/home/user/.local/share/Steam/steamapps/workshop/content/431960/3132826255",
  "target": { "output": "eDP-1" },
  "scaling": "fit"
}
```

**Fields:**
- `path` (string, required): Absolute or relative filesystem path to an image, video, shader (`.frag`), or Wallpaper Engine item directory (containing `project.json` or `scene.pkg`).
- `target` (string or object, required):
  - `"all"`: Applies to all connected and future outputs.
  - `{"output": "DP-2"}`: Applies only to the specified output.
- `scaling` (string, required): One of `"fill"`, `"fit"`, `"stretch"`, `"center"`.

---

### 2. `status`
Query current daemon status, per-output wallpaper assignments, and memory footprint.

```json
{
  "command": "status"
}
```

---

### 3. `pause`
Manually suspend all renderers. Rendering loops and decoders freeze at the current frame, dropping CPU and GPU utilization to 0%. Layer surfaces remain mapped so the desktop background does not go black.

```json
{
  "command": "pause"
}
```

---

### 4. `resume`
Resume all suspended renderers. Rendering loops and decoders resume advancing frames.

```json
{
  "command": "resume"
}
```

---

### 5. `stop`
Gracefully shutdown the daemon. Renderers are cleanly torn down, layer surfaces unmapped, the UNIX socket unlinked, and the process exits.

```json
{
  "command": "stop"
}
```

---

### 6. `ping`
Check daemon liveness without side effects.

```json
{
  "command": "ping"
}
```

---

## Responses

All responses contain a `"result"` discriminator field.

### 1. `ok`
Returned when a `set`, `pause`, `resume`, `stop`, or `ping` command succeeds.

```json
{
  "result": "ok"
}
```

### 2. `status`
Returned in response to the `status` command.

```json
{
  "result": "status",
  "paused": false,
  "rss_kb": 363248,
  "outputs": [
    {
      "name": "eDP-1",
      "width": 1024,
      "height": 640,
      "scale": 2,
      "wallpaper": "/home/user/.local/share/Steam/steamapps/workshop/content/431960/1810612745/Perfection.mp4",
      "scaling": "fill"
    },
    {
      "name": "DP-2",
      "width": 2560,
      "height": 1440,
      "scale": 1,
      "wallpaper": "/home/user/hyprwpe/examples/rainbow.frag",
      "scaling": "fill"
    }
  ]
}
```

**Fields:**
- `paused` (boolean): `true` if rendering is paused (via `pause` command or automatic occlusion).
- `rss_kb` (integer, optional): Resident Set Size of the daemon in kilobytes (read directly from `/proc/self/status`).
- `outputs` (array): List of known Wayland outputs and their assigned wallpapers.

### 3. `error`
Returned if a command fails (e.g. invalid arguments, unreadable path, unsupported format).

```json
{
  "result": "error",
  "message": "failed to open wallpaper: No such file or directory (os error 2)"
}
```

---

## Interactive Debugging Examples

Because the protocol is newline-delimited JSON, you can inspect or drive `hyprwpe` using standard UNIX utilities such as `socat`, `nc`, or `jq`:

```bash
# Ping the daemon
echo '{"command":"ping"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/hyprwpe.sock

# Query status and format with jq
echo '{"command":"status"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/hyprwpe.sock | jq .

# Set wallpaper from a bash script
echo '{"command":"set","path":"/path/to/video.mp4","target":"all","scaling":"fill"}' \
  | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/hyprwpe.sock
```
