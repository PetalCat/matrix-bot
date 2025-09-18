pub mod plugin_trait;

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
    #[must_use]
    pub fn is_enabled(&self, id: &str) -> bool {
        let default = self.by_id.get(id).is_some_and(|e| e.spec.enabled);
        self.state
            .try_lock()
            .ok()
            .map_or(default, |m| m.get(id).copied().unwrap_or(default))
    }
}

pub fn str_conf(spec: &ToolSpec, key: &str) -> Option<String> {
    spec.config
        .get(key)
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[must_use]
fn decorate_dev(text: &str, dev_active: bool) -> String {
    if dev_active {
        format!("=======DEV MODE=======\n{text}")
    } else {
        text.to_owned()
    }
}

/// Send a text message to the room associated with the context.
///
/// # Errors
///
/// This function will return an error if sending the message fails.
pub async fn send_text(ctx: &ToolContext, text: impl Into<String>) -> Result<()> {
    let content = RoomMessageEventContent::text_plain(decorate_dev(&text.into(), ctx.dev_active));
    ctx.room.send(content).await?;
    Ok(())
}

#[must_use]
pub fn sanitize_line(s: &str, max: usize) -> String {
    let compact = s.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&compact, max)
}
