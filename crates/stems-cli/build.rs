//! Build metadata for `stems --version --json`: the short git commit
//! (`STEMS_GIT_COMMIT`, "unknown" outside a git checkout) and the UTC build
//! date (`STEMS_BUILD_DATE`, honouring `SOURCE_DATE_EPOCH` for reproducible
//! builds). Only reads git (`rev-parse`); never writes.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let git_dir = Path::new(&manifest).join("../../.git");
    let head = git_dir.join("HEAD");
    if head.is_file() {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(text) = std::fs::read_to_string(&head)
            && let Some(r) = text.strip_prefix("ref: ")
        {
            let reference = git_dir.join(r.trim());
            if reference.is_file() {
                println!("cargo:rerun-if-changed={}", reference.display());
            }
        }
        let packed = git_dir.join("packed-refs");
        if packed.is_file() {
            println!("cargo:rerun-if-changed={}", packed.display());
        }
    }

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=STEMS_GIT_COMMIT={commit}");

    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    println!("cargo:rustc-env=STEMS_BUILD_DATE={y:04}-{m:02}-{d:02}");
}

/// Days since 1970-01-01 to (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
