use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use tools::{Tool, ToolContext, ToolSpec, ToolTriggers, send_text};

use tools::plugin_trait::Plugin;

pub struct EchoPlugin;

impl Plugin for EchoPlugin {
    fn register_defaults(&self, specs: &mut Vec<ToolSpec>) {
        if !specs.iter().any(|t| t.id == "echo") {
            specs.push(ToolSpec {
                id: "echo".into(),
                enabled: true,
                dev_only: None,
                triggers: ToolTriggers {
                    commands: vec!["!echo".into()],
                    mentions: vec![],
                },
                config: serde_yaml::Value::default(),
            });
        }
    }

    fn build(&self) -> Arc<dyn Tool> {
        Arc::new(EchoTool)
    }
}

pub struct EchoTool;

#[derive(Debug, Clone, Deserialize, Default)]
struct EchoConfig {
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    uppercase: bool,
}

#[async_trait]
impl Tool for EchoTool {
    fn id(&self) -> &'static str {
        "echo"
    }
    fn help(&self) -> &'static str {
        "Echo text back. Config: prefix, uppercase"
    }
    async fn run(&self, ctx: &ToolContext, args: &str, spec: &ToolSpec) -> Result<()> {
        let cfg: EchoConfig = serde_yaml::from_value(spec.config.clone()).unwrap_or_default();
        let mut out = args.trim().to_owned();
        if cfg.uppercase {
            out = out.to_uppercase();
        }
        if let Some(p) = cfg.prefix {
            format!("{p}{out}").clone_into(&mut out);
        }
        if out.is_empty() {
            "(nothing to echo)".clone_into(&mut out);
        }
        send_text(ctx, out).await
    }
}
