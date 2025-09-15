use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::{Client};
use matrix_sdk::room::Room;
use ruma::events::room::message::RoomMessageEventContent;
use tracing::{info, warn};

#[async_trait]
pub trait Command: Send + Sync {
    fn name(&self) -> &'static str;
    fn help(&self) -> &'static str;
    fn dev_only(&self) -> bool { false }
    async fn run(&self, ctx: &CommandContext, args: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct CommandContext {
    pub client: Client,
    pub room: Room,
    pub sender: String,
    pub commands: Arc<CommandMap>,
    pub dev_active: bool,
}

pub type CommandMap = std::collections::HashMap<String, Arc<dyn Command>>;

pub fn default_registry() -> CommandMap {
    let mut map: CommandMap = CommandMap::new();
    map.insert("!ping".into(), Arc::new(PingCommand));
    map.insert("!diag".into(), Arc::new(DiagCommand));
    map.insert("!help".into(), Arc::new(HelpCommand));
    map.insert("!mode".into(), Arc::new(ModeCommand));
    map
}

pub struct PingCommand;

#[async_trait]
impl Command for PingCommand {
    fn name(&self) -> &'static str { "!ping" }
    fn help(&self) -> &'static str { "Responds with pong." }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        let content = RoomMessageEventContent::text_plain("pong");
        ctx.room.send(content).await?;
        Ok(())
    }
}

pub struct DiagCommand;

#[async_trait]
impl Command for DiagCommand {
    fn name(&self) -> &'static str { "!diag" }
    fn help(&self) -> &'static str { "Show encryption/session diagnostics." }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        let user_id = match ctx.client.user_id() { Some(u) => u.to_string(), None => "<unknown>".into() };
        let device_id = ctx
            .client
            .device_id()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "<unknown>".into());

        let is_encrypted = match ctx.room.is_encrypted().await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, room_id = %ctx.room.room_id(), "Failed to check encryption state");
                false
            }
        };

        let mut bot_verified = None;
        if let Ok(Some(dev)) = ctx.client.encryption().get_own_device().await {
            bot_verified = Some(dev.is_verified());
        }
        let backup_state = format!("{:?}", ctx.client.encryption().backups().state());

        let mut lines = vec![
            format!("diag for {}", ctx.room.room_id()),
            format!("user: {}", user_id),
            format!("device: {}", device_id),
            format!("room_encrypted: {}", is_encrypted),
            format!("backup_state: {}", backup_state),
        ];
        if let Some(v) = bot_verified { lines.push(format!("bot_verified: {}", v)); }

        if is_encrypted {
            lines.push("hint: if messages don’t decrypt, verify the bridge/device and send a new message.".into());
        } else {
            lines.push("hint: room not encrypted; encryption diagnostics not applicable.".into());
        }

        let msg = lines.join("\n");
        let content = RoomMessageEventContent::text_plain(msg);
        if let Err(e) = ctx.room.send(content).await {
            warn!(error = %e, "Failed to send diag");
        }
        Ok(())
    }
}

pub struct HelpCommand;

#[async_trait]
impl Command for HelpCommand {
    fn name(&self) -> &'static str { "!help" }
    fn help(&self) -> &'static str { "List available commands." }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        let mut pairs: Vec<(String, String)> = ctx
            .commands
            .values()
            .filter(|cmd| !cmd.dev_only() || ctx.dev_active)
            .map(|cmd| (cmd.name().to_string(), cmd.help().to_string()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut lines = vec!["Available commands:".to_string()];
        for (name, help) in pairs {
            lines.push(format!("{} — {}", name, help));
        }
        let content = RoomMessageEventContent::text_plain(lines.join("\n"));
        ctx.room.send(content).await?;
        Ok(())
    }
}

pub struct ModeCommand;

#[async_trait]
impl Command for ModeCommand {
    fn name(&self) -> &'static str { "!mode" }
    fn help(&self) -> &'static str { "Show current mode (dev/prod) and how to target it." }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        let mode = if ctx.dev_active { "dev" } else { "prod" };
        let mut lines = vec![format!("mode: {}", mode)];
        match mode {
            "dev" => {
                lines.push("this instance handles commands that include -d".into());
                lines.push("example: !diag -d".into());
            }
            _ => {
                lines.push("this instance handles commands without -d".into());
                lines.push("example: !diag".into());
            }
        }
        let content = RoomMessageEventContent::text_plain(lines.join("\n"));
        ctx.room.send(content).await?;
        Ok(())
    }
}
