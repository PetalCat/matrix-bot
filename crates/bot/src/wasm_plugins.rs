//! WASM plugin loader and adapter.
//!
//! This module provides a minimal, feature-gated skeleton for dynamically loading
//! plugins compiled as WebAssembly components. It is intentionally conservative and
//! focused on structure so we can land it without disrupting the existing native
//! plugin system.
//!
//! Status:
//! - Behind the `wasm-plugins` cargo feature.
//! - Scans a directory for `.wasm` files.
//! - Creates `WasmPlugin` adapter instances with default specs (id derived from filename).
//! - Registers them into the existing `PluginRegistry`.
//! - `run()` attempts to instantiate the component with wasmtime (when the `wasm-plugins` feature is enabled) and will be extended to call into the WIT-defined exports; messages from host-io will be queued and flushed after execution.
//!
//! Next steps (non-breaking, incremental):
//! - Wire in wasmtime component instantiation, link WASI preview2 and `host-io`
//!   from the WIT in `wit/plugin.wit`.
//! - Call `plugin.get-spec` and `plugin.help` at load-time to populate `PluginSpec`
//!   and help text.
//! - Call `plugin.run` on invocation and surface output via `host-io::send-text`.
//!
//! Notes:
//! - This module does not change the default behavior unless the `wasm-plugins`
//!   feature is enabled and the caller explicitly invokes the `register_*` helpers.

use std::{
    ffi::OsStr,
    fmt::Debug,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use tracing::{debug, warn};

use plugin_core::{Plugin, PluginContext, PluginRegistry, PluginSpec, PluginTriggers, send_text};
#[cfg(feature = "wasm-plugins")]
use wasmtime::component::Component;
#[cfg(feature = "wasm-plugins")]
use wasmtime::{Config, Engine};

#[cfg(feature = "wasm-plugins")]
mod wit_bindings {
    // Generated Rust bindings for WIT world "matrix-plugin"
    wasmtime::component::bindgen!({
        path: "../../wit",
        world: "matrix-plugin",
        with: {
            "wasi": wasmtime_wasi::p2::bindings,
        },
        imports: { default: async },
    });
}

#[cfg(feature = "wasm-plugins")]
#[derive(Default, Debug)]
struct HostState {
    queued_text: Vec<String>,
    // TODO: Add WASI preview2 state (resource table + ctx) when wiring execution
}

/// Public entry point: scan `plugins_dir` for WASM components and register them.
///
/// - When the `wasm-plugins` feature is disabled, this is a no-op that returns `Ok(0)`.
/// - When enabled, it discovers files ending in `.wasm` and registers a
///   placeholder `WasmPlugin` for each.
///
/// Returns the count of registered WASM plugins.
pub async fn register_wasm_plugins_in_dir(
    registry: &PluginRegistry,
    plugins_dir: impl AsRef<Path>,
) -> Result<usize> {
    let plugins_dir = plugins_dir.as_ref();
    if !plugins_dir.exists() || !plugins_dir.is_dir() {
        debug!(
            dir = %plugins_dir.display(),
            "WASM plugins directory not found or not a directory; skipping"
        );
        return Ok(0);
    }

    let candidates = discover_wasm_components(plugins_dir)
        .with_context(|| format!("discovering WASM components in {}", plugins_dir.display()))?;

    let mut count = 0usize;
    for wasm_path in candidates {
        match build_wasm_plugin_spec(&wasm_path) {
            Ok(spec) => {
                let plugin: Arc<dyn Plugin + Send + Sync> =
                    Arc::new(WasmPlugin::new(spec.clone(), wasm_path));
                registry.register(spec, plugin).await;
                count += 1;
            }
            Err(err) => {
                warn!(
                    file = %wasm_path.display(),
                    error = %err,
                    "Skipping WASM plugin due to spec build error"
                );
            }
        }
    }

    Ok(count)
}

/// Attempt to discover `.wasm` files directly under the given directory.
///
/// This intentionally avoids recursive traversal for now to keep semantics simple.
/// You can organize per-plugin assets in subfolders and reference them relatively
/// from inside the component if needed.
fn discover_wasm_components(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in
        fs::read_dir(dir).with_context(|| format!("reading plugins directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(OsStr::to_str) {
            Some("wasm") => out.push(path),
            _ => {}
        }
    }
    Ok(out)
}

/// Build a default PluginSpec for a given WASM file path.
///
/// This is a placeholder until the runtime calls `plugin.get-spec` in the component.
/// Today:
/// - id: derived from the file stem
/// - enabled: true
/// - dev_only: None
/// - triggers: empty (user must configure triggers in YAML, e.g. via `plugins/<id>/config.yaml`)
/// - config: empty (merged later from file if present by the existing system)
fn build_wasm_plugin_spec(path: &Path) -> Result<PluginSpec> {
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("cannot derive plugin id from filename {}", path.display()))?;

    let id = stem.to_owned();

    Ok(PluginSpec {
        id,
        enabled: true,
        dev_only: None,
        triggers: PluginTriggers::default(),
        config: serde_yaml::Value::default(),
    })
}

/// Adapter that implements the native `Plugin` trait for a WASM component plugin.
///
/// This is currently a thin stub that holds the desired `PluginSpec` and a path
/// to the component file. The `run()` method is a placeholder that returns an
/// informative message.
#[derive(Debug, Clone)]
pub struct WasmPlugin {
    spec: PluginSpec,
    wasm_path: PathBuf,
    help: Arc<str>,
}

impl WasmPlugin {
    pub fn new(spec: PluginSpec, wasm_path: PathBuf) -> Self {
        Self {
            spec,
            wasm_path,
            help: Arc::from("WASM plugin (wasm-plugins: stub; runtime wiring pending)"),
        }
    }

    /// Future: instantiate the component with wasmtime and WASI preview2,
    /// then call `plugin.get-spec` to refine the spec (triggers/help/dev-only/etc).
    #[allow(dead_code)]
    fn maybe_refresh_spec_from_component(&mut self) -> Result<()> {
        // Intentionally left as a stub in this initial landing.
        // Implementation plan (to be done in a follow-up PR):
        // - Use wasmtime::component::bindgen! against wit/ to get typed host/guest bindings.
        // - Create Engine/Linker/Store, wire WASI and host-io (send_text) shims.
        // - Instantiate `matrix-plugin` world, call `plugin.get-spec` and `plugin.help`.
        // - Merge returned spec defaults with our existing config file overlays (if any).
        let _ = &self.wasm_path;
        Ok(())
    }
}

#[async_trait]
impl Plugin for WasmPlugin {
    fn id(&self) -> &'static str {
        // We need to return a 'static str but we only have a String in the spec.
        // Allocate once per instance and leak to get a 'static lifetime safely for the process.
        // This is acceptable since plugin IDs are few and long-lived.
        Box::leak(self.spec.id.clone().into_boxed_str())
    }

    fn help(&self) -> &'static str {
        Box::leak(self.help.to_string().into_boxed_str())
    }

    fn spec(&self) -> PluginSpec {
        self.spec.clone()
    }

    async fn run(&self, ctx: &PluginContext, args: &str, _spec: &PluginSpec) -> Result<()> {
        #[cfg(feature = "wasm-plugins")]
        {
            // For now, only load the component to validate it can be parsed.
            // Instantiation is deferred until WASI and host-io wiring is complete.
            let mut cfg = Config::new();
            cfg.wasm_component_model(true);
            cfg.async_support(true);
            let engine = Engine::new(&cfg)?;
            let _component = Component::from_file(&engine, &self.wasm_path)
                .with_context(|| format!("loading component {}", self.wasm_path.display()))?;
            let msg = format!(
                "Loaded WASM component for plugin '{}' (instantiation deferred until WASI wiring is ready).\n- Args: {}\n- File: {}",
                self.spec.id,
                args,
                self.wasm_path.display()
            );
            return send_text(ctx, msg).await;
        }
        #[cfg(not(feature = "wasm-plugins"))]
        {
            // Surface a friendly message and echo the argument tail so users know the wiring.
            let msg = format!(
                "WASM plugin '{}' is not yet executable in this build.\n\
                 - Args: {}\n\
                 - File: {}\n\
                 - Action: enable 'wasm-plugins' feature and complete wasmtime/WASI integration.",
                self.spec.id,
                args,
                self.wasm_path.display()
            );
            return send_text(ctx, msg).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn discover_filters_extensions() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Create some files
        let mut f1 = fs::File::create(dir.join("a.wasm")).unwrap();
        writeln!(f1, "not really wasm").unwrap();

        let mut f3 = fs::File::create(dir.join("c.txt")).unwrap();
        writeln!(f3, "ignore me").unwrap();

        let list = discover_wasm_components(dir).unwrap();
        let mut names: Vec<_> = list
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();

        assert_eq!(names, vec!["a.wasm".to_string()]);
    }

    #[test]
    fn spec_derives_id_from_filename() {
        let p = PathBuf::from("/plugins/echo-tools.wasm");
        let spec = build_wasm_plugin_spec(&p).unwrap();
        assert_eq!(spec.id, "echo-tools");
        assert!(spec.enabled);
        assert!(spec.triggers.commands.is_empty());
        assert!(spec.triggers.mentions.is_empty());
    }
}
