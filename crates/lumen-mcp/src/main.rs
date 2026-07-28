// lumen-mcp — MCP stdio server.
// STDOUT: JSON-RPC 2.0 frames only (newline-delimited).
// STDERR: all logging / diagnostics.
//
// This binary is deliberately thin: it owns the two side effects (writing stdout
// and writing the metering row) and nothing else. All request handling lives in
// the `lumen_mcp` library, where it is unit-testable.

use lumen_mcp::{ErrorResponse, Outcome, Payload, Request, Response, RpcError, dispatch};
use serde_json::Value;
use std::io::{self, BufRead, Write};

fn send(id: Value, result: Value) {
    let out = serde_json::to_string(&Response {
        jsonrpc: "2.0",
        id,
        result,
    })
    .expect("serialization never fails");
    println!("{out}");
    io::stdout().flush().ok();
}

fn send_err(id: Value, code: i32, message: String) {
    let out = serde_json::to_string(&ErrorResponse {
        jsonrpc: "2.0",
        id,
        error: RpcError { code, message },
    })
    .expect("serialization never fails");
    println!("{out}");
    io::stdout().flush().ok();
}

/// Write the reply, then the metering row. Order matters: the response must
/// never be delayed by a DB write.
fn emit(id: Value, outcome: Outcome) {
    match outcome.payload {
        Payload::Ok(result) => send(id, result),
        Payload::Err { code, message } => send_err(id, code, message),
    }
    if let Some(row) = outcome.meter {
        row.record();
    }
}

fn main() {
    eprintln!(
        "lumen-mcp v{} starting (stdio transport)",
        lumen_mcp::SERVER_VERSION
    );

    for line in io::stdin().lock().lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            Ok(_) => continue,
            Err(e) => {
                eprintln!("stdin read error: {e}");
                break;
            }
        };

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("JSON parse error: {e}");
                continue;
            }
        };

        let id = match req.id {
            Some(id) => id,
            None => {
                eprintln!("notification: {}", req.method);
                continue;
            }
        };

        eprintln!("→ {} (id={})", req.method, id);
        emit(id, dispatch(&req.method, req.params));
    }

    eprintln!("lumen-mcp exiting");
}
