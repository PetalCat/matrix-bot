use std::{string::ToString, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::{Client, room::Room, ruma::events::room::message::RoomMessageEventContent};
use serde::Deserialize;
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
    map.insert("!ai".to_owned(), Arc::new(AiCommand));
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

pub struct AiCommand;

#[async_trait]
impl Command for AiCommand {
    fn name(&self) -> &'static str {
        "!ai"
    }
    fn help(&self) -> &'static str {
        "Ask the AI: !ai <prompt> (dev only)"
    }
    fn dev_only(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &CommandContext, args: &str) -> Result<()> {
        let prompt = args.trim();
        if prompt.is_empty() {
            return send_text(ctx, "Usage: !ai <prompt>").await;
        }

        let api_key = std::env::var("AI_API_KEY")
            .ok()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());
        if api_key.is_none() {
            return send_text(ctx, "AI_API_KEY (or OPENAI_API_KEY) not set").await;
        }
        let api_key = api_key.unwrap();

        let api_base =
            std::env::var("AI_API_BASE").unwrap_or_else(|_| "https://api.openai.com".to_string());
        let api_path =
            std::env::var("AI_API_PATH").unwrap_or_else(|_| "/v1/chat/completions".to_string());
        let model = std::env::var("AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
        let url = format!("{}{}", api_base.trim_end_matches('/'), api_path);

        #[derive(Deserialize)]
        struct ChoiceMsg {
            content: Option<String>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: ChoiceMsg,
        }
        #[derive(Deserialize)]
        struct ChatResp {
            choices: Vec<Choice>,
        }

        #[derive(serde::Serialize)]
        struct Msg<'a> {
            role: &'a str,
            content: &'a str,
        }
        #[derive(serde::Serialize)]
        struct Body<'a> {
            model: &'a str,
            messages: Vec<Msg<'a>>,
            max_tokens: Option<u32>,
        }

        let body = Body {
            model: &model,
            messages: vec![Msg {
                role: "user",
                content: prompt,
            }],
            max_tokens: Some(512),
        };

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) => {
                if !r.status().is_success() {
                    let code = r.status();
                    let text = r.text().await.unwrap_or_default();
                    let msg = format!("AI error: {}\n{}", code, truncate_for_ai(&text, 400));
                    return send_text(ctx, msg).await;
                }
                match r.json::<ChatResp>().await {
                    Ok(parsed) => {
                        let out = parsed
                            .choices
                            .get(0)
                            .and_then(|c| c.message.content.as_ref())
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "<no content>".to_string());
                        send_text(ctx, out).await
                    }
                    Err(e) => send_text(ctx, format!("Failed to parse AI response: {}", e)).await,
                }
            }
            Err(e) => send_text(ctx, format!("Failed to call AI API: {}", e)).await,
        }
    }
}

fn truncate_for_ai(s: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in s.chars().take(max) {
        out.push(ch);
    }
    out
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
