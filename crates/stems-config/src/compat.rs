//! Workspace format compatibility (FR-DS-2): which `schema_version` values
//! this build understands, and the first stems release that understood each.
//!
//! When a new schema version ships, append a row to [`SCHEMA_VERSIONS`] with
//! the release that introduces it. An older binary never sees the new row, so
//! for a `schema_version` it does not know it can only say "newer than me";
//! for one it knows (but no longer supports) it names the exact release.

/// `(schema_version, minimum stems version)`, oldest first.
pub const SCHEMA_VERSIONS: &[(u32, &str)] = &[(1, "0.1.0")];

/// `schema_version` values this binary understands.
pub const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[1];

/// The newest `schema_version` this binary understands.
pub const LATEST_SCHEMA_VERSION: u32 = 1;

/// The minimum stems version that supports `schema_version`, when this
/// binary knows it.
pub fn min_stems_version(schema_version: u32) -> Option<&'static str> {
    SCHEMA_VERSIONS
        .iter()
        .find(|(v, _)| *v == schema_version)
        .map(|(_, stems)| *stems)
}

/// The stems version required by `schema_version`, as a human requirement:
/// `">= X"` when the table knows it, else `"> <this version>"` (a future
/// format no release known to this binary supports).
pub fn required_stems(schema_version: u32, this_version: &str) -> String {
    match min_stems_version(schema_version) {
        Some(v) => format!(">= {v}"),
        None => format!("> {this_version}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_and_supported_list_agree() {
        let listed: Vec<u32> = SCHEMA_VERSIONS.iter().map(|(v, _)| *v).collect();
        assert_eq!(listed, SUPPORTED_SCHEMA_VERSIONS);
        assert_eq!(listed.last().copied(), Some(LATEST_SCHEMA_VERSION));
        assert_eq!(crate::defaults::SCHEMA_VERSION, LATEST_SCHEMA_VERSION);
    }

    #[test]
    fn required_version_is_named() {
        assert_eq!(min_stems_version(1), Some("0.1.0"));
        assert_eq!(required_stems(1, "0.3.0"), ">= 0.1.0");
        assert_eq!(min_stems_version(99), None);
        assert_eq!(required_stems(99, "0.1.0"), "> 0.1.0");
    }
}
