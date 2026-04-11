use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

/// Shared context for commands that operate on a dip project.
///
/// Replaces the four-line boilerplate that was repeated in every command:
/// ```ignore
/// let out     = Output::new(no_color);
/// let project = ProjectConfig::load()?;
/// let rt      = Runtime::new(project, verbose, no_color);
/// ```
///
/// Note: `check_daemon()` is intentionally *not* called here. Every docker
/// command will fail with a clear error if the daemon is not running, so the
/// extra `docker info` round-trip is unnecessary overhead (~200 ms per call).
pub struct Ctx {
    pub out: Output,
    pub rt: Runtime,
}

impl Ctx {
    /// Load project config, build runtime and output in one call.
    pub fn load(verbose: bool, no_color: bool) -> Result<Self> {
        let project = ProjectConfig::load()?;
        Ok(Self {
            out: Output::new(no_color),
            rt: Runtime::new(project, verbose, no_color),
        })
    }
}
