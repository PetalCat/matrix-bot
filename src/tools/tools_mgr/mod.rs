use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::tools::{Tool, ToolContext, ToolSpec, ToolTriggers, send_text};

pub fn register_defaults(specs: &mut Vec<ToolSpec>) {
    if !specs.iter().any(|t| t.id == "tools") {
        specs.push(ToolSpec {
            id: "tools".into(),
            enabled: true,
            dev_only: None,
            triggers: ToolTriggers {
                commands: vec!["!tools".into()],
                mentions: vec![],
            },
            config: serde_yaml::Value::default(),
        });
    }
}

pub fn build() -> Arc<dyn Tool> {
    Arc::new(ToolsManager)
}

pub struct ToolsManager;

#[async_trait]
impl Tool for ToolsManager {
    fn id(&self) -> &'static str {
        "tools"
    }
    fn help(&self) -> &'static str {
        "Manage tools: !tools list | enable <id> | disable <id>"
    }
    async fn run(&self, ctx: &ToolContext, args: &str, _spec: &ToolSpec) -> Result<()> {
        let registry = &ctx.registry;
        let mut parts = args.split_whitespace();
        match parts.next() {
            Some("list") | None => {
                let mut rows = vec!["tools:".to_owned()];
                for (id, entry) in registry.by_id.iter() {
                    let enabled = registry.is_enabled(id);
                    #[allow(clippy::or_fun_call, reason = "const fn")]
                    let dev_only = entry.spec.dev_only.unwrap_or(entry.tool.dev_only());
                    let triggers = format!(
                        "cmds=[{}], mentions=[{}]",
                        entry.spec.triggers.commands.join(", "),
                        entry.spec.triggers.mentions.join(", ")
                    );
                    rows.push(format!(
                        "- {id}: enabled={enabled} dev_only={dev_only} {triggers}",
                    ));
                }
                send_text(ctx, rows.join("\n")).await
            }
            Some("enable") => {
                if let Some(id) = parts.next() {
                    registry.state.lock().await.insert(id.to_owned(), true);
                    send_text(ctx, format!("enabled tool: {id}")).await
                } else {
                    send_text(ctx, "Usage: !tools enable <id>").await
                }
            }
            Some("disable") => {
                if let Some(id) = parts.next() {
                    registry.state.lock().await.insert(id.to_owned(), false);
                    send_text(ctx, format!("disabled tool: {id}")).await
                } else {
                    send_text(ctx, "Usage: !tools disable <id>").await
                }
            }
            _ => send_text(ctx, "Usage: !tools [list|enable <id>|disable <id>]").await,
        }
    }
}
