use anyhow::Result;

use async_trait::async_trait;

use plugin_core::{Plugin, PluginContext, PluginSpec, PluginTriggers, send_text};

#[derive(Debug)]
pub struct Ping;

/// This crate is retained as a lightweight compatibility shim for existing
/// configurations that reference the `ping` plugin. The preferred replacement
/// is the more-generic `plugin-phrases` crate, which lets you configure many
/// command -> response mappings (for example: `ping: ["Pong! 🏓"]`).
#[async_trait]
impl Plugin for Ping {
    fn id(&self) -> &'static str {
        "ping"
    }

    fn help(&self) -> &'static str {
        "🏓 (DEPRECATED: prefer plugin-phrases)"
    }

    fn spec(&self, config: serde_yaml::Value) -> PluginSpec {
        PluginSpec {
            id: "ping".to_owned(),
            enabled: true,
            dev_only: None,
            triggers: PluginTriggers {
                commands: vec!["!ping".to_owned()],
                mentions: vec![],
            },
            config,
        }
    }

    async fn run(&self, ctx: &PluginContext, _args: &str, _spec: &PluginSpec) -> Result<()> {
        // Deprecated: prefer `plugin-phrases` for configurable replies.
        // Keep backward-compatible behaviour: reply with the classic Pong.
        send_text(ctx, "Pong! 🏓".to_owned()).await
    }
}
