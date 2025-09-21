use std::{collections::HashMap, sync::Arc};

use crate::{BotConfig, RoomCluster};
use plugin_core::{PluginRegistry, PluginSpec, PluginTriggers, factory::PluginFactory};
use plugin_relay::{RelayConfig, RelayPlugin};
use tracing::warn;

// TODO: Why have both FactoryRegistry and PluginRegistry? Can we merge them?
struct FactoryRegistry {
    factories: HashMap<String, Box<dyn PluginFactory + Send + Sync>>,
}

impl FactoryRegistry {
    fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    fn with_factory<F: PluginFactory + Send + Sync + 'static>(
        mut self,
        name: impl Into<String>,
        factory: F,
    ) -> Self {
        self.factories.insert(name.into(), Box::new(factory));
        self
    }

    fn get(&self, name: &str) -> Option<&(dyn PluginFactory + Send + Sync)> {
        self.factories.get(name).map(|f| &**f)
    }
}

pub async fn build_registry(config: &BotConfig) -> Arc<PluginRegistry> {
    let factories = FactoryRegistry::new()
        .with_factory("ping", plugin_ping::PingPlugin)
        .with_factory("mode", plugin_mode::ModePlugin)
        .with_factory("diag", plugin_diagnostics::DiagnosticsPlugin)
        .with_factory("tools", plugin_tools_manager::ToolsManagerPlugin)
        .with_factory("ai", plugin_ai::AiPlugin)
        .with_factory("echo", plugin_echo::EchoPlugin)
        .with_factory("relay", RelayPlugin);

    let mut specs = config.plugins.clone().unwrap_or_default();

    // Inject relay plugin configuration if clusters are defined and no explicit spec exists.
    if !specs.iter().any(|s| s.id == "relay") && !config.clusters.is_empty() {
        let relay_config = RelayConfig {
            clusters: config.clusters.iter().map(cluster_from_bot).collect(),
            reupload_media: config.reupload_media,
            caption_media: config.caption_media,
        };
        let config_value = serde_yaml::to_value(relay_config).unwrap_or_default();
        specs.push(PluginSpec {
            id: "relay".to_owned(),
            enabled: true,
            dev_only: None,
            triggers: PluginTriggers::default(),
            config: config_value,
        });
    }

    for factory in factories.factories.values() {
        factory.register_defaults(&mut specs);
    }

    let registry = Arc::new(PluginRegistry::new());
    let default_dir = if std::path::Path::new("./plugins").exists() {
        "./plugins".to_owned()
    } else {
        "./tools".to_owned()
    };
    let plugins_dir = std::env::var("PLUGINS_DIR")
        .or_else(|_| std::env::var("TOOLS_DIR"))
        .unwrap_or(default_dir);

    for mut spec in specs {
        let Some(factory) = factories.get(spec.id.as_str()) else {
            warn!("Unknown plugin ID: {}", spec.id);
            continue;
        };
        let plugin = factory.build();
        if let Some(file_cfg) = load_plugin_config(&plugins_dir, spec.id.as_str()) {
            spec.config = merge_yaml(file_cfg, spec.config);
        }
        registry.register(spec, plugin).await;
    }

    registry
}

fn cluster_from_bot(cluster: &RoomCluster) -> plugin_relay::RelayCluster {
    plugin_relay::RelayCluster {
        rooms: cluster.rooms.clone(),
        reupload_media: cluster.reupload_media,
        caption_media: cluster.caption_media,
    }
}

fn merge_yaml(file_cfg: serde_yaml::Value, spec_cfg: serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value::{Mapping, Sequence};
    match (file_cfg, spec_cfg) {
        (Mapping(mut a), Mapping(b)) => {
            for (k, v_b) in b {
                match a.get_mut(&k) {
                    Some(v_a) => {
                        let merged = merge_yaml(v_a.clone(), v_b);
                        *v_a = merged;
                    }
                    None => {
                        a.insert(k, v_b);
                    }
                }
            }
            Mapping(a)
        }
        (Sequence(mut a), Sequence(b)) => {
            a.extend(b);
            Sequence(a)
        }
        (a, _b) => a,
    }
}

fn load_plugin_config(root: &str, id: &str) -> Option<serde_yaml::Value> {
    let root = root.trim_end_matches('/');
    let path = format!("{root}/{id}/config.yaml");
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_yaml::from_str::<serde_yaml::Value>(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(plugin = %id, file = %path, error = %e, "Failed to parse plugin config YAML");
                None
            }
        },
        Err(e) => {
            if std::path::Path::new(&path).exists() {
                tracing::warn!(plugin = %id, file = %path, error = %e, "Failed to read plugin config file");
            }
            None
        }
    }
}
