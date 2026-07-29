// End-to-end test of the READ path, from a file on disk to the figure on screen.
//
// The existing e2e suite (`lumen-daemon/tests/e2e_pipeline.rs`) covers the other half of
// the product: a transcript line becoming a durable row and a live WebSocket frame. It
// says nothing about what happens when Claude *reads a file* — and that is the half the
// product's headline claim rests on.
//
// This drives the chain a real interception travels:
//
//   a source file on disk
//     -> the REAL lumen-mcp binary over stdio, exactly as Claude Code speaks to it
//       -> a row in SQLite
//         -> lumen_stats::get_optimizer_stats  — the Optimizer screen's numbers
//         -> lumen_stats::get_context_report   — the Hotspots screen's numbers
//
// Nothing is stubbed between those points. A break anywhere along it — a renamed column,
// a migration that does not run, a serde rename, an arithmetic slip in the dollar figure —
// fails here rather than on a user's screen.
//
// Every case is isolated to a tempdir via LUMEN_DB. Nothing here may touch the real
// ledger, and the fixture asserts that rather than trusting it.

use lumen_stats::{connect, get_context_report, get_optimizer_stats};
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

/// A temp ledger plus the source tree the reads will target.
struct Fixture {
    _dir: TempDir,
    db: std::path::PathBuf,
    src: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("ledger.db");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Isolation asserted, not assumed: a regression that ignored LUMEN_DB would otherwise
    // quietly write to the developer's own ledger, and the test would still pass.
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the test ledger must live under the temp dir, got {}",
        db.display()
    );
    Fixture { _dir: dir, db, src }
}

impl Fixture {
    /// Write a Rust file of roughly `defs` definitions and return its path.
    ///
    /// Deliberately large enough to clear `S_min` at the shipped defaults, because a file
    /// below the bar is refused and the test would then be asserting the refusal path.
    fn write_source(&self, name: &str, defs: usize) -> String {
        let mut body = String::new();
        for i in 0..defs {
            body.push_str(&format!(
                "/// Item {i}.\npub fn function_number_{i}(a: u32, b: &str) \
                 -> Result<Vec<String>, Error> {{\n    let mut v = Vec::new();\n    \
                 v.push(format!(\"{{}}\", a));\n    Ok(v)\n}}\n\n"
            ));
        }
        let p = self.src.join(name);
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().to_string()
    }

    /// Call a Lumen MCP tool through the real binary, as Claude Code does.
    ///
    /// Over stdio and in a subprocess rather than by calling the function: the tool's
    /// behaviour depends on the environment (`LUMEN_DB`, the rollout flag), and mutating
    /// the environment in-process is racy across threads and `unsafe` in edition 2024.
    fn call_tool(&self, tool: &str, args: serde_json::Value, env: &[(&str, &str)]) -> String {
        let reqs = [
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                "params":{"protocolVersion":"2024-11-05","capabilities":{},
                          "clientInfo":{"name":"e2e","version":"0"}}}),
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name": tool, "arguments": args}}),
        ];
        let mut input = String::new();
        for r in reqs {
            input.push_str(&serde_json::to_string(&r).unwrap());
            input.push('\n');
        }

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
        cmd.env("LUMEN_DB", &self.db)
            .env_remove("LUMEN_RANKED_OUTLINE")
            // Pinned high so a slow CI runner cannot turn a behavioural assertion into a
            // timing one. The timeout path has its own test in lumen-mcp.
            .env("LUMEN_RANKED_TIME_BUDGET_MS", "60000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn lumen-mcp");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("wait for lumen-mcp");
        assert!(
            out.status.success(),
            "lumen-mcp exited with {:?}",
            out.status.code()
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    async fn pool(&self) -> sqlx::SqlitePool {
        connect(&format!("sqlite:{}?mode=rwc", self.db.display()))
            .await
            .expect("open the ledger the binary just wrote")
    }
}

/// Enough main-agent turns for the cost side of the dollar figure to be computable.
async fn seed_turns(pool: &sqlx::SqlitePool, n: i64, ctx: i64, out: i64) {
    for i in 0..n {
        sqlx::query(
            "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                               cache_read_input_tokens,cache_creation_input_tokens)
             VALUES(?1,'s','2026-07-29T00:00:00Z','claude-opus-5',0,?2,?3,0)",
        )
        .bind(format!("t{i}"))
        .bind(out)
        .bind(ctx)
        .execute(pool)
        .await
        .expect("seed turn");
    }
}

/// The whole chain: a real read produces the Optimizer screen's numbers.
#[tokio::test]
async fn a_real_read_reaches_the_optimizer_screen() {
    let f = fixture();
    let path = f.write_source("big.rs", 400);

    let stdout = f.call_tool("smart_read", serde_json::json!({"path": path}), &[]);
    assert!(
        stdout.contains("outline"),
        "the tool must have answered with an outline: {stdout:.400}"
    );

    let pool = f.pool().await;
    seed_turns(&pool, 200, 100_000, 1_000).await;
    let o = get_optimizer_stats(&pool).await.unwrap();

    // The row exists and is attributed to the tool, not to a bypassed read.
    assert_eq!(o.missed_calls, 0, "a Lumen tool call is not a missed read");
    assert!(
        o.lifetime_full_tokens > 0 && o.lifetime_optimized_tokens > 0,
        "the read produced no savings: full={} saved={}",
        o.lifetime_full_tokens,
        o.lifetime_optimized_tokens
    );
    assert!(
        o.lifetime_optimized_tokens < o.lifetime_full_tokens,
        "an outline must return fewer tokens than the file it summarises"
    );

    // And the dollar figure the screen leads with is computed, not left at zero.
    assert!(o.round_cost_usd > 0.0, "a round must have been priced");
    assert!(o.gross_value_usd > 0.0);
    assert!(
        (o.net_value_usd - (o.gross_value_usd - o.round_cost_usd)).abs() < 1e-9,
        "net must be gross less cost: {} vs {} - {}",
        o.net_value_usd,
        o.gross_value_usd,
        o.round_cost_usd
    );
    assert_eq!(
        o.value_rounds, 194.0,
        "R must reach the screen, not be hidden"
    );
    assert!(
        o.pair_multiplier > 1.0,
        "an intercept costs more than one round"
    );

    // Provenance: an in-process tokenizer has no fallback, so nothing is unverified.
    assert_eq!(
        o.unverified_provenance_rows, 0,
        "a row written by the MCP binary is measured, never estimated"
    );
    assert_eq!(o.provenance_total_rows, 1);
}

/// The same read reaches the Hotspots screen, with the file named and ranked.
#[tokio::test]
async fn a_real_read_reaches_the_hotspots_screen() {
    let f = fixture();
    let big = f.write_source("hot.rs", 400);
    let small = f.write_source("cold.rs", 400);

    // Read one file twice and the other once, so the ranking has something to order.
    f.call_tool("smart_read", serde_json::json!({"path": &big}), &[]);
    f.call_tool(
        "recall_file",
        serde_json::json!({"path": &big, "names": ["function_number_1"]}),
        &[],
    );
    f.call_tool("smart_read", serde_json::json!({"path": &small}), &[]);

    let pool = f.pool().await;
    let r = get_context_report(&pool).await.unwrap();

    assert_eq!(r.distinct_files, 2, "both files must appear");
    assert!(r.total_tokens_read > 0);
    assert_eq!(
        r.top_files[0].name,
        "hot.rs",
        "the file read twice must rank first; got {:?}",
        r.top_files.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert_eq!(r.top_files[0].reads, 2);
    assert!(
        r.top_files[0].share_pct > r.top_files[1].share_pct,
        "shares must be ordered with the ranking"
    );
    assert!(
        (r.top_files.iter().map(|f| f.share_pct).sum::<f64>() - 100.0).abs() < 0.01,
        "with only two files, their shares must account for everything read"
    );

    // The file was not edited between the two reads, so the second learned nothing new.
    assert_eq!(
        r.top_files[0].unchanged_rereads, 1,
        "two reads of an unmodified file leave one that re-acquired known context"
    );
    assert_eq!(r.total_unchanged_rereads, 1);
}

/// A file too small to repay the round it forces must be refused, and the refusal must be
/// visible in the ledger under its own route rather than blended into the savings.
#[tokio::test]
async fn a_file_below_the_bar_is_refused_and_the_refusal_is_recorded() {
    let f = fixture();
    let path = f.write_source("tiny.rs", 2);

    f.call_tool(
        "smart_read",
        serde_json::json!({"path": path}),
        &[("LUMEN_RANKED_OUTLINE", "on")],
    );

    let pool = f.pool().await;
    let route: (String, Option<i64>) =
        sqlx::query_as("SELECT routed_via, budget FROM read_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        route.0, "ranked_not_worth_it",
        "a file this small cannot pay for the round it forced"
    );
    assert!(
        route.1.unwrap() < 0,
        "and its budget must be negative, not merely small"
    );

    // A refusal still answered the caller — the model asked for an outline.
    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.provenance_total_rows, 1, "the refusal is still metered");
}

/// The rollout flag is off by default, and the default arm is the one that always shipped.
#[tokio::test]
async fn the_ranked_arm_is_off_unless_asked_for() {
    let f = fixture();
    let path = f.write_source("big.rs", 400);

    f.call_tool("smart_read", serde_json::json!({"path": &path}), &[]);
    let pool = f.pool().await;
    let (route, budget): (String, Option<i64>) =
        sqlx::query_as("SELECT routed_via, budget FROM read_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(route, "smart_read", "the default arm must be unchanged");
    assert_eq!(
        budget, None,
        "the legacy arm makes no budget decision, so it must claim none"
    );
}

/// With the flag on, the ranking binds and the decision is recorded end to end.
///
/// `k < n` is the assertion that matters: before 1.3.1 the budget was `full - S_min`,
/// every definition fitted, and the ranking selected nothing while appearing to work.
#[tokio::test]
async fn the_ranked_arm_records_a_binding_decision() {
    let f = fixture();
    let path = f.write_source("big.rs", 400);

    f.call_tool(
        "smart_read",
        serde_json::json!({"path": &path}),
        &[("LUMEN_RANKED_OUTLINE", "on")],
    );

    let pool = f.pool().await;
    let (route, k, n, target): (String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT routed_via, k_selected, n_total, target_outline FROM read_events LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(route, "ranked_outline");
    let (k, n) = (k.unwrap(), n.unwrap());
    assert!(n > 0, "definitions were found");
    assert!(
        k < n,
        "the budget did not bind: k={k} of n={n} — the ranking selected nothing"
    );
    assert_eq!(
        target,
        Some(800),
        "the sweep value must be on the row, or arms from different targets pool"
    );
}

/// A read the model never routed through Lumen must count as missed, never as a saving.
#[tokio::test]
async fn a_bypassed_read_is_counted_as_missed_not_saved() {
    let f = fixture();
    let pool = f.pool().await;
    // What the PostToolUse hook writes when Claude used the built-in Read.
    sqlx::query(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,saved_tokens,
                                 routed_via,channel,token_source,writer_hook)
         VALUES('2026-07-29T00:00:00Z','Read','/p/bypassed.rs',400,9000,9000,0,
                'builtin_read','cli','measured','lumen_meter.sh')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.missed_calls, 1);
    assert_eq!(o.missed_full_tokens, 9_000);
    assert_eq!(
        o.lifetime_optimized_tokens, 0,
        "a bypassed read saved nothing and must never appear as a saving"
    );
}

/// A FRESH ledger the binary creates has every current column.
///
/// This covers the `DDL` path only. It is deliberately named for what it tests, because
/// the obvious framing — "the migration must have run" — would be false: a fresh database
/// gets its columns from `CREATE TABLE`, so deleting a migration statement leaves this
/// test green. Verified by doing exactly that.
#[tokio::test]
async fn a_fresh_ledger_the_binary_creates_has_every_current_column() {
    let f = fixture();
    let path = f.write_source("big.rs", 400);
    f.call_tool(
        "smart_read",
        serde_json::json!({"path": path}),
        &[("LUMEN_RANKED_OUTLINE", "on")],
    );

    let pool = f.pool().await;
    assert_current_shape(&pool).await;
}

/// Columns every current writer binds. Shared so the fresh-database and migrated-database
/// tests cannot drift apart.
async fn assert_current_shape(pool: &sqlx::SqlitePool) {
    let cols: Vec<(i64, String)> =
        sqlx::query_as("SELECT cid, name FROM pragma_table_info('read_events')")
            .fetch_all(pool)
            .await
            .unwrap();
    let names: Vec<&str> = cols.iter().map(|(_, n)| n.as_str()).collect();
    for want in [
        "session_id",
        "file_mtime",
        "req_key",
        "writer_hook",
        "token_source",
        "budget",
        "s_min",
        "econ_context",
        "econ_rounds",
        "econ_output",
        "econ_source",
        "k_selected",
        "n_total",
        "coeff_version",
        "target_outline",
    ] {
        assert!(
            names.contains(&want),
            "read_events is missing {want}: {names:?}"
        );
    }
}

/// An EXISTING ledger from an older release must be migrated in place, and the binary
/// must then be able to write to it.
///
/// This is the 1.1.3 regression class, and the one the fresh-database test above cannot
/// reach. `DDL` is `CREATE TABLE IF NOT EXISTS`, so against a database that already has
/// the table it does nothing at all — a column added only to `MIGRATIONS` never arrives,
/// and every insert binding it fails silently because metering is deliberately
/// fire-and-forget. The only way to catch that is to start from a genuinely old schema.
///
/// The table below is hand-written at the pre-1.2.0 shape rather than generated from the
/// current `DDL`, which would already contain the columns and could never fail.
#[tokio::test]
async fn a_legacy_ledger_is_migrated_and_then_written_to() {
    let f = fixture();
    let path = f.write_source("big.rs", 400);

    // A ledger as 1.1.x left it: no decision columns, no provenance columns.
    {
        let conn = rusqlite::Connection::open(&f.db).unwrap();
        conn.execute_batch(
            "CREATE TABLE read_events (
                 ts              TEXT NOT NULL,
                 tool            TEXT NOT NULL,
                 path            TEXT NOT NULL,
                 lines           INTEGER,
                 tokens_returned INTEGER NOT NULL,
                 full_tokens     INTEGER NOT NULL,
                 saved_tokens    INTEGER NOT NULL,
                 routed_via      TEXT NOT NULL
             );",
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('read_events') WHERE name='budget'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "premise: the legacy table has no decision columns");
    }

    f.call_tool(
        "smart_read",
        serde_json::json!({"path": path}),
        &[("LUMEN_RANKED_OUTLINE", "on")],
    );

    let pool = f.pool().await;
    assert_current_shape(&pool).await;

    // Migrating is not enough — the insert has to have succeeded. A silent failure here
    // is exactly what 1.1.3 shipped.
    let (route, target): (String, Option<i64>) =
        sqlx::query_as("SELECT routed_via, target_outline FROM read_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("the binary must have written a row to the migrated ledger");
    assert_eq!(route, "ranked_outline");
    assert_eq!(
        target,
        Some(800),
        "a column that arrived by migration must be writable, not merely present"
    );
}

/// An empty ledger must render as empty, not as a suspiciously round zero-dollar claim.
#[tokio::test]
async fn an_untouched_install_claims_nothing() {
    let f = fixture();
    let pool = f.pool().await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.net_value_usd, 0.0);
    assert_eq!(o.gross_value_usd, 0.0);
    assert_eq!(
        o.round_cost_usd, 0.0,
        "with no turns to average, no round can be priced and none must be claimed"
    );

    let r = get_context_report(&pool).await.unwrap();
    assert_eq!(r.total_tokens_read, 0);
    assert!(r.top_files.is_empty());
}
