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
use hyprwpe_core::Kind;
use hyprwpe_render::{Scaling, Target, WallpaperSpec, Wallpapers};
use smithay_client_toolkit::reexports::{
    calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction},
    calloop_wayland_source::WaylandSource,
};
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use wayland_client::{globals::registry_queue_init, Connection};

use crate::policy::{PolicyAction, PolicyEngine};

fn apply_action(action: PolicyAction, wallpapers: &mut Wallpapers) {
    match action {
        PolicyAction::Pause => wallpapers.pause(),
        PolicyAction::Resume => wallpapers.resume(),
        PolicyAction::None => {}
    }
}

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
    let mut wallpapers = Wallpapers::new(&conn, &globals, &qh)?;

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

    let policy = Rc::new(RefCell::new(PolicyEngine::new()));
    policy.borrow_mut().sync_from_hyprland();

    // Listen to Hyprland's socket2 event stream if Hyprland is the compositor.
    if let Some(hypr_stream) = hyprwpe_core::hyprland::connect_event_stream() {
        let read_stream = match hypr_stream.try_clone() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("hyprwpe: cloning Hyprland event socket: {e}");
                None
            }
        };
        if let Some(read_stream) = read_stream {
            let mut reader = hyprwpe_core::hyprland::EventReader::new(read_stream);
            let policy_hypr = Rc::clone(&policy);
            if let Err(e) = handle.insert_source(
                Generic::new(hypr_stream, Interest::READ, Mode::Level),
                move |_, _, state: &mut Wallpapers| {
                    let events = reader.poll();
                    let mut pol = policy_hypr.borrow_mut();
                    for event in events {
                        let action = pol.handle_event(&event);
                        apply_action(action, state);
                    }
                    if reader.is_eof() {
                        eprintln!("hyprwpe: Hyprland event socket closed");
                        return Ok(PostAction::Remove);
                    }
                    Ok(PostAction::Continue)
                },
            ) {
                eprintln!("hyprwpe: inserting Hyprland event source: {e}");
            }
        }
    } else {
        eprintln!("hyprwpe: Hyprland socket2 not detected; running without compositor occlusion detection");
    }

    let policy_ipc = Rc::clone(&policy);
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, listener, state: &mut Wallpapers| {
                // Level-triggered, so drain everything pending before returning.
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, state, &policy_ipc),
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
            let timeout = policy.borrow().pending_timeout();
            event_loop
                .dispatch(timeout, &mut wallpapers)
                .context("event loop dispatch")?;
            let action = policy.borrow_mut().tick();
            apply_action(action, &mut wallpapers);
        }
        Ok(())
    })();

    // Leave no socket behind on a clean exit, so the next start does not have to
    // reason about whether it is stale.
    let _ = std::fs::remove_file(&sock);
    result
}

/// How long a client has to send its request before the daemon gives up on it.
///
/// This runs inside the event loop, so a client that connects and says nothing
/// would otherwise stop the daemon dead — no further requests, and no Wayland
/// events either, which means no response to hotplug or reconfiguration. A
/// crashed client mid-request is enough to trigger it. Generous for a local
/// socket, short enough that nothing notices.
const CLIENT_TIMEOUT: Duration = Duration::from_millis(500);

/// Handle one request and close. Connections are not kept open: the protocol is
/// request/response and clients are short-lived, so there is no session state to
/// track and a wedged client cannot hold the daemon.
fn serve(stream: UnixStream, state: &mut Wallpapers, policy: &Rc<RefCell<PolicyEngine>>) {
    if let Err(e) = stream.set_read_timeout(Some(CLIENT_TIMEOUT)) {
        eprintln!("hyprwpe: setting client timeout: {e}");
        return;
    }
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));

    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hyprwpe: cloning client socket: {e}");
            return;
        }
    });
    let mut line = String::new();
    if let Err(e) = reader.read_line(&mut line) {
        // A silent client is dropped rather than waited on.
        if e.kind() != std::io::ErrorKind::WouldBlock && e.kind() != std::io::ErrorKind::TimedOut {
            eprintln!("hyprwpe: reading request: {e}");
        }
        return;
    }

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => handle(req, state, policy),
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

fn handle(req: Request, state: &mut Wallpapers, policy: &Rc<RefCell<PolicyEngine>>) -> Response {
    match req {
        Request::Ping => Response::Ok,

        Request::Stop => {
            state.request_exit();
            Response::Ok
        }

        Request::Pause => {
            let action = policy.borrow_mut().set_manual_pause(true);
            apply_action(action, state);
            Response::Ok
        }

        Request::Resume => {
            let action = policy.borrow_mut().set_manual_pause(false);
            apply_action(action, state);
            Response::Ok
        }

        Request::Status => Response::Status(Status {
            outputs: state.status(),
            paused: policy.borrow().is_paused() || state.is_paused(),
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

    let kind = Kind::from_extension(path)
        .ok_or_else(|| format!("{} is not a wallpaper hyprwpe can render", path.display()))?;
    if !matches!(kind, Kind::Image | Kind::Video | Kind::Shader | Kind::Scene) {
        return Err(format!(
            "hyprwpe cannot render {} wallpapers yet",
            kind.as_str()
        ));
    }

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
            kind,
        },
    ))
}
