use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;
use tools::{Tool, ToolEntry, ToolSpec, ToolsRegistry};

pub fn build_registry(
    config_tools: Option<Vec<ToolSpec>>,
    env_ai_handle: Option<String>,
) -> ToolsRegistry {
    let mut by_id: HashMap<String, ToolEntry> = HashMap::new();
    let mut by_command: HashMap<String, String> = HashMap::new();
    let mut by_mention: HashMap<String, String> = HashMap::new();

    // defaults from each tool module
    let mut specs = config_tools.unwrap_or_default();
    let plugins: Vec<fn(&mut Vec<ToolSpec>)> = vec![
        plugin_ping::register_defaults,
        plugin_mode::register_defaults,
        plugin_diagnostics::register_defaults,
        plugin_tools_manager::register_defaults,
        plugin_ai::register_defaults,
        plugin_echo::register_defaults,
    ];
    for register_defaults in plugins {
        register_defaults(&mut specs);
    }
    if let Some(handle) = env_ai_handle {
        append_mention(&mut specs, "ai", &handle);
    }
    // If ai has no mention trigger yet, derive one from name (config or AI_NAME) or default to @Claire
    if !specs
        .iter()
        .any(|t| t.id == "ai" && !t.triggers.mentions.is_empty())
        && let Some(ai_spec) = specs.iter_mut().find(|t| t.id == "ai")
    {
        let name = ai_spec
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| std::env::var("AI_NAME").ok())
            .unwrap_or_else(|| "Claire".to_owned());
        ai_spec.triggers.mentions.push(format!("@{name}"));
    }

    // tool configs directory (can override via TOOLS_DIR). Default to src/tools next to code.
    let tools_dir = std::env::var("TOOLS_DIR").unwrap_or_else(|_| "./src/tools".to_owned());

    for mut spec in specs {
        let id = spec.id.clone();
        let tool: Arc<dyn Tool> = match id.as_str() {
            "mode" => plugin_mode::build(),
            "diag" => plugin_diagnostics::build(),
            "ai" => plugin_ai::build(),
            "tools" => plugin_tools_manager::build(),
            "echo" => plugin_echo::build(),
            "ping" => plugin_ping::build(),
            _ => {
                // unknown tool id
                continue;
            }
        };
        // Load per-tool config from tools_dir/<id>/config.yaml and merge.
        if let Some(file_cfg) = load_tool_config(&tools_dir, &id) {
            spec.config = merge_yaml(file_cfg, spec.config); // file takes precedence
        }
        by_command.extend(
            spec.triggers
                .commands
                .iter()
                .map(|c| (normalize_cmd(c), id.clone())),
        );
        by_mention.extend(
            spec.triggers
                .mentions
                .iter()
                .map(|m| (normalize_mention(m), id.clone())),
        );
        by_id.insert(id, ToolEntry { spec, tool });
    }

    ToolsRegistry {
        by_id: Arc::new(by_id),
        by_command: Arc::new(by_command),
        by_mention: Arc::new(by_mention),
        state: Arc::new(Mutex::new(HashMap::new())),
    }
}

fn normalize_cmd(s: &str) -> String {
    if s.starts_with('!') {
        s.to_owned()
    } else {
        format!("!{s}")
    }
}
fn normalize_mention(s: &str) -> String {
    let raw = if s.starts_with('@') {
        s.to_owned()
    } else {
        format!("@{s}")
    };
    raw.to_lowercase()
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
        // By default, prefer file config value when types differ or non-mapping
        (a, _b) => a,
    }
}

fn append_mention(specs: &mut [ToolSpec], id: &str, mention: &str) {
    if let Some(t) = specs.iter_mut().find(|t| t.id == id) {
        t.triggers.mentions.push(mention.to_owned());
    }
}

fn load_tool_config(root: &str, id: &str) -> Option<serde_yaml::Value> {
    let path = format!("{}/{}/config.yaml", root.trim_end_matches('/'), id);
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_yaml::from_str::<serde_yaml::Value>(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(tool = %id, file = %path, error = %e, "Failed to parse tool config YAML");
                None
            }
        },
        Err(e) => {
            // Only log if file exists but couldn't be read; otherwise silent if not found
            if std::path::Path::new(&path).exists() {
                tracing::warn!(tool = %id, file = %path, error = %e, "Failed to read tool config file");
            }
            None
        }
    }
}
