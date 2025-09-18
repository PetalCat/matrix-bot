use std::sync::Arc;

use anyhow::Result;

use async_trait::async_trait;
use tools::{Tool, ToolContext, ToolSpec, send_text};

use tools::plugin_trait::Plugin;

pub struct PingPlugin;

impl Plugin for PingPlugin {
    fn register_defaults(&self, specs: &mut Vec<tools::ToolSpec>) {
        specs.push(tools::ToolSpec {
            id: "ping".to_owned(),
            enabled: true,
            dev_only: None,
            triggers: tools::ToolTriggers {
                commands: vec!["!ping".to_owned()],
                mentions: vec![],
            },
            config: serde_yaml::Value::default(),
        });
    }

    fn build(&self) -> Arc<dyn Tool> {
        Arc::new(Ping)
    }
}

pub struct Ping;

#[async_trait]
impl Tool for Ping {
    fn id(&self) -> &'static str {
        "ping"
    }
    fn help(&self) -> &'static str {
        "🏓"
    }

    async fn run(&self, ctx: &ToolContext, _args: &str, _spec: &ToolSpec) -> Result<()> {
        send_text(ctx, "Pong! 🏓".to_owned()).await
    }
}
