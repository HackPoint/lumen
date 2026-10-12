use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sqlx::{Connection, SqliteConnection};

#[derive(Clone, Debug)]
pub struct Col {
    pub name: String,
    pub ty: String,
    pub notnull: bool,
    pub dflt: Option<String>,
    pub pk: i64,
}

pub struct TableData {
    pub columns: Vec<Col>,
    pub rows: Vec<BTreeMap<String, String>>,
}

pub struct Snapshot {
    pub facts: BTreeSet<String>,
    pub tables: BTreeMap<String, TableData>,
}

pub fn url(db: &Path, mode: &str) -> String {
    format!("sqlite:{}?mode={mode}", db.display())
}

pub async fn connect(db: &Path) -> SqliteConnection {
    SqliteConnection::connect(&url(db, "rw"))
        .await
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", db.display()))
}

/// Empty when the table does not exist.
pub async fn columns_of(conn: &mut SqliteConnection, table: &str) -> Vec<Col> {
    let rows: Vec<(i64, String, String, i64, Option<String>, i64)> = sqlx::query_as(
        sqlx::AssertSqlSafe(format!("PRAGMA table_info(\"{table}\")")),
    )
    .fetch_all(conn)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(_, name, ty, notnull, dflt, pk)| Col {
            name,
            ty,
            notnull: notnull != 0,
            dflt,
            pk,
        })
        .collect()
}

pub fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn short(rendered: String) -> String {
    if rendered.len() <= 120 {
        return rendered;
    }
    format!(
        "big({} bytes, fnv {:016x})",
        rendered.len(),
        fnv1a(&rendered)
    )
}

fn one_line(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn table_facts(conn: &mut SqliteConnection, table: &str, facts: &mut BTreeSet<String>) {
    for c in columns_of(conn, table).await {
        facts.insert(format!(
            "column {table}.{} {} notnull={} default={} pk={}",
            c.name,
            c.ty,
            u8::from(c.notnull),
            c.dflt.as_deref().unwrap_or("-"),
            c.pk
        ));
    }
    let indexes: Vec<(i64, String, i64, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(
        format!("PRAGMA index_list(\"{table}\")"),
    ))
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    for (_, name, unique, origin, partial) in indexes {
        let cols: Vec<(i64, i64, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "PRAGMA index_info(\"{name}\")"
        )))
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        let cols: Vec<String> = cols
            .into_iter()
            .map(|(_, _, c)| c.unwrap_or_else(|| "<expr>".into()))
            .collect();
        facts.insert(format!(
            "index {name} on {table} ({}) unique={unique} origin={origin} partial={partial}",
            cols.join(",")
        ));
    }
}

async fn table_rows(
    conn: &mut SqliteConnection,
    table: &str,
    columns: &[Col],
) -> Vec<BTreeMap<String, String>> {
    let list: Vec<String> = columns
        .iter()
        .map(|c| format!("quote(\"{}\")", c.name))
        .collect();
    let sql = format!("SELECT {} FROM \"{table}\"", list.join(", "));
    let rows: Vec<sqlx::sqlite::SqliteRow> = sqlx::query(sqlx::AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .unwrap();
    rows.iter()
        .map(|row| {
            use sqlx::Row;
            columns
                .iter()
                .enumerate()
                .map(|(i, c)| (c.name.clone(), short(row.get::<String, _>(i))))
                .collect()
        })
        .collect()
}

/// Schema facts plus every row, each value rendered by SQLite's `quote()` so comparison is exact.
pub async fn take(db: &Path) -> Snapshot {
    let mut conn = connect(db).await;
    let objects: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\' AND type IN ('table','view','trigger') \
         ORDER BY name",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();

    let mut facts = BTreeSet::new();
    let mut tables = BTreeMap::new();
    for (kind, name, sql) in objects {
        match kind.as_str() {
            "table" => {
                facts.insert(format!("table {name}"));
                table_facts(&mut conn, &name, &mut facts).await;
                let columns = columns_of(&mut conn, &name).await;
                let rows = table_rows(&mut conn, &name, &columns).await;
                tables.insert(name, TableData { columns, rows });
            }
            other => {
                facts.insert(format!(
                    "{other} {name}: {}",
                    one_line(&sql.unwrap_or_default())
                ));
            }
        }
    }
    conn.close().await.unwrap();
    Snapshot { facts, tables }
}

pub async fn integrity_check(db: &Path) -> Vec<String> {
    let mut conn = connect(db).await;
    let rows: Vec<(String,)> = sqlx::query_as("PRAGMA integrity_check")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    rows.into_iter().map(|(s,)| s).collect()
}

fn render(row: &BTreeMap<String, String>, only: Option<&BTreeSet<&str>>) -> String {
    row.iter()
        .filter(|(c, _)| only.is_none_or(|keep| keep.contains(c.as_str())))
        .map(|(c, v)| format!("{c}={v}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn sorted(rows: &[BTreeMap<String, String>], only: Option<&BTreeSet<&str>>) -> Vec<String> {
    let mut out: Vec<String> = rows.iter().map(|r| render(r, only)).collect();
    out.sort();
    out
}

fn multiset(rows: Vec<String>) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for r in rows {
        *m.entry(r).or_insert(0) += 1;
    }
    m
}

/// Tables, columns (name, type, notnull, default, pk), indexes and views match; their order does not matter.
pub fn assert_same_schema(what: &str, got: &Snapshot, want: &Snapshot) {
    let missing: Vec<_> = want.facts.difference(&got.facts).collect();
    let extra: Vec<_> = got.facts.difference(&want.facts).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{what}\n  missing from the upgraded ledger: {missing:#?}\n  only in the upgraded ledger: {extra:#?}"
    );
}

/// Old rows survive in the columns both schemas have; a table the current schema dropped may go.
pub fn assert_old_rows_survive(
    what: &str,
    old: &Snapshot,
    new: &Snapshot,
    current: &Snapshot,
    exact: bool,
) {
    for (table, before) in &old.tables {
        if !current.tables.contains_key(table) {
            continue;
        }
        let after = new
            .tables
            .get(table)
            .unwrap_or_else(|| panic!("{what}: table {table} vanished"));
        let kept: BTreeSet<&str> = before
            .columns
            .iter()
            .filter(|c| after.columns.iter().any(|a| a.name == c.name))
            .map(|c| c.name.as_str())
            .collect();
        let was = multiset(sorted(&before.rows, Some(&kept)));
        let now = multiset(sorted(&after.rows, Some(&kept)));
        for (row, n) in &was {
            let have = now.get(row).copied().unwrap_or(0);
            assert!(
                have >= *n,
                "{what}: {table} lost a row ({have} of {n} left): {row}"
            );
        }
        if exact {
            assert_eq!(
                was, now,
                "{what}: {table} rows changed on the way up (columns {kept:?})"
            );
        }
    }
}

/// Added columns hold NULL or their default for old rows and created tables are empty: no guesses.
pub fn assert_nothing_invented(what: &str, old: &Snapshot, new: &Snapshot) {
    for (table, after) in &new.tables {
        let Some(before) = old.tables.get(table) else {
            assert!(
                after.rows.is_empty(),
                "{what}: new table {table} was created holding {} rows",
                after.rows.len()
            );
            continue;
        };
        for col in after
            .columns
            .iter()
            .filter(|c| !before.columns.iter().any(|b| b.name == c.name))
        {
            for row in &after.rows {
                let v = &row[&col.name];
                assert!(
                    v == "NULL" || Some(v) == col.dflt.as_ref(),
                    "{what}: {table}.{} was filled with {v} for an old row (default {:?})",
                    col.name,
                    col.dflt
                );
            }
        }
    }
}

/// Schema and rows, both exactly as they were.
pub fn assert_unchanged(what: &str, before: &Snapshot, after: &Snapshot) {
    assert_same_schema(what, after, before);
    assert_eq!(
        before.tables.keys().collect::<Vec<_>>(),
        after.tables.keys().collect::<Vec<_>>(),
        "{what}: the set of tables changed"
    );
    for (table, b) in &before.tables {
        assert_eq!(
            multiset(sorted(&b.rows, None)),
            multiset(sorted(&after.tables[table].rows, None)),
            "{what}: rows of {table} changed"
        );
    }
}
