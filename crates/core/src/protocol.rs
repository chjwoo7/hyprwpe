//! The wire protocol between `hyprwpe` clients and the daemon.
//!
//! One JSON object per line over a unix socket, request then response, then the
//! connection closes. Newline-delimited JSON costs nothing at this message
//! volume and keeps the protocol debuggable with `socat` alone — worth more than
//! the bytes a binary encoding would save.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Socket path, honouring `XDG_RUNTIME_DIR` and falling back to `/tmp` so a
/// login without one still works.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("hyprwpe.sock")
}

/// Which outputs a command applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Every output, now and any connected later.
    All,
    /// One output by name, e.g. `DP-2`.
    Output(String),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::All => f.write_str("all"),
            Target::Output(name) => f.write_str(name),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    /// Show a wallpaper. `scaling` is a `Scaling::as_str` value.
    Set {
        path: PathBuf,
        target: Target,
        scaling: String,
    },
    /// Per-output state and resource use.
    Status,
    /// Suspend all renderers. Frame callbacks stop, video decoders stop,
    /// and CPU drops to zero. The surfaces stay mapped so the compositor
    /// shows the last frame rather than nothing.
    Pause,
    /// Resume suspended renderers.
    Resume,
    /// Ask the daemon to exit. Used by `hyprwpe stop`, and to check liveness
    /// without side effects when paired with `Ping`.
    Stop,
    /// Liveness check. A stale socket file fails to connect; a live daemon
    /// answers.
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Status(Status),
    /// The request was understood but could not be carried out.
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub outputs: Vec<OutputStatus>,
    /// Whether all renderers are paused (either by `hyprwpe pause` or by
    /// automatic occlusion detection).
    #[serde(default)]
    pub paused: bool,
    /// Resident set size in kilobytes, as the daemon sees itself. Efficiency
    /// claims should be checkable without an external profiler.
    pub rss_kb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputStatus {
    pub name: String,
    /// Logical size reported by the compositor.
    pub width: u32,
    pub height: u32,
    pub scale: i32,
    pub wallpaper: Option<PathBuf>,
    pub scaling: Option<String>,
}

/// Read the daemon's own RSS. Linux-specific and best-effort: a missing or
/// unparsable `status` file yields `None` rather than an error.
pub fn self_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(req: &Request) -> Request {
        let line = serde_json::to_string(req).unwrap();
        assert!(
            !line.contains('\n'),
            "a request must fit on one line: {line}"
        );
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn requests_survive_a_roundtrip() {
        let set = Request::Set {
            path: PathBuf::from("/tmp/a b.png"),
            target: Target::Output("DP-2".into()),
            scaling: "fill".into(),
        };
        match roundtrip(&set) {
            Request::Set {
                path,
                target,
                scaling,
            } => {
                assert_eq!(path, PathBuf::from("/tmp/a b.png"));
                assert_eq!(target, Target::Output("DP-2".into()));
                assert_eq!(scaling, "fill");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(matches!(roundtrip(&Request::Status), Request::Status));
        assert!(matches!(roundtrip(&Request::Pause), Request::Pause));
        assert!(matches!(roundtrip(&Request::Resume), Request::Resume));
        assert!(matches!(roundtrip(&Request::Ping), Request::Ping));
    }

    /// Paths are not required to be UTF-8, and a newline in one would split the
    /// message. serde_json escapes both, but the guarantee is worth pinning.
    #[test]
    fn awkward_paths_stay_on_one_line() {
        let req = Request::Set {
            path: PathBuf::from("/tmp/we\nird\"quote.png"),
            target: Target::All,
            scaling: "fit".into(),
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n'));
        assert!(matches!(
            serde_json::from_str::<Request>(&line),
            Ok(Request::Set { .. })
        ));
    }

    #[test]
    fn unknown_command_is_rejected_not_guessed() {
        let err = serde_json::from_str::<Request>(r#"{"command":"nonsense"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn socket_path_follows_xdg_runtime_dir() {
        // Not asserting the process env, only the shape of the result.
        let p = socket_path();
        assert_eq!(p.file_name().unwrap(), "hyprwpe.sock");
        assert!(p.is_absolute());
    }
}
