use std::sync::Arc;

use anyhow::Result;

use async_trait::async_trait;
use plugin_core::factory::PluginFactory;
use plugin_core::{Plugin, PluginContext, PluginSpec, PluginTriggers, send_text};

#[derive(Debug)]
pub struct PingPlugin;

impl PluginFactory for PingPlugin {
    fn register_defaults(&self, specs: &mut Vec<PluginSpec>) {
        specs.push(PluginSpec {
            id: "ping".to_owned(),
            enabled: true,
            dev_only: None,
            triggers: PluginTriggers {
                commands: vec!["!ping".to_owned()],
                mentions: vec![],
            },
            config: serde_yaml::Value::default(),
        });
    }

    fn build(&self) -> Arc<dyn Plugin + Send + Sync> {
        Arc::new(Ping)
    }
}

#[derive(Debug)]
pub struct Ping;

#[async_trait]
impl Plugin for Ping {
    fn id(&self) -> &'static str {
        "ping"
    }
    fn help(&self) -> &'static str {
        "🏓"
    }

    async fn run(&self, ctx: &PluginContext, _args: &str, _spec: &PluginSpec) -> Result<()> {
        send_text(ctx, "Pong! 🏓".to_owned()).await
    }
}
