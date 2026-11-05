//! `stems features [<area>] [--json]` (50): the feature index
//! (`stems_config::features::FEATURES`), as text or the raw table. Needs no
//! workspace and no daemon.

use crate::cli::FeaturesArgs;
use crate::output::CommandOutput;

/// Run `stems features`.
pub fn run(args: &FeaturesArgs) -> CommandOutput {
    let area = args.area.as_deref();
    CommandOutput::data(stems_config::features::to_json(area))
        .with_human(crate::help::features_text(area))
}
