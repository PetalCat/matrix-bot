use std::{collections::HashMap, sync::Arc};

use crate::{BotConfig, RoomCluster};
use plugin_core::{Plugin, PluginRegistry, PluginSpec, PluginTriggers};
use plugin_relay::{Relay, RelayConfig};
use tracing::warn;

pub async fn build_registry(config: &BotConfig) -> Arc<PluginRegistry> {
    // Build a map of plugin id -> instance. Plugins are stateless; one instance is fine.
    #[rustfmt::skip]
    let plugins: HashMap<&'static str, Arc<dyn Plugin + Send + Sync>> = HashMap::from([
        ("phrases", Arc::new(plugin_phrases::Phrases) as Arc<dyn Plugin + Send + Sync>),
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
            relay_spec.triggers = p.spec(serde_yaml::Value::default()).triggers;
            // keep our injected config_value overriding default
        }
        specs.push(relay_spec);
    }
    // Merge defaults from each plugin implementation, without duplicating IDs.
    for p in plugins.values() {
        // Allow plugins to compute their default spec based on a provided config
        // value. We supply an empty/default config here; any file-based plugin
        // config found later via `load_plugin_config` will be merged afterwards.
        merge_default_spec(&mut specs, p.spec(serde_yaml::Value::default()));
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

    for spec in specs {
        let Some(plugin) = plugins.get(spec.id.as_str()) else {
            warn!("Unknown plugin ID: {}", spec.id);
            continue;
        };

        // If a file config exists for this plugin, merge it with the spec.config,
        // then ask the plugin to compute a spec based on that merged config.
        // This allows plugins to derive triggers and other spec fields from
        // their config.
        if let Some(file_cfg) = load_plugin_config(&plugins_dir, spec.id.as_str()) {
            // If a file config exists for this plugin, merge it with the spec.config,
            // then ask the plugin to compute a spec from that merged config.
            let merged_cfg = merge_yaml(file_cfg, spec.config);
            let mut computed_spec = plugin.spec(merged_cfg);

            // Preserve explicit user-provided values from the original spec where appropriate.
            // Keep the user-provided enabled flag and dev_only override if present.
            computed_spec.enabled = spec.enabled;
            if spec.dev_only.is_some() {
                computed_spec.dev_only = spec.dev_only;
            }

            // Ensure the plugin id remains correct and respect any explicit trigger
            // overrides provided in the original spec.
            spec.id.clone_into(&mut computed_spec.id);
            if !spec.triggers.commands.is_empty() || !spec.triggers.mentions.is_empty() {
                computed_spec.triggers = spec.triggers.clone();
            }

            registry.register(computed_spec, Arc::clone(plugin)).await;
        } else {
            // No file config found: ask the plugin to compute a spec from the
            // config already present in the spec (typically defaults).
            let mut computed_spec = plugin.spec(spec.config.clone());
            computed_spec.enabled = spec.enabled;
            if spec.dev_only.is_some() {
                computed_spec.dev_only = spec.dev_only;
            }
            spec.id.clone_into(&mut computed_spec.id);
            if !spec.triggers.commands.is_empty() || !spec.triggers.mentions.is_empty() {
                computed_spec.triggers = spec.triggers.clone();
            }
            registry.register(computed_spec, Arc::clone(plugin)).await;
        }
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
