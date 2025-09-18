use std::sync::Arc;

use crate::{Tool, ToolSpec};

/// A trait that defines the interface for plugins.
/// Each plugin must implement methods to register its default specifications
/// and to build its tool instance.
pub trait Plugin {
    /// Registers the default specifications for the plugin.
    ///
    /// # Arguments
    ///
    /// * `specs` - A mutable reference to a vector of `ToolSpec` where the plugin's
    ///   default specifications will be added.
    fn register_defaults(&self, specs: &mut Vec<ToolSpec>);

    /// Builds the tool instance for the plugin.
    ///
    /// # Returns
    ///
    /// An `Arc` containing the tool instance.
    #[must_use]
    fn build(&self) -> Arc<dyn Tool>;
}
