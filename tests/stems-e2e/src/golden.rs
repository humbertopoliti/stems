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

/// Normalises `text`: drops empty lines and masks ignored columns.
///
/// The first block of lines (up to the first blank line) is the table. When
/// it is aligned under its header (every row has a space right before each
/// header column's start), rows are cut at the header's column positions, so
/// a cell may contain spaces (`✓ healthy`, a reason); otherwise rows are
/// split on whitespace. Later lines (a summary) are split on whitespace and
/// never masked.
pub fn normalise(text: &str, ignore: &[String]) -> Vec<Vec<String>> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let first = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    let table_end = lines[first..]
        .iter()
        .position(|l| l.trim().is_empty())
        .map_or(lines.len(), |i| first + i);
    let table: Vec<Vec<char>> = lines[first..table_end]
        .iter()
        .map(|l| l.chars().collect())
        .collect();
    let tokens = |l: &str| l.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    let starts: Vec<usize> = table.first().map_or_else(Vec::new, |h| {
        (0..h.len())
            .filter(|&i| h[i] != ' ' && (i == 0 || h[i - 1] == ' '))
            .collect()
    });
    let aligned = !starts.is_empty()
        && table.iter().all(|row| {
            starts
                .iter()
                .skip(1)
                .all(|&s| row.get(s - 1).is_none_or(|c| *c == ' '))
        });
    let mut rows: Vec<Vec<String>> = table
        .iter()
        .map(|row| {
            if !aligned {
                return tokens(&row.iter().collect::<String>());
            }
            starts
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    let e = starts
                        .get(i + 1)
                        .copied()
                        .unwrap_or(row.len())
                        .min(row.len());
                    row.get(s.min(e)..e).map_or_else(String::new, |c| {
                        c.iter().collect::<String>().trim().to_owned()
                    })
                })
                .collect()
        })
        .collect();
    if let Some(header) = rows.first().cloned() {
        let masked: Vec<usize> = header
            .iter()
            .enumerate()
            .filter(|(_, h)| ignore.iter().any(|i| i.eq_ignore_ascii_case(h)))
            .map(|(i, _)| i)
            .collect();
        for row in rows.iter_mut().skip(1) {
            for &i in &masked {
                if let Some(cell) = row.get_mut(i) {
                    "*".clone_into(cell);
                }
            }
        }
    }
    rows.extend(
        lines[table_end..]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| tokens(l)),
    );
    rows
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
    fn aligned_tables_keep_cells_with_spaces() {
        let a = "STEM  STATUS     REASON        PID\napi   ✓ healthy  ready (alive)  123\n\n1 healthy\n";
        let b = "STEM  STATUS     REASON          PID\napi   ✓ healthy  ready (alive)    98765\n\n1 healthy\n";
        let ignore = vec!["PID".to_owned()];
        let n = normalise(a, &ignore);
        assert_eq!(n[1], ["api", "✓ healthy", "ready (alive)", "*"]);
        assert_eq!(n[2], ["1", "healthy"]);
        assert_eq!(n, normalise(b, &ignore));
        let c = "STEM  STATUS     REASON        PID\napi   ✓ healthy  ready (dead)   123\n\n1 healthy\n";
        assert_ne!(normalise(a, &ignore), normalise(c, &ignore));
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
