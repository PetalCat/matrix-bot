use anyhow::Result;
use async_trait::async_trait;
use rand::seq::SliceRandom as _;
use rand::thread_rng;
use serde::Deserialize;

use plugin_core::{Plugin, PluginContext, PluginSpec, PluginTriggers, send_text};

use std::collections::HashMap;

/// Config shape: a mapping from command name (without leading `!`) to a list of
/// possible reply strings.
///
/// Example YAML:
/// ```yaml
/// ping:
///   - "Pong! 🏓"
///   - "Pong! Another reply"
/// ```
#[derive(Debug, Default, Deserialize)]
struct PhrasesConfig {
    #[serde(flatten)]
    pub phrases: HashMap<String, Vec<String>>,
}

#[derive(Debug)]
pub struct Phrases;

impl Phrases {
    fn load_config(spec: &PluginSpec) -> PhrasesConfig {
        // Convert the spec.config (serde_yaml::Value) into our typed config.
        // If parsing fails, return an empty/default config.
        serde_yaml::from_value::<PhrasesConfig>(spec.config.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Plugin for Phrases {
    fn id(&self) -> &'static str {
        "phrases"
    }

    fn help(&self) -> &'static str {
        "Generic phrase responder: define commands -> replies in plugin config"
    }

    /// Compute a [`PluginSpec`] influenced by the provided configuration value.
    /// Plugins must return a [`PluginSpec`] derived from the provided `config`.
    /// The bot will call this with either an empty/default config when building
    /// defaults, or with a merged file config when loading per-plugin config
    /// files. Implementations should compute any triggers or other derived spec
    /// fields based on the provided `config` and return the complete `PluginSpec`.
    fn spec(&self, config: serde_yaml::Value) -> PluginSpec {
        // Parse config into typed structure (tolerate parse failure).
        let cfg = serde_yaml::from_value::<PhrasesConfig>(config.clone()).unwrap_or_default();

        // Build a set of distinct commands (normalized, with leading '!').
        let mut seen = std::collections::BTreeSet::new();
        let mut commands: Vec<String> = Vec::new();
        for raw_key in cfg.phrases.keys() {
            let key = raw_key.trim_start_matches('!').trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            let token = format!("!{key}");
            if seen.insert(token.clone()) {
                commands.push(token);
            }
        }

        PluginSpec {
            id: "phrases".to_owned(),
            enabled: true,
            dev_only: None,
            triggers: PluginTriggers {
                commands,
                mentions: Vec::new(),
            },
            config,
        }
    }

    /// When invoked via a registered command, pick a response for the specific
    /// command that triggered this run. The triggering token (e.g. `!ping`) may
    /// be supplied via `ctx.trigger` by the dispatcher; otherwise fall back to
    /// the first configured trigger in `spec.triggers.commands`.
    async fn run(&self, ctx: &PluginContext, _args: &str, spec: &PluginSpec) -> Result<()> {
        // Determine which command token triggered this invocation.
        let key = if let Some(trig) = ctx.trigger.as_deref() {
            trig.trim_start_matches('!').to_lowercase()
        } else if let Some(cmd) = spec.triggers.commands.first() {
            cmd.trim_start_matches('!').to_lowercase()
        } else {
            // Nothing to map to.
            return Ok(());
        };

        // Load configured replies from spec.config
        let cfg = Self::load_config(spec);

        // Case-insensitive lookup using normalized key
        if let Some(list) = cfg.phrases.get(&key) {
            if !list.is_empty() {
                // Create RNG and pick choice inside a non-async scope so the RNG
                // does not live across the await point (ThreadRng is not Send).
                let choice = {
                    let mut rng = thread_rng();
                    list.choose(&mut rng).cloned()
                };
                if let Some(choice) = choice {
                    let _ = send_text(ctx, choice).await;
                }
            }
        } else {
            // tolerant fallback: try with leading '!' in config keys
            let ex = format!("!{key}");
            if let Some(list) = cfg.phrases.get(&ex)
                && !list.is_empty()
            {
                let choice = {
                    let mut rng = thread_rng();
                    list.choose(&mut rng).cloned()
                };
                if let Some(choice) = choice {
                    let _ = send_text(ctx, choice).await;
                }
            }
        }

        Ok(())
    }
}
