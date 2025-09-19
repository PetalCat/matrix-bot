use std::sync::Arc;

use crate::{Plugin, PluginSpec};

/// A trait that defines the interface for plugins.
/// Each plugin must implement methods to register its default specifications
/// and to build its tool instance.
pub trait PluginFactory {
    /// Registers the default specifications for the plugin.
    ///
    /// # Arguments
    ///
    /// * `specs` - A mutable reference to a vector of `PluginSpec` where the plugin's
    ///   default specifications will be added.
    fn register_defaults(&self, specs: &mut Vec<PluginSpec>);

    /// Builds the plugin instance.
    ///
    /// # Returns
    ///
    /// An `Arc` containing the plugin instance.
    #[must_use]
    fn build(&self) -> Arc<dyn Plugin + Send + Sync>;
}
