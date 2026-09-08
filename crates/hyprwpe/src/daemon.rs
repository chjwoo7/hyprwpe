//! The daemon: owns the layer surfaces and serves the control socket.
//!
//! One event loop watches both the Wayland connection and the IPC socket, so a
//! client request and a compositor event go through the same state without
//! locking. `calloop` rather than an async runtime: this waits on a handful of
//! file descriptors and never needs a thread pool, and the process that stays
//! resident should stay small.

use anyhow::{bail, Context, Result};
use hyprwpe_core::protocol::{self, Request, Response, Status};
use hyprwpe_core::settings::State;
use hyprwpe_render::{Scaling, Target, WallpaperSpec, Wallpapers};
use smithay_client_toolkit::reexports::{
    calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction},
    calloop_wayland_source::WaylandSource,
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use wayland_client::{globals::registry_queue_init, Connection};

pub fn run() -> Result<()> {
    let sock = protocol::socket_path();

    // A socket file outlives a daemon that was killed, so its mere presence
    // proves nothing. Connecting does: if someone answers, they own the display.
    if UnixStream::connect(&sock).is_ok() {
        bail!("hyprwpe is already running (socket {})", sock.display());
    }
    if sock.exists() {
        std::fs::remove_file(&sock)
            .with_context(|| format!("removing stale socket {}", sock.display()))?;
    }

    let listener =
        UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;
    listener
        .set_nonblocking(true)
        .context("setting the listener non-blocking")?;

    let conn = Connection::connect_to_env()
        .context("connecting to the Wayland compositor (is WAYLAND_DISPLAY set?)")?;
    let (globals, queue) = registry_queue_init(&conn).context("initialising registry")?;
    let qh = queue.handle();
    let mut wallpapers = Wallpapers::new(&globals, &qh)?;

    // Restore what the last session was showing. Without this, adding
    // `exec-once = hyprwpe daemon` gets you a black screen at login, which is
    // not a wallpaper daemon anyone can use.
    let mut saved = State::load();
    saved.prune_missing();
    wallpapers.restore(&saved);

    let mut event_loop: EventLoop<Wallpapers> =
        EventLoop::try_new().context("creating the event loop")?;
    let handle = event_loop.handle();

    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .map_err(|e| anyhow::anyhow!("inserting the wayland source: {e}"))?;

    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            |_, listener, state: &mut Wallpapers| {
                // Level-triggered, so drain everything pending before returning.
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, state),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            eprintln!("hyprwpe: accept failed: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("inserting the socket source: {e}"))?;

    let result = (|| -> Result<()> {
        while !wallpapers.should_exit() {
            event_loop
                .dispatch(None, &mut wallpapers)
                .context("event loop dispatch")?;
        }
        Ok(())
    })();

    // Leave no socket behind on a clean exit, so the next start does not have to
    // reason about whether it is stale.
    let _ = std::fs::remove_file(&sock);
    result
}

/// Handle one request and close. Connections are not kept open: the protocol is
/// request/response and clients are short-lived, so there is no session state to
/// track and a wedged client cannot hold the daemon.
fn serve(stream: UnixStream, state: &mut Wallpapers) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hyprwpe: cloning client socket: {e}");
            return;
        }
    });
    let mut line = String::new();
    if let Err(e) = reader.read_line(&mut line) {
        eprintln!("hyprwpe: reading request: {e}");
        return;
    }

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => handle(req, state),
        Err(e) => Response::Error {
            message: format!("malformed request: {e}"),
        },
    };

    let mut stream = stream;
    if let Ok(mut body) = serde_json::to_string(&response) {
        body.push('\n');
        let _ = stream.write_all(body.as_bytes());
    }
}

fn handle(req: Request, state: &mut Wallpapers) -> Response {
    match req {
        Request::Ping => Response::Ok,

        Request::Stop => {
            state.request_exit();
            Response::Ok
        }

        Request::Status => Response::Status(Status {
            outputs: state.status(),
            rss_kb: protocol::self_rss_kb(),
        }),

        Request::Set {
            path,
            target,
            scaling,
        } => match prepare_set(&path, &target, &scaling, state) {
            Ok((target, spec)) => {
                state.set(target, spec);
                // Persist after applying, so a state file only ever describes
                // something that actually worked.
                if let Err(e) = state.snapshot().save() {
                    eprintln!("hyprwpe: could not save state: {e:#}");
                }
                Response::Ok
            }
            Err(message) => Response::Error { message },
        },
    }
}

/// Validate a `set` before it touches state, so a bad request leaves the current
/// wallpaper alone rather than half-applying.
fn prepare_set(
    path: &Path,
    target: &protocol::Target,
    scaling: &str,
    state: &Wallpapers,
) -> Result<(Target, WallpaperSpec), String> {
    if !path.is_file() {
        return Err(format!("{} is not a readable file", path.display()));
    }

    let scaling =
        Scaling::parse(scaling).ok_or_else(|| format!("unknown scaling mode {scaling:?}"))?;

    let target = match target {
        protocol::Target::All => Target::All,
        protocol::Target::Output(name) => {
            let known = state.output_names();
            if !known.iter().any(|n| n == name) {
                return Err(format!(
                    "no output named {name:?}; connected outputs: {}",
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                ));
            }
            Target::Output(name.clone())
        }
    };

    Ok((
        target,
        WallpaperSpec {
            path: path.to_path_buf(),
            scaling,
        },
    ))
}
