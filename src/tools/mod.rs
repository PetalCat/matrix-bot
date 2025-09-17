use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::{Client};
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ToolContext {
    pub client: Client,
    pub room: Room,
    pub dev_active: bool,
    pub registry: Arc<ToolsRegistry>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> &'static str;
    fn help(&self) -> &'static str;
    fn dev_only(&self) -> bool { false }
    async fn run(&self, ctx: &ToolContext, args: &str, spec: &ToolSpec) -> Result<()>;
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolTriggers {
    #[serde(default)] pub commands: Vec<String>,
    #[serde(default)] pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolSpec {
    pub id: String,
    #[serde(default = "enabled_true")] pub enabled: bool,
    #[serde(default)] pub dev_only: Option<bool>,
    #[serde(default)] pub triggers: ToolTriggers,
    #[serde(default)] pub config: serde_yaml::Value,
}

fn enabled_true() -> bool { true }

pub struct ToolEntry {
    pub spec: ToolSpec,
    pub tool: Arc<dyn Tool>,
}

#[derive(Clone)]
pub struct ToolsRegistry {
    pub by_id: Arc<HashMap<String, ToolEntry>>,
    pub by_command: Arc<HashMap<String, String>>,  // command -> id
    pub by_mention: Arc<HashMap<String, String>>,  // mention -> id
    pub state: Arc<Mutex<HashMap<String, bool>>>,  // runtime enabled overrides
}

impl ToolsRegistry {
    pub fn is_enabled(&self, id: &str) -> bool {
        let default = self.by_id.get(id).map(|e| e.spec.enabled).unwrap_or(false);
        if let Some(m) = self.state.try_lock().ok() {
            m.get(id).copied().unwrap_or(default)
        } else { default }
    }
}

// subtool modules
pub mod mode;
pub mod diag;
pub mod ai;
pub mod tools_mgr;
pub mod echo;

fn str_conf(spec: &ToolSpec, key: &str) -> Option<String> {
    spec.config.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn truncate(s: &str, max: usize) -> String { s.chars().take(max).collect() }

fn decorate_dev(text: &str, dev_active: bool) -> String {
    if dev_active { format!("=======DEV MODE=======\n{}", text) } else { text.to_string() }
}

async fn send_text(ctx: &ToolContext, text: impl Into<String>) -> Result<()> {
    let content = RoomMessageEventContent::text_plain(decorate_dev(&text.into(), ctx.dev_active));
    ctx.room.send(content).await?;
    Ok(())
}

pub fn build_registry(config_tools: Option<Vec<ToolSpec>>, env_ai_handle: Option<String>) -> ToolsRegistry {
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
    if let Some(h) = env_ai_handle { append_mention(&mut specs, "ai", &h); }

    for spec in specs.into_iter() {
        let id = spec.id.clone();
        let tool: Arc<dyn Tool> = match id.as_str() {
            "mode" => mode::build(),
            "diag" => diag::build(),
            "ai" => ai::build(),
            "tools" => tools_mgr::build(),
            "echo" => echo::build(),
            _ => { continue; } // unknown tool id
        };
        by_command.extend(spec.triggers.commands.iter().map(|c| (normalize_cmd(c), id.clone())));
        by_mention.extend(spec.triggers.mentions.iter().map(|m| (normalize_mention(m), id.clone())));
        by_id.insert(id, ToolEntry { spec, tool });
    }

    ToolsRegistry { by_id: Arc::new(by_id), by_command: Arc::new(by_command), by_mention: Arc::new(by_mention), state: Arc::new(Mutex::new(HashMap::new())) }
}

fn normalize_cmd(s: &str) -> String { if s.starts_with('!') { s.to_string() } else { format!("!{}", s) } }
fn normalize_mention(s: &str) -> String { if s.starts_with('@') { s.to_string() } else { format!("@{}", s) } }

fn ensure_tool(specs: &mut Vec<ToolSpec>, id: &str, default_cmds: Vec<&str>, default_mentions: Vec<&str>) {
    if specs.iter().any(|t| t.id == id) { return; }
    specs.push(ToolSpec { id: id.to_string(), enabled: true, dev_only: None, triggers: ToolTriggers { commands: default_cmds.into_iter().map(|s| s.to_string()).collect(), mentions: default_mentions.into_iter().map(|s| s.to_string()).collect() }, config: serde_yaml::Value::default() });
}

fn append_mention(specs: &mut Vec<ToolSpec>, id: &str, mention: &str) {
    if let Some(t) = specs.iter_mut().find(|t| t.id == id) {
        t.triggers.mentions.push(mention.to_string());
    }
}
