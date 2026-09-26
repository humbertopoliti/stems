//! YAML schema types, merge, substitution and validation for `stems.yaml`.

/// Name of this crate, used to prove the workspace wiring in tests.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_wired() {
        assert_eq!(super::CRATE_NAME, "stems-config");
    }
}
