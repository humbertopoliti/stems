//! `stems --version`: `stems <semver>` as text; with `--json` the envelope
//! with `data: { version, commit, build_date, install_method }`
//! (`install_method`: brew, tarball or cargo; see `commands::upgrade`).

use serde_json::json;

use crate::commands::Ctx;
use crate::output::{CommandOutput, VERSION};

/// Short git commit the binary was built from ("unknown" outside git).
pub const COMMIT: &str = env!("STEMS_GIT_COMMIT");
/// UTC build date (YYYY-MM-DD).
pub const BUILD_DATE: &str = env!("STEMS_BUILD_DATE");

/// Run `--version`.
pub fn run(ctx: &Ctx) -> CommandOutput {
    let raw = if ctx.global.verbose > 0 {
        format!("stems {VERSION} (commit {COMMIT}, built {BUILD_DATE})\n")
    } else {
        format!("stems {VERSION}\n")
    };
    CommandOutput::data(json!({
        "version": VERSION,
        "commit": COMMIT,
        "build_date": BUILD_DATE,
        "install_method": crate::commands::upgrade::detect(ctx).as_str(),
    }))
    .with_raw(raw)
}
