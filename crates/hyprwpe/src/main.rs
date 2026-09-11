//! hyprwpe — wallpaper daemon and client for Hyprland.
//!
//! One binary, two roles: `hyprwpe daemon` runs the daemon, every other
//! subcommand is a thin client. Anything the GUI can do must be reachable here
//! first.

mod daemon;
mod policy;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hyprwpe_core::client;
use hyprwpe_core::protocol::{self, Request, Response};
use hyprwpe_core::settings::Config;
use hyprwpe_core::{Catalog, Kind, Source, WallpaperId};
use hyprwpe_render::{Scaling, Wallpapers};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "hyprwpe", version, about = "Wallpaper daemon for Hyprland")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon. It owns the wallpaper surfaces for the whole session.
    Daemon,

    /// Set the wallpaper. Accepts an image path or a catalog id.
    Set {
        /// Image file, or an id from `hyprwpe list`.
        wallpaper: String,
        /// Restrict to one output, e.g. `DP-2`. Defaults to every output.
        #[arg(long, value_name = "NAME")]
        output: Option<String>,
        /// How to fit the image to each output.
        #[arg(long, value_enum, default_value_t = ScalingArg::Fill)]
        scaling: ScalingArg,
    },

    /// Per-output state and resource use.
    Status {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Suspend wallpaper rendering (0% CPU).
    Pause,

    /// Resume wallpaper rendering.
    Resume,

    /// Ask the daemon to exit.
    Stop,

    /// List wallpapers hyprwpe can see.
    List {
        /// Show only wallpapers of this kind.
        #[arg(long, value_enum)]
        kind: Option<KindArg>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Scan this directory instead of the configured sources. Workshop
        /// items are recognised by the presence of project.json.
        #[arg(long, value_name = "PATH")]
        source: Vec<PathBuf>,
    },

    /// List the settings a wallpaper exposes, and their current values.
    Properties {
        /// Wallpaper path or catalog id.
        wallpaper: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Change one of a wallpaper's settings and re-apply it live.
    SetProperty {
        /// Wallpaper path or catalog id.
        wallpaper: String,
        /// Property key, as `hyprwpe properties` prints it.
        key: String,
        /// New value. Parsed as JSON when it can be, so `0.5`, `true` and
        /// `"text"` all work as written.
        value: String,
    },

    /// Print the effective configuration and where it was read from.
    Config,

    /// Show an image without a daemon, holding it until interrupted.
    ///
    /// Useful for testing a renderer in isolation; `set` is the normal way in.
    Show {
        image: PathBuf,
        #[arg(long, value_enum, default_value_t = ScalingArg::Fill)]
        scaling: ScalingArg,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum KindArg {
    Image,
    Video,
    Shader,
    Scene,
    Web,
}

impl From<KindArg> for Kind {
    fn from(k: KindArg) -> Kind {
        match k {
            KindArg::Image => Kind::Image,
            KindArg::Video => Kind::Video,
            KindArg::Shader => Kind::Shader,
            KindArg::Scene => Kind::Scene,
            KindArg::Web => Kind::Web,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ScalingArg {
    Fill,
    Fit,
    Stretch,
    Center,
}

impl From<ScalingArg> for Scaling {
    fn from(s: ScalingArg) -> Scaling {
        match s {
            ScalingArg::Fill => Scaling::Fill,
            ScalingArg::Fit => Scaling::Fit,
            ScalingArg::Stretch => Scaling::Stretch,
            ScalingArg::Center => Scaling::Center,
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Daemon => daemon::run(),
        Command::Set {
            wallpaper,
            output,
            scaling,
        } => set(&wallpaper, output, scaling.into()),
        Command::Status { json } => status(json),
        Command::Pause => pause(),
        Command::Resume => resume(),
        Command::Stop => stop(),
        Command::List { kind, json, source } => list(kind.map(Kind::from), json, source),
        Command::Properties { wallpaper, json } => properties(&wallpaper, json),
        Command::SetProperty {
            wallpaper,
            key,
            value,
        } => set_property(&wallpaper, &key, &value),
        Command::Config => show_config(),
        Command::Show { image, scaling } => Wallpapers::run_standalone(&image, scaling.into()),
    }
}

fn sources_from(paths: Vec<PathBuf>) -> Vec<Source> {
    if paths.is_empty() {
        Config::load().sources()
    } else {
        paths.into_iter().map(source_for).collect()
    }
}

/// A directory holding `project.json` children is a Workshop root; anything else
/// is a plain image directory. Saves the user from stating which is which.
fn source_for(path: PathBuf) -> Source {
    let looks_like_workshop = std::fs::read_dir(&path)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .any(|e| e.path().join("project.json").exists())
        })
        .unwrap_or(false);
    if looks_like_workshop {
        Source::Workshop(path)
    } else {
        Source::Directory {
            path,
            recursive: true,
        }
    }
}

/// Turn what the user typed into a file the daemon can render.
///
/// A path is used directly. Anything else is looked up in the catalog, so
/// `hyprwpe set 2904275363` works straight from a `list`. Wallpapers hyprwpe
/// cannot render yet are refused by name rather than silently ignored.
fn resolve(wallpaper: &str) -> Result<PathBuf> {
    let as_path = Path::new(wallpaper);
    if as_path.is_file() {
        return Ok(as_path.to_path_buf());
    }

    let catalog = Catalog::scan(&Config::load().sources());
    let found = catalog.wallpapers.iter().find(|w| match &w.id {
        WallpaperId::Wpe(id) => id == wallpaper,
        WallpaperId::File(p) => p.as_os_str() == wallpaper,
    });

    let Some(w) = found else {
        bail!("{wallpaper:?} is neither a readable file nor a known wallpaper id");
    };

    match w.kind {
        // A Workshop item is a directory; project.json names the file inside it
        // that is actually the wallpaper.
        Kind::Image | Kind::Video | Kind::Shader | Kind::Scene => w.media.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "{:?} declares itself a {} wallpaper but ships no file to play",
                w.title,
                w.kind.as_str()
            )
        }),
        other => bail!(
            "{:?} is a {} wallpaper, which hyprwpe cannot render yet",
            w.title,
            other.as_str()
        ),
    }
}

fn set(wallpaper: &str, output: Option<String>, scaling: Scaling) -> Result<()> {
    let path = resolve(wallpaper)?;
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;

    let target = match output {
        Some(name) => protocol::Target::Output(name),
        None => protocol::Target::All,
    };

    client::send_ok(&Request::Set {
        path,
        target: target.clone(),
        scaling: scaling.as_str().to_string(),
    })?;
    eprintln!("set {} on {}", wallpaper, target);
    Ok(())
}

/// Parse a value the way a user would write it.
///
/// JSON first, so `0.5`, `true`, `"a string"` and numbers keep their types;
/// a bare word that is not valid JSON is taken as a string, because that is
/// what someone typing `hyprwpe set-property X mode Rain` means.
fn parse_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

fn properties(wallpaper: &str, json: bool) -> Result<()> {
    let path = resolve(wallpaper)?;
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;

    let Response::Properties {
        properties,
        script_bindings,
    } = client::send_ok(&Request::Properties { path })?
    else {
        bail!("daemon returned an unexpected response to properties");
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&properties)?);
        return Ok(());
    }

    if properties.is_empty() {
        println!("{} has no settings", wallpaper);
        return Ok(());
    }

    for p in &properties {
        let value = serde_json::to_string(&p.value).unwrap_or_default();
        let mut line = format!("{:28} {:9} {}", p.key, p.kind.as_str(), value);
        if let Some(r) = &p.range {
            line.push_str(&format!("   ({}..{})", r.min, r.max));
        }
        if !p.options.is_empty() {
            let opts: Vec<String> = p
                .options
                .iter()
                .map(|o| format!("{}={}", o.label, o.value))
                .collect();
            line.push_str(&format!("   [{}]", opts.join(", ")));
        }
        if !p.kind.is_editable() {
            line.push_str("   (label, not a value)");
        }
        if !p.text.is_empty() && p.text != p.key {
            line.push_str(&format!("   # {}", p.text));
        }
        println!("{line}");
    }
    if script_bindings > 0 {
        // Do not let a setting look broken when the cause is known.
        eprintln!(
            "note: {script_bindings} scene field(s) are driven by SceneScript, which hyprwpe does not run; \
             changing those settings has no visible effect"
        );
    }
    Ok(())
}

fn set_property(wallpaper: &str, key: &str, value: &str) -> Result<()> {
    let path = resolve(wallpaper)?;
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    let value = parse_value(value);
    match client::send_ok(&Request::SetProperty {
        path,
        key: key.to_string(),
        value: value.clone(),
    })? {
        Response::Ok => {
            eprintln!("{key} = {}", serde_json::to_string(&value)?);
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        other => bail!("daemon returned an unexpected response: {other:?}"),
    }
}

fn show_config() -> Result<()> {
    let config = Config::load();
    match hyprwpe_core::settings::config_path() {
        Some(p) => println!("# {}", p.display()),
        None => println!("# (no config directory)"),
    }
    print!("{}", config.to_toml());
    Ok(())
}

fn status(json: bool) -> Result<()> {
    let Response::Status(status) = client::send_ok(&Request::Status)? else {
        bail!("daemon returned an unexpected response to status");
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    if status.outputs.is_empty() {
        println!("no outputs");
    }
    if status.paused {
        println!("state: paused");
    }
    for o in &status.outputs {
        let wallpaper = o
            .wallpaper
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".into());
        let scaling = o.scaling.as_deref().unwrap_or("-");
        println!(
            "{:<10} {}x{} @{}x  {:<8} {}",
            o.name, o.width, o.height, o.scale, scaling, wallpaper
        );
    }
    if let Some(kb) = status.rss_kb {
        println!("\ndaemon rss: {:.0} MB", kb as f64 / 1024.0);
    }
    Ok(())
}

fn pause() -> Result<()> {
    if !client::daemon_running() {
        eprintln!("no daemon running");
        return Ok(());
    }
    client::send_ok(&Request::Pause)?;
    eprintln!("daemon paused");
    Ok(())
}

fn resume() -> Result<()> {
    if !client::daemon_running() {
        eprintln!("no daemon running");
        return Ok(());
    }
    client::send_ok(&Request::Resume)?;
    eprintln!("daemon resumed");
    Ok(())
}

fn stop() -> Result<()> {
    if !client::daemon_running() {
        eprintln!("no daemon running");
        return Ok(());
    }
    client::send_ok(&Request::Stop)?;
    eprintln!("daemon stopping");
    Ok(())
}

fn list(kind: Option<Kind>, json: bool, sources: Vec<PathBuf>) -> Result<()> {
    let sources = sources_from(sources);
    if sources.is_empty() {
        eprintln!("No wallpaper sources found.");
        eprintln!("Point hyprwpe at one with --source <PATH>.");
        return Ok(());
    }

    let catalog = Catalog::scan(&sources);
    let shown: Vec<_> = catalog
        .wallpapers
        .iter()
        .filter(|w| kind.is_none_or(|k| w.kind == k))
        .collect();

    if json {
        let items: Vec<_> = shown
            .iter()
            .map(|w| {
                serde_json::json!({
                    "id": w.id.to_string(),
                    "title": w.title,
                    "kind": w.kind.as_str(),
                    "path": w.path,
                    "preview": w.preview,
                    "supported": w.supported(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    for w in &shown {
        // A trailing marker rather than a hidden entry: the user owns this
        // wallpaper, so it should be visible along with the reason it cannot
        // be used yet.
        let note = if w.supported() { "" } else { "  (unsupported)" };
        println!("{:<7} {:<12} {}{}", w.kind.as_str(), w.id, w.title, note);
    }

    let total = catalog.wallpapers.len();
    if shown.len() == total {
        let breakdown: Vec<String> = catalog
            .count_by_kind()
            .iter()
            .map(|(k, n)| format!("{n} {}", k.as_str()))
            .collect();
        eprintln!("\n{total} wallpapers: {}", breakdown.join(", "));
    } else {
        eprintln!("\n{} of {total} wallpapers", shown.len());
    }

    for (path, err) in &catalog.problems {
        eprintln!("skipped {}: {err}", path.display());
    }

    Ok(())
}
