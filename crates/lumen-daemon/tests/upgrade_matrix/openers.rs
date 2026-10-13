use std::path::Path;

use lumen_core::{meter, schema};

/// The code paths that open the ledger and bring its schema up to date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opener {
    /// What `lumen-daemon`'s `main` runs before it serves anything.
    Daemon,
    /// The GUI's pool.
    Gui,
    /// Hooks, the MCP server and the CLI.
    Rusqlite,
}

fn url(db: &Path) -> String {
    format!("sqlite:{}?mode=rwc", db.display())
}

impl Opener {
    /// Open the ledger at `db` as this path does, creating or migrating it, then let go of it.
    pub async fn open(self, db: &Path) -> Result<(), String> {
        match self {
            Opener::Daemon => {
                meter::restore_sidecar_write_bits(db);
                let pool = schema::open_pool(&url(db))
                    .await
                    .map_err(|e| e.to_string())?;
                pool.close().await;
                Ok(())
            }
            Opener::Gui => {
                let pool = lumen_stats::connect(&url(db)).await?;
                pool.close().await;
                Ok(())
            }
            Opener::Rusqlite => meter::connect_db(db).map(drop).map_err(|e| e.to_string()),
        }
    }
}
