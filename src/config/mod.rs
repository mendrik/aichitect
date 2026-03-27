use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

impl Default for ColorMode {
    fn default() -> Self { ColorMode::Auto }
}

/// Raw TOML shape — every tunable is `Option<T>` so we can distinguish
/// "user set this" from "user omitted this".  Only `api_key` is required.
#[derive(Debug, Clone, Deserialize)]
struct RawConfig {
    pub api_key: String,
    pub model: Option<String>,
    pub model_fix: Option<String>,
    pub base_url: Option<String>,
    pub organization: Option<String>,
    pub project: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub streaming: Option<bool>,
    pub color_mode: Option<ColorMode>,
    pub autosave: Option<bool>,
    pub autosave_interval_secs: Option<u64>,
    pub system_prompt_override: Option<String>,
}

/// Resolved config used throughout the app.
///
/// Fields that affect the OpenAI API payload (`temperature`, `max_tokens`,
/// `streaming`) remain `Option<T>` so the client only sends what the user
/// explicitly configured.  Everything else has a concrete default for
/// internal use.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    pub api_key: String,
    pub model: String,
    pub model_fix: String,
    pub base_url: Option<String>,
    pub organization: Option<String>,
    pub project: Option<String>,
    /// `None` → field not present in config.toml → not sent to the API.
    pub temperature: Option<f32>,
    /// `None` → field not present in config.toml → not sent to the API.
    pub max_tokens: Option<u32>,
    /// `None` → not set → default `false` (non-streaming non-breaking fallback).
    pub streaming: bool,
    pub color_mode: ColorMode,
    pub autosave: bool,
    pub autosave_interval_secs: u64,
    pub system_prompt_override: Option<String>,
}

impl Config {
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".aichitect")
            .join("config.toml")
    }

    /// Read and parse config from disk.  Called on every startup and before
    /// every API request so that edits to config.toml take effect immediately.
    pub fn load() -> Result<Config> {
        let path = Self::config_path();
        let content = fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read config from {}. Run `aichitect --init` to create a sample.",
                path.display()
            )
        })?;
        let raw: RawConfig =
            toml::from_str(&content).context("Failed to parse config.toml")?;
        if raw.api_key.is_empty() || raw.api_key.starts_with("sk-...") {
            anyhow::bail!(
                "api_key is not set in config.toml. \
                 Add your OpenAI API key and try again."
            );
        }
        Ok(Config {
            api_key: raw.api_key,
            model: raw.model.unwrap_or_else(|| "gpt-4o".to_string()),
            model_fix: raw.model_fix.unwrap_or_else(|| "gpt-4o".to_string()),
            base_url: raw.base_url,
            organization: raw.organization,
            project: raw.project,
            temperature: raw.temperature,
            max_tokens: raw.max_tokens,
            streaming: raw.streaming.unwrap_or(false),
            color_mode: raw.color_mode.unwrap_or_default(),
            autosave: raw.autosave.unwrap_or(false),
            autosave_interval_secs: raw.autosave_interval_secs.unwrap_or(30),
            system_prompt_override: raw.system_prompt_override,
        })
    }

    pub fn write_sample() -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Only api_key and model are uncommented — everything else is optional
        // and shows the user exactly what they can override.
        let sample = r#"# Aichitect configuration  (~/.aichitect/config.toml)
# Only api_key is required.  All other fields are optional; omitting them
# lets the OpenAI API use its own defaults.

api_key = "sk-..."
model = "gpt-4o"
model_fix = "gpt-4o-mini"

# Optional — uncomment to override OpenAI defaults:
# temperature = 0.3
# max_tokens = 4096
# streaming = true

# Optional — routing / auth:
# base_url = "https://api.openai.com/v1"
# organization = "org-..."
# project = "proj_..."

# Optional — TUI behaviour:
# color_mode = "auto"          # auto | always | never
# autosave = false
# autosave_interval_secs = 30

# Optional — override the system prompt used for revisions:
# system_prompt_override = "You are a helpful document editor."
"#;
        fs::write(&path, sample)?;
        Ok(())
    }
}
