// Tool modules
pub mod ai;
pub mod diag;
pub mod echo;
pub mod mode;
pub mod tools_mgr;

use std::{borrow::ToOwned, collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::{Client, room::Room, ruma::events::room::message::RoomMessageEventContent};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ToolContext {
    pub client: Client,
    pub room: Room,
    pub dev_active: bool,
    pub registry: Arc<ToolsRegistry>,
    pub history_dir: Arc<PathBuf>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> &'static str;
    fn help(&self) -> &'static str;
    fn dev_only(&self) -> bool {
        false
    }
    async fn run(&self, ctx: &ToolContext, args: &str, spec: &ToolSpec) -> Result<()>;
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolTriggers {
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolSpec {
    pub id: String,
    #[serde(default = "enabled_true")]
    pub enabled: bool,
    #[serde(default)]
    pub dev_only: Option<bool>,
    #[serde(default)]
    pub triggers: ToolTriggers,
    #[serde(default)]
    pub config: serde_yaml::Value,
}

const fn enabled_true() -> bool {
    true
}

pub struct ToolEntry {
    pub spec: ToolSpec,
    pub tool: Arc<dyn Tool>,
}

#[derive(Clone)]
pub struct ToolsRegistry {
    pub by_id: Arc<HashMap<String, ToolEntry>>,
    pub by_command: Arc<HashMap<String, String>>, // command -> id
    pub by_mention: Arc<HashMap<String, String>>, // mention -> id
    pub state: Arc<Mutex<HashMap<String, bool>>>, // runtime enabled overrides
}

impl ToolsRegistry {
    pub fn is_enabled(&self, id: &str) -> bool {
        let default = self.by_id.get(id).is_some_and(|e| e.spec.enabled);
        self.state
            .try_lock()
            .ok()
            .map_or(default, |m| m.get(id).copied().unwrap_or(default))
    }
}

fn str_conf(spec: &ToolSpec, key: &str) -> Option<String> {
    spec.config
        .get(key)
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn decorate_dev(text: &str, dev_active: bool) -> String {
    if dev_active {
        format!("=======DEV MODE=======\n{text}")
    } else {
        text.to_owned()
    }
}

async fn send_text(ctx: &ToolContext, text: impl Into<String>) -> Result<()> {
    let content = RoomMessageEventContent::text_plain(decorate_dev(&text.into(), ctx.dev_active));
    ctx.room.send(content).await?;
    Ok(())
}

pub fn sanitize_line(s: &str, max: usize) -> String {
    let compact = s.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&compact, max)
}

pub fn build_registry(
    config_tools: Option<Vec<ToolSpec>>,
    env_ai_handle: Option<String>,
) -> ToolsRegistry {
    let mut by_id: HashMap<String, ToolEntry> = HashMap::new();
    let mut by_command: HashMap<String, String> = HashMap::new();
    let mut by_mention: HashMap<String, String> = HashMap::new();

    // defaults from each tool module
    let mut specs = config_tools.unwrap_or_default();
    mode::register_defaults(&mut specs);
    diag::register_defaults(&mut specs);
    tools_mgr::register_defaults(&mut specs);
    ai::register_defaults(&mut specs);
    echo::register_defaults(&mut specs);
    if let Some(h) = env_ai_handle {
        append_mention(&mut specs, "ai", &h);
    }
    // If ai has no mention trigger yet, derive one from name (config or AI_NAME) or default to @Claire
    if !specs
        .iter()
        .any(|t| t.id == "ai" && !t.triggers.mentions.is_empty())
        && let Some(ai_spec) = specs.iter_mut().find(|t| t.id == "ai")
    {
        let name = ai_spec
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| std::env::var("AI_NAME").ok())
            .unwrap_or_else(|| "Claire".to_owned());
        ai_spec.triggers.mentions.push(format!("@{name}"));
    }

    // tool configs directory (can override via TOOLS_DIR). Default to src/tools next to code.
    let tools_dir = std::env::var("TOOLS_DIR").unwrap_or_else(|_| "./src/tools".to_owned());

    for mut spec in specs {
        let id = spec.id.clone();
        let tool: Arc<dyn Tool> = match id.as_str() {
            "mode" => mode::build(),
            "diag" => diag::build(),
            "ai" => ai::build(),
            "tools" => tools_mgr::build(),
            "echo" => echo::build(),
            _ => {
                // unknown tool id
                continue;
            }
        };
        // Load per-tool config from tools_dir/<id>/config.yaml and merge.
        if let Some(file_cfg) = load_tool_config(&tools_dir, &id) {
            spec.config = merge_yaml(file_cfg, spec.config); // file takes precedence
        }
        by_command.extend(
            spec.triggers
                .commands
                .iter()
                .map(|c| (normalize_cmd(c), id.clone())),
        );
        by_mention.extend(
            spec.triggers
                .mentions
                .iter()
                .map(|m| (normalize_mention(m), id.clone())),
        );
        by_id.insert(id, ToolEntry { spec, tool });
    }

    ToolsRegistry {
        by_id: Arc::new(by_id),
        by_command: Arc::new(by_command),
        by_mention: Arc::new(by_mention),
        state: Arc::new(Mutex::new(HashMap::new())),
    }
}

fn normalize_cmd(s: &str) -> String {
    if s.starts_with('!') {
        s.to_owned()
    } else {
        format!("!{s}")
    }
}
fn normalize_mention(s: &str) -> String {
    let raw = if s.starts_with('@') {
        s.to_owned()
    } else {
        format!("@{s}")
    };
    raw.to_lowercase()
}

fn append_mention(specs: &mut [ToolSpec], id: &str, mention: &str) {
    if let Some(t) = specs.iter_mut().find(|t| t.id == id) {
        t.triggers.mentions.push(mention.to_owned());
    }
}

fn load_tool_config(root: &str, id: &str) -> Option<serde_yaml::Value> {
    let path = format!("{}/{}/config.yaml", root.trim_end_matches('/'), id);
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_yaml::from_str::<serde_yaml::Value>(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(tool = %id, file = %path, error = %e, "Failed to parse tool config YAML");
                None
            }
        },
        Err(e) => {
            // Only log if file exists but couldn't be read; otherwise silent if not found
            if std::path::Path::new(&path).exists() {
                tracing::warn!(tool = %id, file = %path, error = %e, "Failed to read tool config file");
            }
            None
        }
    }
}

fn merge_yaml(file_cfg: serde_yaml::Value, spec_cfg: serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value::{Mapping, Sequence};
    match (file_cfg, spec_cfg) {
        (Mapping(mut a), Mapping(b)) => {
            for (k, v_b) in b {
                match a.get_mut(&k) {
                    Some(v_a) => {
                        let merged = merge_yaml(v_a.clone(), v_b);
                        *v_a = merged;
                    }
                    None => {
                        a.insert(k, v_b);
                    }
                }
            }
            Mapping(a)
        }
        (Sequence(mut a), Sequence(b)) => {
            a.extend(b);
            Sequence(a)
        }
        // By default, prefer file config value when types differ or non-mapping
        (a, _b) => a,
    }
}
