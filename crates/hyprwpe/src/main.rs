//! hyprwpe — wallpaper daemon and client for Hyprland.
//!
//! One binary, two roles: `hyprwpe daemon` runs the daemon, every other
//! subcommand is a thin client. Anything the GUI can do must be reachable here
//! first.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use hyprwpe_core::{config, Catalog, Kind, Source};

#[derive(Parser)]
#[command(name = "hyprwpe", version, about = "Wallpaper daemon for Hyprland")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
        source: Vec<std::path::PathBuf>,
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

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::List { kind, json, source } => list(kind.map(Kind::from), json, source),
    }
}

/// A directory holding `project.json` children is a Workshop root; anything else
/// is a plain image directory. Saves the user from stating which is which.
fn source_for(path: std::path::PathBuf) -> Source {
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

fn list(kind: Option<Kind>, json: bool, sources: Vec<std::path::PathBuf>) -> Result<()> {
    let sources: Vec<Source> = if sources.is_empty() {
        config::default_sources()
    } else {
        sources.into_iter().map(source_for).collect()
    };

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
