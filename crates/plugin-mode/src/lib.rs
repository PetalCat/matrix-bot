use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use tools::{Tool, ToolContext, ToolSpec, ToolTriggers, send_text};

use tools::plugin_trait::Plugin;

pub struct ModePlugin;

impl Plugin for ModePlugin {
    fn register_defaults(&self, specs: &mut Vec<ToolSpec>) {
        if !specs.iter().any(|t| t.id == "mode") {
            specs.push(ToolSpec {
                id: "mode".into(),
                enabled: true,
                dev_only: None,
                triggers: ToolTriggers {
                    commands: vec!["!mode".into()],
                    mentions: vec![],
                },
                config: serde_yaml::Value::default(),
            });
        }
    }

    fn build(&self) -> Arc<dyn Tool> {
        Arc::new(ModeTool)
    }
}

pub struct ModeTool;

#[async_trait]
impl Tool for ModeTool {
    fn id(&self) -> &'static str {
        "mode"
    }
    fn help(&self) -> &'static str {
        "Show current mode (dev/prod) and how to target it."
    }
    async fn run(&self, ctx: &ToolContext, _args: &str, _spec: &ToolSpec) -> Result<()> {
        let mode = if ctx.dev_active { "dev" } else { "prod" };
        let mut lines = vec![format!("mode: {}", mode)];
        if ctx.dev_active {
            if let Some(dev_id) = ctx.dev_id.as_deref() {
                lines.push(format!(
                    "this instance handles commands tagged as !{dev_id}.<command>"
                ));
                lines.push(format!("example: !{dev_id}.diag"));
                lines.push(format!("mentions must use @{dev_id}.<name>"));
            } else {
                lines.push("this instance handles commands routed to dev".to_owned());
                lines.push("example: !devid.diag".to_owned());
            }
        } else {
            if let Some(dev_id) = ctx.dev_id.as_deref() {
                lines.push(format!("commands without !{dev_id}. prefix run here"));
                lines.push(format!(
                    "commands containing !{dev_id}.<command> are ignored"
                ));
            } else {
                lines.push("this instance handles commands without a dev prefix".to_owned());
            }
            lines.push("example: !diag".to_owned());
        }
        send_text(ctx, lines.join("\n")).await
    }
}
