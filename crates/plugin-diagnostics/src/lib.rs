use std::{string::ToString, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;

use plugin_core::factory::PluginFactory;
use plugin_core::{Plugin, PluginContext, PluginSpec, PluginTriggers, send_text};

pub struct DiagnosticsPlugin;

impl PluginFactory for DiagnosticsPlugin {
    fn register_defaults(&self, specs: &mut Vec<PluginSpec>) {
        if !specs.iter().any(|t| t.id == "diag") {
            specs.push(PluginSpec {
                id: "diag".into(),
                enabled: true,
                dev_only: None,
                triggers: PluginTriggers {
                    commands: vec!["!diag".into()],
                    mentions: vec![],
                },
                config: serde_yaml::Value::default(),
            });
        }
    }

    fn build(&self) -> Arc<dyn Plugin> {
        Arc::new(DiagTool)
    }
}

pub struct DiagTool;

#[async_trait]
impl Plugin for DiagTool {
    fn id(&self) -> &'static str {
        "diag"
    }
    fn help(&self) -> &'static str {
        "Show encryption/session diagnostics."
    }
    async fn run(&self, ctx: &PluginContext, _args: &str, _spec: &PluginSpec) -> Result<()> {
        let user_id = ctx
            .client
            .user_id()
            .map_or_else(|| "<unknown>".to_owned(), ToString::to_string);
        let device_id = ctx
            .client
            .device_id()
            .map_or_else(|| "<unknown>".to_owned(), ToString::to_string);
        let is_encrypted = ctx
            .room
            .latest_encryption_state()
            .await
            .map(|s| s.is_encrypted())
            .unwrap_or(false);
        let bot_verified = if let Ok(Some(dev)) = ctx.client.encryption().get_own_device().await {
            Some(dev.is_verified())
        } else {
            None
        };
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
        send_text(ctx, lines.join("\n")).await
    }
}
