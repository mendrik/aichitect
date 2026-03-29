use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

mod config;
mod document;
mod history;
mod openai;
mod remarks;
mod revision_context;
mod review;
mod tui;
mod watcher;

use config::Config;
use document::Document;

#[derive(Parser, Debug)]
#[command(name = "aichitect", about = "Terminal-first AI document iteration tool", version)]
struct Cli {
    /// Path to the Markdown document to open
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Initialize sample config at ~/.aichitect/config.toml
    #[arg(long)]
    init: bool,

    /// Print the anchor map for the document and exit
    #[arg(long)]
    anchors: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.init {
        Config::write_sample()?;
        let path = Config::config_path();
        println!("Sample config written to: {}", path.display());
        println!("Edit it to add your OpenAI API key, then run: aichitect <file.md>");
        return Ok(());
    }

    let file = match cli.file {
        Some(f) => f,
        None => {
            eprintln!("Usage: aichitect <file.md>");
            eprintln!("       aichitect --init   (create sample config)");
            std::process::exit(1);
        }
    };

    let config = Config::load().map_err(|e| {
        eprintln!("Configuration error: {}", e);
        e
    })?;

    let doc = if file.exists() {
        Document::load(file).map_err(|e| {
            eprintln!("Failed to load document: {}", e);
            e
        })?
    } else {
        // File does not exist yet — open in creation-prompt mode.
        Document::empty(file)
    };

    if cli.anchors {
        if doc.is_new() {
            eprintln!("File does not exist yet; no anchors to show.");
            std::process::exit(1);
        }
        println!("{}", doc.anchor_map_display());
        return Ok(());
    }

    tui::run(config, doc).await?;

    Ok(())
}
