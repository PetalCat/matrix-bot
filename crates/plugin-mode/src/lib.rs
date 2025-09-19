use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use plugin_core::factory::PluginFactory;
use plugin_core::{Plugin, PluginContext, PluginSpec, PluginTriggers, send_text};

pub struct ModePlugin;

impl PluginFactory for ModePlugin {
    fn register_defaults(&self, specs: &mut Vec<PluginSpec>) {
        if !specs.iter().any(|t| t.id == "mode") {
            specs.push(PluginSpec {
                id: "mode".into(),
                enabled: true,
                dev_only: None,
                triggers: PluginTriggers {
                    commands: vec!["!mode".into()],
                    mentions: vec![],
                },
                config: serde_yaml::Value::default(),
            });
        }
    }

    fn build(&self) -> Arc<dyn Plugin> {
        Arc::new(ModeTool)
    }
}

pub struct ModeTool;

#[async_trait]
impl Plugin for ModeTool {
    fn id(&self) -> &'static str {
        "mode"
    }
    fn help(&self) -> &'static str {
        "Show current mode (dev/prod) and how to target it."
    }
    async fn run(&self, ctx: &PluginContext, _args: &str, _spec: &PluginSpec) -> Result<()> {
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
