//! Talking to the daemon.
//!
//! Every client command is one connection, one request, one response. Anything
//! the GUI will do must be reachable here first, so the CLI stays the reference
//! implementation of the protocol.

use crate::protocol::{self, Request, Response};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

pub fn send(req: &Request) -> Result<Response> {
    let sock = protocol::socket_path();
    let mut stream = UnixStream::connect(&sock).with_context(|| {
        format!(
            "connecting to {}. Is the daemon running? Start it with `hyprwpe daemon`",
            sock.display()
        )
    })?;

    let mut body = serde_json::to_string(req).context("encoding request")?;
    body.push('\n');
    stream
        .write_all(body.as_bytes())
        .context("sending request")?;

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("reading response")?;
    if line.trim().is_empty() {
        bail!("daemon closed the connection without replying");
    }
    serde_json::from_str(line.trim()).context("decoding response")
}

/// Send a request that is expected to succeed, turning a daemon-side error into
/// a normal failure rather than a surprising `Ok`.
pub fn send_ok(req: &Request) -> Result<Response> {
    match send(req)? {
        Response::Error { message } => bail!("{message}"),
        other => Ok(other),
    }
}

/// Whether a daemon is listening. A leftover socket file answers nothing, so
/// this connects rather than checking for the path.
pub fn daemon_running() -> bool {
    matches!(send(&Request::Ping), Ok(Response::Ok))
}
