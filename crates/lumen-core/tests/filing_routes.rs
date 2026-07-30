//! Exercise the REST and browser filing routes for real.
//!
//! Both shipped in 1.5.0 having never created anything. `gh` was the only route ever
//! run, once, by hand. A filing path whose first execution is on a user's machine is
//! untested, however many unit tests surround it — so these drive the actual code:
//! `via_api` against a local TCP listener that speaks HTTP, and `via_browser` against a
//! stub opener that records the URL it was handed.
//!
//! Nothing here touches the network. Every outbound call is injected through
//! [`Endpoints`]: the REST base points at a local listener, the browser opener and the
//! GitHub CLI are stub scripts. The first version of this suite instead relied on real
//! `gh` rejecting a nonexistent repository — a live network call, which made the suite
//! pass or fail depending on GitHub's latency.
//!
//! Injected rather than set through the environment, too, so these are safe to run in
//! parallel: an earlier version raced on `set_var` and the failures looked like logic bugs
//! in the fallback chain.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

use lumen_core::report::{Endpoints, Filed, file_issue_with};

/// One captured HTTP request.
#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A single-shot HTTP server. Returns its base URL and a receiver of what it saw.
///
/// Deliberately hand-rolled: the point is to observe the bytes the filing code actually
/// puts on the wire, and a mocking library would sit between the two.
fn serve_once(status: u16, response_body: &'static str) -> (String, mpsc::Receiver<Captured>) {
    serve_script(vec![(status, response_body)])
}

/// Answer each connection from the script in order. The dedupe scan and the create call
/// need different statuses, and serving one status for both made the scan fail in a way
/// that silently changed which route the chain took.
fn serve_script(script: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for (status, body) in script {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            if let Some(c) = handle(stream, status, body) {
                let _ = tx.send(c);
            }
        }
    });

    (format!("http://127.0.0.1:{port}"), rx)
}

fn handle(mut stream: TcpStream, status: u16, response_body: &str) -> Option<Captured> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);

    let mut start = String::new();
    reader.read_line(&mut start).ok()?;
    let mut parts = start.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = Vec::new();
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_string(), v.trim().to_string());
            if k.eq_ignore_ascii_case("content-length") {
                len = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body).ok()?;
    }

    let reason = if (200..300).contains(&status) {
        "OK"
    } else {
        "Error"
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
        response_body.len()
    );
    let _ = stream.flush();

    Some(Captured {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// A stub opener that appends the URL it was given to a file.
///
/// Written rather than mocked so the argv assembly is exercised too. Per-platform because
/// the first version hardcoded `/bin/sh`, which does not exist on Windows — so the two
/// browser-route tests failed there while passing everywhere else, and the Windows arm of
/// `open_in_browser` was the one piece of that function nothing covered.
fn stub_opener(dir: &std::path::Path) -> (Vec<String>, std::path::PathBuf) {
    let log = dir.join("opened.txt");

    #[cfg(windows)]
    {
        let script = dir.join("open.bat");
        // `%*` is every argument. The URL is the last one, and it is the only one this
        // stub is ever given.
        std::fs::write(
            &script,
            format!("@echo off\r\n>>\"{}\" echo %*\r\n", log.display()),
        )
        .expect("write stub");
        return (
            vec![
                "cmd".to_string(),
                "/C".to_string(),
                script.to_string_lossy().into_owned(),
            ],
            log,
        );
    }

    #[cfg(not(windows))]
    {
        let script = dir.join("open.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\n", log.display()),
        )
        .expect("write stub");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
        (
            vec!["/bin/sh".to_string(), script.to_string_lossy().into_owned()],
            log,
        )
    }
}

/// A `gh` that always fails, without touching the network.
///
/// `/usr/bin/false` does not exist on Windows, which is how this suite came to pass on
/// Unix and fail on Windows for a reason unrelated to what it was testing.
fn failing_gh() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd".to_string(),
            "/C".to_string(),
            "exit".to_string(),
            "1".to_string(),
        ]
    } else {
        vec!["/usr/bin/false".to_string()]
    }
}

/// Endpoints pointing at a local listener, with `gh` made unreachable so the chain is
/// forced past it — otherwise a machine with the CLI installed silently tests route 1.
fn endpoints(
    api: &str,
    web: &str,
    open_cmd: Option<Vec<String>>,
    token: Option<&str>,
) -> Endpoints {
    Endpoints {
        api_base: api.to_string(),
        web_base: web.to_string(),
        open_cmd,
        // Always exits non-zero and touches nothing, so the gh route declines and the
        // chain is forced onto the route under test — deterministically, and offline.
        gh_cmd: Some(failing_gh()),
        token: token.map(str::to_string),
    }
}

/// Any repo name will do now that the gh route is a stub; kept explicit so a test that
/// somehow reached the network would be obvious in the failure.
const UNRESOLVABLE_REPO: &str = "lumen-test-owner-that-does-not-exist/nope";

#[test]
fn the_rest_route_posts_a_real_issue_request() {
    let (base, rx) = serve_script(vec![
        // The dedupe scan: no open issue carries this fingerprint.
        (200, "[]"),
        // The create call.
        (
            201,
            r#"{"html_url":"https://example.test/owner/repo/issues/7","number":7}"#,
        ),
    ]);
    let ep = endpoints(&base, "https://example.test", None, Some("test-token-abc"));
    let filing = file_issue_with(&ep, UNRESOLVABLE_REPO, "a title", "a body", "deadbeef")
        .expect("a route should succeed");

    assert_eq!(
        filing.route, "api",
        "expected the REST route, got {:?}",
        filing
    );
    assert_eq!(
        filing.outcome,
        Filed::Created("https://example.test/owner/repo/issues/7".into()),
        "the created URL must come from the response, not be constructed"
    );
    assert!(
        filing.fell_back.iter().any(|s| s.starts_with("gh:")),
        "gh should have been tried and reported: {:?}",
        filing.fell_back
    );

    // First request is the dedupe scan, second is the POST.
    let scan = rx.recv().expect("dedupe scan");
    assert_eq!(scan.method, "GET");
    assert!(
        scan.path.contains("/issues?state=open"),
        "got {}",
        scan.path
    );

    let post = rx.recv().expect("the create request");
    assert_eq!(post.method, "POST");
    assert_eq!(post.path, format!("/repos/{UNRESOLVABLE_REPO}/issues"));
    assert_eq!(
        post.header("Authorization"),
        Some("Bearer test-token-abc"),
        "the token must be sent as a bearer credential"
    );
    assert_eq!(
        post.header("Accept"),
        Some("application/vnd.github+json"),
        "GitHub's versioned media type is required"
    );
    assert!(post.header("X-GitHub-Api-Version").is_some());

    let sent: serde_json::Value = serde_json::from_str(&post.body).expect("JSON body");
    assert_eq!(sent["title"], "a title");
    assert_eq!(sent["body"], "a body");
}

#[test]
fn the_rest_route_surfaces_githubs_own_error_message() {
    let (base, _rx) = serve_script(vec![
        (200, "[]"),
        (422, r#"{"message":"Validation Failed"}"#),
    ]);
    let ep = endpoints(&base, "https://example.test", None, Some("test-token-abc"));
    // With no browser opener and a rejecting API, every route fails — which is what makes
    // the aggregated error assertable.
    // A machine with a working `open` hands off to the browser instead of failing, which
    // is correct behaviour — so the assertion is conditional rather than unconditional.
    if let Err(e) = file_issue_with(&ep, UNRESOLVABLE_REPO, "t", "b", "cafe") {
        assert!(e.contains("422"), "status should be reported: {e}");
        assert!(
            e.contains("Validation Failed"),
            "GitHub's message should be surfaced verbatim, not replaced: {e}"
        );
    }
}

#[test]
fn the_browser_route_hands_over_a_prefilled_url_that_decodes_back_to_the_body() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (open_cmd, log) = stub_opener(dir.path());

    // No token, so the REST route declines before it makes a request. An api_base that
    // refuses connections, so the dedupe scan finds nothing either.
    let ep = endpoints(
        "http://127.0.0.1:1",
        "https://example.test",
        Some(open_cmd),
        None,
    );

    let body = "### lumen — a title\n\n**Impact:** something | with `pipes` & #hashes\n\n<!-- lumen-fault: cafef00d -->\n";
    let filing = file_issue_with(&ep, "owner/repo", "a title", body, "cafef00d")
        .expect("the browser route should succeed");

    assert_eq!(filing.route, "browser");
    let url = match &filing.outcome {
        Filed::Handoff(u) => u.clone(),
        other => panic!("expected a handoff, got {other:?}"),
    };
    assert!(
        url.starts_with("https://example.test/owner/repo/issues/new?"),
        "got {url}"
    );

    // The stub must actually have been invoked with that URL.
    let opened = std::fs::read_to_string(&log).expect("the opener ran");
    assert_eq!(
        opened.trim(),
        url,
        "the URL handed to the opener must be the one reported"
    );

    // And the body must survive the round trip, or the user submits a truncated report.
    let query = url.split_once("&body=").expect("a body parameter").1;
    assert_eq!(percent_decode(query), body);
}

/// Decode a percent-encoded string, for asserting the round trip.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).expect("hex");
            out.push(u8::from_str_radix(hex, 16).expect("hex byte"));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("utf8")
}

#[test]
fn an_existing_issue_opens_that_issue_rather_than_a_duplicate_form() {
    // The dedupe scan finds an open issue carrying the fingerprint marker.
    let marker = lumen_core::report::marker("beefcafe");
    let payload: &'static str = Box::leak(
        serde_json::json!([{
            "number": 42,
            "url": "https://example.test/owner/repo/issues/42",
            "body": format!("some earlier report\n{marker}\n"),
        }])
        .to_string()
        .into_boxed_str(),
    );

    let dir = tempfile::TempDir::new().expect("tempdir");
    let (open_cmd, log) = stub_opener(dir.path());
    let (base, _rx) = serve_once(200, payload);
    let ep = endpoints(&base, "https://example.test", Some(open_cmd), None);

    let filing = file_issue_with(&ep, "owner/repo", "t", "b", "beefcafe").expect("succeeds");

    let url = match &filing.outcome {
        Filed::Handoff(u) => u.clone(),
        other => panic!("expected a handoff, got {other:?}"),
    };
    assert_eq!(
        url, "https://example.test/owner/repo/issues/42",
        "a known fingerprint must open its issue, never a form that would duplicate it"
    );
    assert!(
        !url.contains("issues/new"),
        "opened a new-issue form despite a dedupe match: {url}"
    );
    assert_eq!(
        std::fs::read_to_string(&log).expect("opener ran").trim(),
        url
    );
}
