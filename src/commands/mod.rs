use std::{string::ToString, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::{Client, room::Room, ruma::events::room::message::RoomMessageEventContent};

use tracing::warn;

#[async_trait]
pub trait Command: Send + Sync {
    fn name(&self) -> &'static str;
    fn help(&self) -> &'static str;
    fn dev_only(&self) -> bool {
        false
    }
    async fn run(&self, ctx: &CommandContext, args: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct CommandContext {
    pub client: Client,
    pub room: Room,
    pub commands: Arc<CommandMap>,
    pub dev_active: bool,
}

pub type CommandMap = std::collections::HashMap<String, Arc<dyn Command>>;

pub fn default_registry() -> CommandMap {
    let mut map: CommandMap = CommandMap::new();
    map.insert("!ping".to_owned(), Arc::new(PingCommand));
    map.insert("!diag".to_owned(), Arc::new(DiagCommand));
    map.insert("!help".to_owned(), Arc::new(HelpCommand));
    map.insert("!mode".to_owned(), Arc::new(ModeCommand));
    map
}

pub struct PingCommand;

#[async_trait]
impl Command for PingCommand {
    fn name(&self) -> &'static str {
        "!ping"
    }
    fn help(&self) -> &'static str {
        "Responds with pong."
    }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        send_text(ctx, "pong").await
    }
}

pub struct DiagCommand;

#[async_trait]
impl Command for DiagCommand {
    fn name(&self) -> &'static str {
        "!diag"
    }
    fn help(&self) -> &'static str {
        "Show encryption/session diagnostics."
    }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        let user_id = ctx
            .client
            .user_id()
            .map_or("<unknown>".to_owned(), ToString::to_string);
        let device_id = ctx
            .client
            .device_id()
            .map_or("<unknown>".to_owned(), ToString::to_string);

        let is_encrypted = match ctx.room.latest_encryption_state().await {
            Ok(b) => b.is_encrypted(),
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
        if let Some(v) = bot_verified {
            lines.push(format!("bot_verified: {v}"));
        }

        if is_encrypted {
            lines.push(
                "hint: if messages don’t decrypt, verify the bridge/device and send a new message."
                    .to_owned(),
            );
        } else {
            lines.push(
                "hint: room not encrypted; encryption diagnostics not applicable.".to_owned(),
            );
        }

        let msg = lines.join("\n");
        if let Err(e) = send_text(ctx, msg).await {
            warn!(error = %e, "Failed to send diag");
        }
        Ok(())
    }
}

pub struct HelpCommand;

#[async_trait]
impl Command for HelpCommand {
    fn name(&self) -> &'static str {
        "!help"
    }
    fn help(&self) -> &'static str {
        "List available commands."
    }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        let mut pairs: Vec<(String, String)> = ctx
            .commands
            .values()
            .filter(|cmd| !cmd.dev_only() || ctx.dev_active)
            .map(|cmd| (cmd.name().to_owned(), cmd.help().to_owned()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut lines = vec!["Available commands:".to_owned()];
        for (name, help) in pairs {
            lines.push(format!("{name} — {help}"));
        }
        send_text(ctx, lines.join("\n")).await
    }
}

pub struct ModeCommand;

#[async_trait]
impl Command for ModeCommand {
    fn name(&self) -> &'static str {
        "!mode"
    }
    fn help(&self) -> &'static str {
        "Show current mode (dev/prod) and how to target it."
    }

    async fn run(&self, ctx: &CommandContext, _args: &str) -> Result<()> {
        let mode = if ctx.dev_active { "dev" } else { "prod" };
        let mut lines = vec![format!("mode: {}", mode)];
        if mode == "dev" {
            lines.push("this instance handles commands that include -d".to_owned());
            lines.push("example: !diag -d".to_owned());
        } else {
            lines.push("this instance handles commands without -d".to_owned());
            lines.push("example: !diag".to_owned());
        }
        send_text(ctx, lines.join("\n")).await
    }
}

fn decorate_dev(text: &str, dev_active: bool) -> String {
    if dev_active {
        format!("=======DEV MODE=======\n{text}")
    } else {
        text.to_owned()
    }
}

async fn send_text(ctx: &CommandContext, text: impl Into<String>) -> Result<()> {
    let out = decorate_dev(&text.into(), ctx.dev_active);
    let content = RoomMessageEventContent::text_plain(out);
    ctx.room.send(content).await?;
    Ok(())
}
