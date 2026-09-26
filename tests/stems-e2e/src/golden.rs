//! Minimal golden-file comparison for tabular human output.
//!
//! Goldens live in `tests/features/goldens/<name>.txt`. The first non-empty
//! line of the output is treated as a header of whitespace-separated column
//! names; columns listed in `ignore` are masked with `*` in every row before
//! comparing (rows are compared token by token, so column widths do not
//! matter). A missing golden is written as `<name>.txt.new` and the step
//! fails so the new file is reviewed; `STEMS_E2E_BLESS=1` writes/overwrites
//! `<name>.txt` directly.

use std::path::Path;

/// Normalises `text`: trims lines, drops empty ones, masks ignored columns.
pub fn normalise(text: &str, ignore: &[String]) -> Vec<Vec<String>> {
    let rows: Vec<Vec<String>> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.split_whitespace().map(str::to_owned).collect())
        .collect();
    let Some(header) = rows.first() else {
        return rows;
    };
    let masked: Vec<usize> = header
        .iter()
        .enumerate()
        .filter(|(_, h)| ignore.iter().any(|i| i.eq_ignore_ascii_case(h)))
        .map(|(i, _)| i)
        .collect();
    rows.iter()
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .enumerate()
                .map(|(i, cell)| {
                    if r > 0 && masked.contains(&i) {
                        "*".to_owned()
                    } else {
                        cell.clone()
                    }
                })
                .collect()
        })
        .collect()
}

/// Compares `actual` to the golden `name` under `dir`.
pub fn check(dir: &Path, name: &str, actual: &str, ignore: &[String]) -> Result<(), String> {
    let path = dir.join(format!("{name}.txt"));
    let bless = std::env::var("STEMS_E2E_BLESS").is_ok_and(|v| v == "1");
    if bless {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, actual).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let Ok(expected) = std::fs::read_to_string(&path) else {
        let new = dir.join(format!("{name}.txt.new"));
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&new, actual).map_err(|e| e.to_string())?;
        return Err(format!(
            "golden {} does not exist; wrote {} for review (rename it, or rerun with STEMS_E2E_BLESS=1)",
            path.display(),
            new.display()
        ));
    };
    if normalise(&expected, ignore) == normalise(actual, ignore) {
        Ok(())
    } else {
        Err(format!(
            "output does not match golden {} (ignoring {ignore:?})\n--- expected\n{expected}\n--- actual\n{actual}",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_ignored_columns_and_whitespace() {
        let a = "NAME   STATUS   PID   UPTIME\napi    healthy  123   3s\n";
        let b = "NAME STATUS PID UPTIME\n  api healthy 999 1m\n\n";
        let ignore = vec!["PID".to_owned(), "uptime".to_owned()];
        assert_eq!(normalise(a, &ignore), normalise(b, &ignore));
        assert_ne!(normalise(a, &[]), normalise(b, &[]));
    }

    #[test]
    fn missing_golden_writes_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = check(dir.path(), "status", "A B\n1 2\n", &[]).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        assert!(dir.path().join("status.txt.new").exists());
        std::fs::write(dir.path().join("status.txt"), "A  B\n1  2\n").unwrap();
        check(dir.path(), "status", "A B\n1 2\n", &[]).unwrap();
        assert!(check(dir.path(), "status", "A B\n1 3\n", &[]).is_err());
    }
}
