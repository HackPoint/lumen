//! `lumen import-ledger <dir|lumen.db> [--dry-run]`: a thin shell around
//! `lumen_core::legacy::import`. The refusals and the report texts live there, so the app names
//! the same command in the same words.

use std::path::Path;

use lumen_core::legacy::import::{ImportOptions, import};
use lumen_core::legacy::retire::Native;

/// Run the import against this install's data folder (the folder of its ledger). Returns the
/// process exit code: 0 on success, 1 on any refusal or failure.
pub fn run(path: &Path, dry_run: bool) -> i32 {
    let Some(db) = lumen_core::meter::db_path() else {
        eprintln!("lumen import-ledger: cannot tell where this install's ledger is");
        return 1;
    };
    let Some(n) = db.parent() else {
        eprintln!("lumen import-ledger: {} has no folder", db.display());
        return 1;
    };
    let opts = ImportOptions::new(&lumen_core::faults::utc_now(), dry_run);
    match import(n, path, &opts, &Native) {
        Ok(report) => {
            for line in report.lines() {
                println!("{line}");
            }
            0
        }
        Err(e) => {
            eprintln!("lumen import-ledger: {e}");
            1
        }
    }
}
