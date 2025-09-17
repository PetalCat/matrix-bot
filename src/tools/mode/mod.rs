use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::tools::{Tool, ToolContext, ToolSpec, ToolTriggers, send_text};

pub fn register_defaults(specs: &mut Vec<ToolSpec>) {
    if !specs.iter().any(|t| t.id == "mode") {
        specs.push(ToolSpec {
            id: "mode".into(),
            enabled: true,
            dev_only: None,
            triggers: ToolTriggers { commands: vec!["!mode".into()], mentions: vec![] },
            config: serde_yaml::Value::default(),
        });
    }
}

pub fn build() -> Arc<dyn Tool> { Arc::new(ModeTool) }

pub struct ModeTool;

#[async_trait]
impl Tool for ModeTool {
    fn id(&self) -> &'static str { "mode" }
    fn help(&self) -> &'static str { "Show current mode (dev/prod) and how to target it." }
    async fn run(&self, ctx: &ToolContext, _args: &str, _spec: &ToolSpec) -> Result<()> {
        let mode = if ctx.dev_active { "dev" } else { "prod" };
        let mut lines = vec![format!("mode: {}", mode)];
        if ctx.dev_active {
            lines.push("this instance handles commands that include -d".to_string());
            lines.push("example: !diag -d".to_string());
        } else {
            lines.push("this instance handles commands without -d".to_string());
            lines.push("example: !diag".to_string());
        }
        send_text(ctx, lines.join("\n")).await
    }
}

