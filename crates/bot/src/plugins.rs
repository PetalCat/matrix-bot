use std::{collections::HashMap, sync::Arc};

use crate::{BotConfig, RoomCluster};
use plugin_core::{Plugin, PluginRegistry, PluginSpec, PluginTriggers};
use plugin_relay::{Relay, RelayConfig};
use tracing::warn;

pub async fn build_registry(config: &BotConfig) -> Arc<PluginRegistry> {
    // Build a map of plugin id -> instance. Plugins are stateless; one instance is fine.
    #[rustfmt::skip]
    let plugins: HashMap<&'static str, Arc<dyn Plugin + Send + Sync>> = HashMap::from([
        ("ping", Arc::new(plugin_ping::Ping) as Arc<dyn Plugin + Send + Sync>),
        ("mode", Arc::new(plugin_mode::ModeTool) as Arc<dyn Plugin + Send + Sync>),
        ("diag", Arc::new(plugin_diagnostics::DiagTool) as Arc<dyn Plugin + Send + Sync>),
        ("tools", Arc::new(plugin_tools_manager::ToolsManager) as Arc<dyn Plugin + Send + Sync>),
        ("ai", Arc::new(plugin_ai::AiTool) as Arc<dyn Plugin + Send + Sync>),
        ("echo", Arc::new(plugin_echo::EchoTool) as Arc<dyn Plugin + Send + Sync>),
        ("relay", Arc::new(Relay::default()) as Arc<dyn Plugin + Send + Sync>),
    ]);

    let mut specs = config.plugins.clone().unwrap_or_default();

    // Inject relay plugin configuration if clusters are defined and no explicit spec exists.
    if !specs.iter().any(|s| s.id == "relay") && !config.clusters.is_empty() {
        let relay_config = RelayConfig {
            clusters: config.clusters.iter().map(cluster_from_bot).collect(),
            reupload_media: config.reupload_media,
            caption_media: config.caption_media,
        };
        let config_value = serde_yaml::to_value(relay_config).unwrap_or_default();
        let mut relay_spec = PluginSpec {
            id: "relay".to_owned(),
            enabled: true,
            dev_only: None,
            triggers: PluginTriggers::default(),
            config: config_value,
        };
        // If the relay plugin provides defaults, merge them first (for future-proofing).
        if let Some(p) = plugins.get("relay") {
            relay_spec.triggers = p.spec().triggers;
            // keep our injected config_value overriding default
        }
        specs.push(relay_spec);
    }
    // Merge defaults from each plugin implementation, without duplicating IDs.
    for p in plugins.values() {
        merge_default_spec(&mut specs, p.spec());
    }

    // Create registry and determine plugin config dir early.
    let registry = Arc::new(PluginRegistry::new());
    let default_dir = if std::path::Path::new("./plugins").exists() {
        "./plugins".to_owned()
    } else {
        "./tools".to_owned()
    };
    let plugins_dir = std::env::var("PLUGINS_DIR")
        .or_else(|_| std::env::var("TOOLS_DIR"))
        .unwrap_or(default_dir);

    // Discover and register WASM plugins (if feature enabled) before applying config.
    register_wasm_dynamic_plugins(&registry).await;

    // Merge per-plugin file config for already-registered entries (e.g., WASM plugins)
    for (id, entry) in registry.entries().await {
        if let Some(file_cfg) = load_plugin_config(&plugins_dir, &id) {
            let mut merged_spec = entry.spec.clone();
            merged_spec.config = merge_yaml(file_cfg, merged_spec.config);
            // re-register with merged spec
            registry.register(merged_spec, entry.plugin).await;
        }
    }

    // Register configured plugins (native or WASM), merging per-plugin file config.
    for mut spec in specs {
        let plugin_arc: Option<Arc<dyn Plugin + Send + Sync>> =
            if let Some(p) = plugins.get(spec.id.as_str()) {
                Some(Arc::clone(p))
            } else if let Some(existing) = registry.entry(&spec.id).await {
                Some(existing.plugin)
            } else {
                None
            };
        let Some(plugin) = plugin_arc else {
            warn!("Unknown plugin ID: {}", spec.id);
            continue;
        };
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

fn merge_default_spec(specs: &mut Vec<PluginSpec>, default: PluginSpec) {
    if let Some(existing) = specs.iter_mut().find(|s| s.id == default.id) {
        // Merge triggers: add any commands/mentions not present
        for cmd in default.triggers.commands {
            if !existing
                .triggers
                .commands
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&cmd))
            {
                existing.triggers.commands.push(cmd);
            }
        }
        for mention in default.triggers.mentions {
            if !existing
                .triggers
                .mentions
                .iter()
                .any(|m| m.eq_ignore_ascii_case(&mention))
            {
                existing.triggers.mentions.push(mention);
            }
        }
        // Do not override existing.config here; file config will merge later.
        // Respect existing.enabled/dev_only as user-provided or file-provided.
    } else {
        specs.push(default);
    }
}

#[cfg(feature = "wasm-plugins")]
async fn register_wasm_dynamic_plugins(registry: &PluginRegistry) {
    let default_dir = if std::path::Path::new("./plugins").exists() {
        "./plugins".to_owned()
    } else {
        "./tools".to_owned()
    };
    let plugins_dir = std::env::var("WASM_PLUGINS_DIR")
        .or_else(|_| std::env::var("PLUGINS_DIR"))
        .unwrap_or(default_dir);

    if let Err(e) = crate::wasm_plugins::register_wasm_plugins_in_dir(registry, &plugins_dir).await
    {
        warn!(error = %e, "Failed to register WASM plugins");
    }
}

#[cfg(not(feature = "wasm-plugins"))]
async fn register_wasm_dynamic_plugins(_registry: &PluginRegistry) {}
