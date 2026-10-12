//! Each `schema_*.rs` is `crates/lumen-core/src/schema.rs` at its tag, verbatim to the end of `DDL`.

use std::fmt::Write as _;

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{SqliteConnection, SqlitePool};

use crate::snapshot::{Col, columns_of, fnv1a};

mod v1_1_4 {
    include!("schema_v1_1_4.rs");
}
mod v1_5_0 {
    include!("schema_v1_5_0.rs");
}
mod v1_6_0 {
    include!("schema_v1_6_0.rs");
}
mod v1_7_0 {
    include!("schema_v1_7_0.rs");
}

#[derive(Clone, Copy, Debug)]
pub enum Release {
    V1_1_4,
    V1_5_0,
    V1_6_0,
    V1_7_0,
}

impl Release {
    pub async fn init_schema(self, pool: &SqlitePool) {
        let done = match self {
            Release::V1_1_4 => v1_1_4::init_schema(pool).await,
            Release::V1_5_0 => v1_5_0::init_schema(pool).await,
            Release::V1_6_0 => v1_6_0::init_schema(pool).await,
            Release::V1_7_0 => v1_7_0::init_schema(pool).await,
        };
        done.unwrap_or_else(|e| panic!("{self:?}'s own init_schema failed: {e}"));
    }

    pub fn read_events_columns(self) -> usize {
        match self {
            // 1.1.4 predates the constant; its DDL declares nine columns.
            Release::V1_1_4 => 9,
            Release::V1_5_0 => v1_5_0::READ_EVENTS_COLUMNS,
            Release::V1_6_0 => v1_6_0::READ_EVENTS_COLUMNS,
            Release::V1_7_0 => v1_7_0::READ_EVENTS_COLUMNS,
        }
    }
}

/// A ledger as an install of some age left it.
#[derive(Clone, Copy, Debug)]
pub enum Fixture {
    /// Before the E7 instrumentation columns: no `session_id`, no `faults`.
    V1_1_4,
    /// Still carries `read_events.is_subagent`, which 1.6.0 drops.
    V1_5_0,
    V1_6_0,
    V1_7_0,
    /// Written by 1.5.0, then opened and written to by 1.6.0, then by 1.7.0.
    LongLived,
}

pub const ALL: [Fixture; 5] = [
    Fixture::V1_1_4,
    Fixture::V1_5_0,
    Fixture::V1_6_0,
    Fixture::V1_7_0,
    Fixture::LongLived,
];

/// The tables each release's ledger has, in name order.
const TABLES_1_1_4: &[&str] = &["calibration", "read_events", "sessions", "turns"];
const TABLES_1_5_0: &[&str] = &["calibration", "faults", "read_events", "sessions", "turns"];
const TABLES_1_7_0: &[&str] = &[
    "calibration",
    "faults",
    "read_events",
    "session_events",
    "sessions",
    "turns",
];

impl Fixture {
    /// The releases that opened this ledger, oldest first.
    fn history(self) -> &'static [Release] {
        match self {
            Fixture::V1_1_4 => &[Release::V1_1_4],
            Fixture::V1_5_0 => &[Release::V1_5_0],
            Fixture::V1_6_0 => &[Release::V1_6_0],
            Fixture::V1_7_0 => &[Release::V1_7_0],
            Fixture::LongLived => &[Release::V1_5_0, Release::V1_6_0, Release::V1_7_0],
        }
    }

    /// The release whose schema the finished ledger has.
    pub fn newest(self) -> Release {
        *self.history().last().expect("every fixture has a release")
    }

    pub fn tables(self) -> &'static [&'static str] {
        match self.newest() {
            Release::V1_1_4 => TABLES_1_1_4,
            Release::V1_5_0 | Release::V1_6_0 => TABLES_1_5_0,
            Release::V1_7_0 => TABLES_1_7_0,
        }
    }

    /// The ledger's bytes: one self-contained file, no WAL beside it.
    pub async fn build(self) -> Vec<u8> {
        let dir = tempfile::tempdir().expect("fixture dir");
        let db = dir.path().join("ledger.db");
        for release in self.history() {
            let pool = SqlitePoolOptions::new()
                .connect(&format!("sqlite:{}?mode=rwc", db.display()))
                .await
                .expect("open the fixture ledger");
            release.init_schema(&pool).await;
            seed(&pool).await;
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&pool)
                .await
                .expect("checkpoint");
            pool.close().await;
        }
        let wal = db.with_file_name("ledger.db-wal");
        let left = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert_eq!(
            left, 0,
            "rows still in the WAL would be missing from the copy"
        );
        std::fs::read(&db).expect("read the fixture ledger")
    }
}

#[derive(Debug)]
enum Shape {
    Full,
    /// What the schema requires and nothing else.
    Required,
    /// What the schema requires plus this one optional column.
    Only(String),
    /// Every text column is the empty string.
    Empty,
    /// Spaces, non-ASCII, quotes, a backslash and a newline in every text column.
    Odd,
    /// A 256 KiB text where the table has a bulk column, and integers past f64's exact range.
    Large,
}

enum Val {
    Int(i64),
    Real(f64),
    Text(String),
}

fn required(c: &Col) -> bool {
    c.pk > 0 || (c.notnull && c.dflt.is_none())
}

fn optional(c: &Col) -> bool {
    !c.notnull && c.pk == 0
}

/// The column that carries the large value, per table.
fn bulk(table: &str) -> Option<&'static str> {
    match table {
        "turns" | "session_events" => Some("source_file"),
        "sessions" => Some("cwd"),
        "read_events" => Some("path"),
        "faults" => Some("detail"),
        _ => None,
    }
}

fn big_text() -> String {
    let mut s = String::with_capacity(256 * 1024);
    for i in 0..32_768u32 {
        write!(s, "{i:08x}").expect("write to a string");
    }
    s
}

fn value(table: &str, col: &Col, shape: &Shape, serial: i64) -> Option<Val> {
    let wanted = match shape {
        Shape::Required => required(col),
        Shape::Only(name) => required(col) || *name == col.name,
        _ => true,
    };
    if !wanted {
        return None;
    }
    // Values encode their column and row, so one that lands in another cell is caught.
    let salt = (fnv1a(&format!("{table}.{}", col.name)) % 997) as i64;
    Some(match col.ty.as_str() {
        "INTEGER" => Val::Int(match shape {
            Shape::Required => 0,
            Shape::Empty => -serial,
            Shape::Large => 9_007_199_254_740_993 + serial,
            _ => serial * 1000 + salt,
        }),
        "REAL" => Val::Real(match shape {
            Shape::Large => 1.2345678901234567e17 + serial as f64,
            _ => 0.1 + 0.2 + serial as f64 + salt as f64 / 1000.0,
        }),
        "TEXT" => Val::Text(text(table, col, shape, serial)),
        other => panic!(
            "{table}.{} is {other}: teach the seeder this type",
            col.name
        ),
    })
}

fn text(table: &str, col: &Col, shape: &Shape, serial: i64) -> String {
    let unique = format!("{table}.{}#{serial}", col.name);
    if col.pk > 0 {
        return unique;
    }
    match shape {
        Shape::Empty => String::new(),
        Shape::Odd => format!("/Users/Zoë Ångström/プロジェクト 名/it's \"{unique}\"\\n\n\tü.rs"),
        Shape::Large if bulk(table) == Some(col.name.as_str()) => big_text(),
        _ => unique,
    }
}

async fn insert(
    conn: &mut SqliteConnection,
    table: &str,
    columns: &[Col],
    shape: &Shape,
    serial: i64,
) {
    let chosen: Vec<(&Col, Val)> = columns
        .iter()
        .filter_map(|c| value(table, c, shape, serial).map(|v| (c, v)))
        .collect();
    let names: Vec<String> = chosen
        .iter()
        .map(|(c, _)| format!("\"{}\"", c.name))
        .collect();
    let marks = vec!["?"; chosen.len()].join(", ");
    let sql = format!(
        "INSERT INTO \"{table}\" ({}) VALUES ({marks})",
        names.join(", ")
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for (_, v) in chosen {
        query = match v {
            Val::Int(i) => query.bind(i),
            Val::Real(r) => query.bind(r),
            Val::Text(s) => query.bind(s),
        };
    }
    query
        .execute(conn)
        .await
        .unwrap_or_else(|e| panic!("seeding {table} with a {shape:?} row: {e}"));
}

/// Rows of every shape into every table the schema has, one transaction.
async fn seed(pool: &SqlitePool) {
    let mut tx = pool.begin().await.expect("begin");
    for table in [
        "turns",
        "sessions",
        "calibration",
        "read_events",
        "session_events",
        "faults",
    ] {
        let columns = columns_of(&mut tx, table).await;
        if columns.is_empty() {
            continue;
        }
        let have: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM \"{table}\""
        )))
        .fetch_one(&mut *tx)
        .await
        .expect("count");
        let mut shapes = vec![
            Shape::Full,
            Shape::Full,
            Shape::Required,
            Shape::Empty,
            Shape::Odd,
            Shape::Large,
        ];
        shapes.extend(
            columns
                .iter()
                .filter(|c| optional(c))
                .map(|c| Shape::Only(c.name.clone())),
        );
        for (i, shape) in shapes.iter().enumerate() {
            insert(&mut tx, table, &columns, shape, have + i as i64 + 1).await;
        }
    }
    tx.commit().await.expect("commit");
}
