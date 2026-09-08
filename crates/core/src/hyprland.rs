//! Hyprland IPC — read-only connection to the compositor's event stream.
//!
//! Hyprland exposes two unix sockets: one for requests (`.socket.sock`) and one
//! for events (`.socket2.sock`). We only need the event stream: the wallpaper
//! daemon does not send commands to Hyprland, it only reacts to what Hyprland
//! tells it — which windows moved, which workspaces are active, and whether the
//! wallpaper is covered.
//!
//! The events arrive as newline-delimited `EVENT>>DATA` pairs. We parse only
//! the ones that affect visibility.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Events we care about, extracted from Hyprland's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyprEvent {
    /// A window went fullscreen or left fullscreen.
    Fullscreen(bool),
    /// The active workspace changed on some monitor.
    Workspace(String),
    /// A window was opened.
    OpenWindow {
        address: String,
        workspace: String,
        class: String,
        title: String,
    },
    /// A window was closed.
    CloseWindow { address: String },
    /// A window was moved to a workspace.
    MoveWindow {
        address: String,
        workspace: String,
    },
    /// A monitor was added or removed (hotplug).
    MonitorAdded(String),
    MonitorRemoved(String),
    /// Active monitor changed.
    FocusedMon {
        name: String,
        workspace: String,
    },
    /// Screen lock activated or deactivated (hyprlock / ext-session-lock).
    Lock(bool),
    /// DPMS display power state (true = on, false = asleep/off).
    Dpms(bool),
}

/// Locate the Hyprland request socket (`.socket.sock`).
pub fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let instance = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(
        PathBuf::from(runtime)
            .join("hypr")
            .join(instance)
            .join(".socket.sock"),
    )
}

/// Send a synchronous request to Hyprland's request socket and return the response.
pub fn query(cmd: &str) -> Option<String> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).ok()?;
    stream.write_all(cmd.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

/// Locate the Hyprland event socket (`.socket2.sock`).
///
/// The path is `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`.
/// Both environment variables must be set; a missing one means Hyprland is not
/// the compositor, and this module quietly returns `None`.
pub fn socket2_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let instance = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(
        PathBuf::from(runtime)
            .join("hypr")
            .join(instance)
            .join(".socket2.sock"),
    )
}

/// Connect to Hyprland's event stream.
///
/// Returns a non-blocking reader or `None` if Hyprland is not running.
pub fn connect_event_stream() -> Option<UnixStream> {
    let path = socket2_path()?;
    let stream = UnixStream::connect(&path).ok()?;
    stream.set_nonblocking(true).ok()?;
    Some(stream)
}

/// Parse one line from the event stream.
///
/// Lines are `EVENT>>DATA\n`. Unknown events are silently dropped.
pub fn parse_event(line: &str) -> Option<HyprEvent> {
    let (event, data) = line.split_once(">>")?;
    match event {
        "fullscreen" => {
            let on = data.trim() != "0";
            Some(HyprEvent::Fullscreen(on))
        }
        "workspace" => Some(HyprEvent::Workspace(data.trim().to_string())),
        "openwindow" => {
            // address,workspace,class,title
            let mut parts = data.splitn(4, ',');
            let address = parts.next()?.to_string();
            let workspace = parts.next()?.to_string();
            let class = parts.next()?.to_string();
            let title = parts.next().unwrap_or("").trim().to_string();
            Some(HyprEvent::OpenWindow {
                address,
                workspace,
                class,
                title,
            })
        }
        "closewindow" => Some(HyprEvent::CloseWindow {
            address: data.trim().to_string(),
        }),
        "movewindow" => {
            let (address, workspace) = data.trim().split_once(',')?;
            Some(HyprEvent::MoveWindow {
                address: address.to_string(),
                workspace: workspace.to_string(),
            })
        }
        "monitoradded" => Some(HyprEvent::MonitorAdded(data.trim().to_string())),
        "monitorremoved" => Some(HyprEvent::MonitorRemoved(data.trim().to_string())),
        "focusedmon" => {
            let (name, workspace) = data.trim().split_once(',')?;
            Some(HyprEvent::FocusedMon {
                name: name.to_string(),
                workspace: workspace.to_string(),
            })
        }
        "lockscreen" => {
            let on = data.trim() == "1";
            Some(HyprEvent::Lock(on))
        }
        "dpms" => {
            let on = data.trim() == "1";
            Some(HyprEvent::Dpms(on))
        }
        _ => None,
    }
}

/// A non-blocking event reader. Call `poll()` to drain available events.
pub struct EventReader {
    reader: BufReader<UnixStream>,
    buf: String,
    eof: bool,
}

impl EventReader {
    pub fn new(stream: UnixStream) -> Self {
        EventReader {
            reader: BufReader::new(stream),
            buf: String::new(),
            eof: false,
        }
    }

    /// Whether the stream hit EOF.
    pub fn is_eof(&self) -> bool {
        self.eof
    }

    /// Read all currently available events. Returns an empty vec when there is
    /// nothing to read (non-blocking).
    pub fn poll(&mut self) -> Vec<HyprEvent> {
        let mut events = Vec::new();
        if self.eof {
            return events;
        }
        loop {
            self.buf.clear();
            match self.reader.read_line(&mut self.buf) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(_) => {
                    if let Some(event) = parse_event(self.buf.trim()) {
                        events.push(event);
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.eof = true;
                    break;
                }
            }
        }
        events
    }
}

impl AsFd for EventReader {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.reader.get_ref().as_fd()
    }
}

/// State of a single monitor tracked by Hyprland events.
#[derive(Debug, Clone, Default)]
pub struct MonitorInfo {
    pub name: String,
    pub active_workspace: String,
    /// Set of workspaces on this monitor that currently have a fullscreen window.
    pub fullscreen_workspaces: HashSet<String>,
}

/// Tracks compositor visibility and occlusion across all outputs based on
/// Hyprland events and socket state.
#[derive(Debug, Clone)]
pub struct OcclusionTracker {
    monitors: HashMap<String, MonitorInfo>,
    focused_monitor: Option<String>,
    active_fullscreen: bool,
    locked: bool,
    dpms_on: bool,
}

impl Default for OcclusionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl OcclusionTracker {
    pub fn new() -> Self {
        Self {
            monitors: HashMap::new(),
            focused_monitor: None,
            active_fullscreen: false,
            locked: false,
            dpms_on: true,
        }
    }

    /// Query Hyprland request socket to initialize monitor and window state.
    pub fn sync_from_hyprland(&mut self) {
        if let Some(json_str) = query("j/monitors") {
            #[derive(serde::Deserialize)]
            struct MonJson {
                name: String,
                #[serde(rename = "activeWorkspace")]
                active_workspace: WsJson,
                #[serde(default)]
                focused: bool,
            }
            #[derive(serde::Deserialize)]
            struct WsJson {
                name: String,
            }
            if let Ok(mons) = serde_json::from_str::<Vec<MonJson>>(&json_str) {
                for m in mons {
                    if m.focused {
                        self.focused_monitor = Some(m.name.clone());
                    }
                    self.monitors.insert(
                        m.name.clone(),
                        MonitorInfo {
                            name: m.name,
                            active_workspace: m.active_workspace.name,
                            fullscreen_workspaces: HashSet::new(),
                        },
                    );
                }
            }
        }

        if let Some(json_str) = query("j/clients") {
            #[derive(serde::Deserialize)]
            struct ClientJson {
                workspace: WsJson,
                #[serde(default)]
                fullscreen: serde_json::Value,
            }
            #[derive(serde::Deserialize)]
            struct WsJson {
                name: String,
            }
            if let Ok(clients) = serde_json::from_str::<Vec<ClientJson>>(&json_str) {
                for c in clients {
                    let is_fullscreen = match c.fullscreen {
                        serde_json::Value::Bool(b) => b,
                        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
                        _ => false,
                    };
                    if is_fullscreen {
                        for mon in self.monitors.values_mut() {
                            if mon.active_workspace == c.workspace.name {
                                mon.fullscreen_workspaces.insert(c.workspace.name.clone());
                            }
                        }
                        self.active_fullscreen = true;
                    }
                }
            }
        }
    }

    /// Process a Hyprland event and update occlusion tracking.
    pub fn handle_event(&mut self, event: &HyprEvent) {
        match event {
            HyprEvent::FocusedMon { name, workspace } => {
                self.focused_monitor = Some(name.clone());
                let mon = self.monitors.entry(name.clone()).or_insert_with(|| MonitorInfo {
                    name: name.clone(),
                    active_workspace: workspace.clone(),
                    fullscreen_workspaces: HashSet::new(),
                });
                mon.active_workspace = workspace.clone();
            }
            HyprEvent::Workspace(workspace) => {
                if let Some(focused) = &self.focused_monitor {
                    if let Some(mon) = self.monitors.get_mut(focused) {
                        mon.active_workspace = workspace.clone();
                    }
                }
            }
            HyprEvent::Fullscreen(on) => {
                self.active_fullscreen = *on;
                if let Some(focused) = &self.focused_monitor {
                    if let Some(mon) = self.monitors.get_mut(focused) {
                        if *on {
                            mon.fullscreen_workspaces.insert(mon.active_workspace.clone());
                        } else {
                            mon.fullscreen_workspaces.remove(&mon.active_workspace);
                        }
                    }
                }
            }
            HyprEvent::Lock(locked) => {
                self.locked = *locked;
            }
            HyprEvent::Dpms(on) => {
                self.dpms_on = *on;
            }
            HyprEvent::MonitorAdded(name) => {
                self.monitors.entry(name.clone()).or_insert_with(|| MonitorInfo {
                    name: name.clone(),
                    active_workspace: "1".into(),
                    fullscreen_workspaces: HashSet::new(),
                });
            }
            HyprEvent::MonitorRemoved(name) => {
                self.monitors.remove(name);
                if self.focused_monitor.as_ref() == Some(name) {
                    self.focused_monitor = self.monitors.keys().next().cloned();
                }
            }
            HyprEvent::OpenWindow { .. } | HyprEvent::CloseWindow { .. } | HyprEvent::MoveWindow { .. } => {}
        }
    }

    /// Returns true if all outputs are currently occluded (covered by fullscreen
    /// windows, display power off, or session locked).
    pub fn is_all_occluded(&self) -> bool {
        if !self.dpms_on || self.locked {
            return true;
        }
        if self.monitors.is_empty() {
            return self.active_fullscreen;
        }
        self.monitors
            .values()
            .all(|m| m.fullscreen_workspaces.contains(&m.active_workspace))
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn is_dpms_off(&self) -> bool {
        !self.dpms_on
    }

    pub fn monitors(&self) -> &HashMap<String, MonitorInfo> {
        &self.monitors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fullscreen_on() {
        let e = parse_event("fullscreen>>1").unwrap();
        assert!(matches!(e, HyprEvent::Fullscreen(true)));
    }

    #[test]
    fn parse_fullscreen_off() {
        let e = parse_event("fullscreen>>0").unwrap();
        assert!(matches!(e, HyprEvent::Fullscreen(false)));
    }

    #[test]
    fn parse_workspace() {
        let e = parse_event("workspace>>3").unwrap();
        assert!(matches!(e, HyprEvent::Workspace(ref n) if n == "3"));
    }

    #[test]
    fn parse_openwindow() {
        let e = parse_event("openwindow>>0x1234,2,kitty,Terminal").unwrap();
        match e {
            HyprEvent::OpenWindow {
                address,
                workspace,
                class,
                title,
            } => {
                assert_eq!(address, "0x1234");
                assert_eq!(workspace, "2");
                assert_eq!(class, "kitty");
                assert_eq!(title, "Terminal");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_closewindow() {
        let e = parse_event("closewindow>>0x1234").unwrap();
        assert!(matches!(e, HyprEvent::CloseWindow { address } if address == "0x1234"));
    }

    #[test]
    fn parse_movewindow() {
        let e = parse_event("movewindow>>0x1234,3").unwrap();
        match e {
            HyprEvent::MoveWindow {
                address,
                workspace,
            } => {
                assert_eq!(address, "0x1234");
                assert_eq!(workspace, "3");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_focusedmon() {
        let e = parse_event("focusedmon>>DP-2,1").unwrap();
        match e {
            HyprEvent::FocusedMon { name, workspace } => {
                assert_eq!(name, "DP-2");
                assert_eq!(workspace, "1");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn unknown_event_is_none() {
        assert!(parse_event("somethingelse>>data").is_none());
    }

    #[test]
    fn empty_line_is_none() {
        assert!(parse_event("").is_none());
    }

    #[test]
    fn parse_fullscreen_mode2() {
        let e = parse_event("fullscreen>>2").unwrap();
        assert!(matches!(e, HyprEvent::Fullscreen(true)));
    }

    #[test]
    fn parse_lockscreen() {
        let on = parse_event("lockscreen>>1").unwrap();
        assert!(matches!(on, HyprEvent::Lock(true)));
        let off = parse_event("lockscreen>>0").unwrap();
        assert!(matches!(off, HyprEvent::Lock(false)));
    }

    #[test]
    fn parse_dpms() {
        let off = parse_event("dpms>>0").unwrap();
        assert!(matches!(off, HyprEvent::Dpms(false)));
        let on = parse_event("dpms>>1").unwrap();
        assert!(matches!(on, HyprEvent::Dpms(true)));
    }

    #[test]
    fn occlusion_tracker_single_monitor() {
        let mut tracker = OcclusionTracker {
            monitors: HashMap::new(),
            focused_monitor: None,
            active_fullscreen: false,
            locked: false,
            dpms_on: true,
        };
        tracker.handle_event(&HyprEvent::FocusedMon {
            name: "DP-2".into(),
            workspace: "1".into(),
        });
        assert!(!tracker.is_all_occluded());

        // Fullscreen on workspace 1 -> occluded
        tracker.handle_event(&HyprEvent::Fullscreen(true));
        assert!(tracker.is_all_occluded());

        // Switch to empty workspace 2 -> visible
        tracker.handle_event(&HyprEvent::Workspace("2".into()));
        assert!(!tracker.is_all_occluded());

        // Switch back to workspace 1 -> occluded again
        tracker.handle_event(&HyprEvent::Workspace("1".into()));
        assert!(tracker.is_all_occluded());

        // Exit fullscreen -> visible
        tracker.handle_event(&HyprEvent::Fullscreen(false));
        assert!(!tracker.is_all_occluded());
    }

    #[test]
    fn occlusion_tracker_multi_monitor() {
        let mut tracker = OcclusionTracker {
            monitors: HashMap::new(),
            focused_monitor: None,
            active_fullscreen: false,
            locked: false,
            dpms_on: true,
        };
        tracker.handle_event(&HyprEvent::FocusedMon {
            name: "DP-2".into(),
            workspace: "1".into(),
        });
        tracker.handle_event(&HyprEvent::FocusedMon {
            name: "eDP-1".into(),
            workspace: "2".into(),
        });

        // Fullscreen on eDP-1 (currently focused) -> DP-2 is still visible!
        tracker.handle_event(&HyprEvent::Fullscreen(true));
        assert!(!tracker.is_all_occluded());

        // Now focus DP-2 and fullscreen it as well -> both occluded!
        tracker.handle_event(&HyprEvent::FocusedMon {
            name: "DP-2".into(),
            workspace: "1".into(),
        });
        tracker.handle_event(&HyprEvent::Fullscreen(true));
        assert!(tracker.is_all_occluded());

        // eDP-1 exits fullscreen -> background visible on eDP-1
        tracker.handle_event(&HyprEvent::FocusedMon {
            name: "eDP-1".into(),
            workspace: "2".into(),
        });
        tracker.handle_event(&HyprEvent::Fullscreen(false));
        assert!(!tracker.is_all_occluded());
    }

    #[test]
    fn occlusion_tracker_lock_and_dpms() {
        let mut tracker = OcclusionTracker {
            monitors: HashMap::new(),
            focused_monitor: None,
            active_fullscreen: false,
            locked: false,
            dpms_on: true,
        };
        tracker.handle_event(&HyprEvent::FocusedMon {
            name: "DP-2".into(),
            workspace: "1".into(),
        });
        assert!(!tracker.is_all_occluded());

        // DPMS off -> occluded
        tracker.handle_event(&HyprEvent::Dpms(false));
        assert!(tracker.is_all_occluded());
        tracker.handle_event(&HyprEvent::Dpms(true));
        assert!(!tracker.is_all_occluded());

        // Lockscreen on -> occluded
        tracker.handle_event(&HyprEvent::Lock(true));
        assert!(tracker.is_all_occluded());
        tracker.handle_event(&HyprEvent::Lock(false));
        assert!(!tracker.is_all_occluded());
    }

    #[test]
    fn socket_path_shape() {
        // Only check that it does not panic; the env vars may or may not be set.
        let _ = socket_path();
        let _ = socket2_path();
    }
}
