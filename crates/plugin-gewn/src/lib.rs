use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use matrix_sdk::attachment::AttachmentConfig;
use mime::{APPLICATION_OCTET_STREAM, IMAGE_GIF, IMAGE_JPEG, IMAGE_PNG, Mime};
use plugin_core::{
    Plugin, PluginContext, PluginSpec, PluginTriggers, factory::PluginFactory, send_text,
};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, warn};

#[derive(Debug)]
pub struct GewnPlugin;

impl PluginFactory for GewnPlugin {
    fn register_defaults(&self, specs: &mut Vec<PluginSpec>) {
        let config = serde_yaml::to_value(GewnConfig::default()).unwrap_or_default();
        specs.push(PluginSpec {
            id: "gewn".to_owned(),
            enabled: true,
            dev_only: None,
            triggers: PluginTriggers {
                commands: vec!["!gewn".to_owned()],
                mentions: vec![],
            },
            config,
        });
    }

    fn build(&self) -> Arc<dyn Plugin + Send + Sync> {
        Arc::new(Gewn)
    }
}

#[derive(Debug)]
pub struct Gewn;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct GewnConfig {
    directory: PathBuf,
    caption_template: Option<String>,
    extensions: Vec<String>,
    recursive: bool,
    fallback_text: Option<String>,
}

impl Default for GewnConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("./plugins/gewn/images"),
            caption_template: None,
            extensions: vec![
                "jpg".to_owned(),
                "jpeg".to_owned(),
                "png".to_owned(),
                "gif".to_owned(),
                "webp".to_owned(),
            ],
            recursive: false,
            fallback_text: Some(
                "No gewn images found. Add files to ./plugins/gewn/images or update plugins/gewn/config.yaml.".to_owned(),
            ),
        }
    }
}

fn parse_config(spec: &PluginSpec) -> GewnConfig {
    match serde_yaml::from_value::<GewnConfig>(spec.config.clone()) {
        Ok(cfg) => cfg,
        Err(err) => {
            warn!(plugin = "gewn", error = %err, "Failed to parse gewn config, using defaults");
            GewnConfig::default()
        }
    }
}

fn normalize_extensions(list: &[String]) -> Option<HashSet<String>> {
    if list.is_empty() {
        return None;
    }
    let set = list
        .iter()
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect::<HashSet<String>>();
    if set.is_empty() { None } else { Some(set) }
}

fn extension_allowed(exts: &Option<HashSet<String>>, path: &Path) -> bool {
    match exts {
        None => true,
        Some(allowed) => path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                let lowered = ext.to_ascii_lowercase();
                allowed.contains(lowered.as_str())
            })
            .unwrap_or(false),
    }
}

async fn collect_files(config: &GewnConfig) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![config.directory.clone()];
    let exts = normalize_extensions(&config.extensions);

    while let Some(dir) = stack.pop() {
        let mut reader = match fs::read_dir(&dir).await {
            Ok(reader) => reader,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                debug!(directory = %dir.display(), "gewn directory missing");
                continue;
            }
            Err(err) => {
                warn!(directory = %dir.display(), error = %err, "Failed to read gewn directory");
                continue;
            }
        };

        while let Some(entry) = reader.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                if config.recursive {
                    stack.push(path);
                }
            } else if file_type.is_file() && extension_allowed(&exts, &path) {
                files.push(path);
            }
        }
    }

    Ok(files)
}

fn guess_mime(path: &Path) -> Mime {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => IMAGE_JPEG,
        Some("png") => IMAGE_PNG,
        Some("gif") => IMAGE_GIF,
        Some("webp") => "image/webp".parse().unwrap_or(APPLICATION_OCTET_STREAM),
        Some("bmp") => "image/bmp".parse().unwrap_or(APPLICATION_OCTET_STREAM),
        Some("heic") => "image/heic".parse().unwrap_or(APPLICATION_OCTET_STREAM),
        _ => APPLICATION_OCTET_STREAM,
    }
}

fn render_caption(config: &GewnConfig, path: &Path) -> Option<String> {
    let template = config.caption_template.as_ref()?;
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("gewn");
    Some(template.replace("{filename}", file_name))
}

#[async_trait]
impl Plugin for Gewn {
    fn id(&self) -> &'static str {
        "gewn"
    }

    fn help(&self) -> &'static str {
        "Send a random gewn picture (-- configurable directory/caption)."
    }

    async fn run(&self, ctx: &PluginContext, _args: &str, spec: &PluginSpec) -> Result<()> {
        let config = parse_config(spec);
        let candidates = collect_files(&config).await?;

        if candidates.is_empty() {
            if let Some(message) = config.fallback_text {
                send_text(ctx, message).await?;
            }
            return Ok(());
        }

        let chosen = {
            use rand::seq::SliceRandom;
            let mut rng = rand::thread_rng();
            candidates
                .choose(&mut rng)
                .cloned()
                .unwrap_or_else(|| candidates[0].clone())
        };

        let data = fs::read(&chosen)
            .await
            .with_context(|| format!("reading image {}", chosen.display()))?;

        let body = chosen
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("gewn")
            .to_owned();

        let mime = guess_mime(&chosen);
        ctx.room
            .send_attachment(&body, &mime, data, AttachmentConfig::new())
            .await
            .with_context(|| format!("sending gewn attachment {}", chosen.display()))?;

        if let Some(caption) = render_caption(&config, &chosen) {
            send_text(ctx, caption).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_extensions_normalize() {
        let cfg = GewnConfig::default();
        let normalized = normalize_extensions(&cfg.extensions).unwrap();
        assert!(normalized.contains("jpg"));
        assert!(normalized.contains("png"));
    }

    #[test]
    fn caption_template_renders_filename() {
        let mut cfg = GewnConfig::default();
        cfg.caption_template = Some("hello {filename}".to_owned());
        let caption = render_caption(&cfg, Path::new("/tmp/example.png"));
        assert_eq!(caption.as_deref(), Some("hello example.png"));
    }
}
