use anyhow::Result;

use crate::project::ProjectConfig;

/// Print the project root directory and exit 0, or exit 1 if not in a dip project.
/// Used by shell hooks to locate .dip/commands/ from any subdirectory.
pub fn run() -> Result<()> {
    let project = ProjectConfig::load()?;
    println!("{}", project.root_dir.display());
    Ok(())
}
