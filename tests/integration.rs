use files_reader_mcp::{
    config::{Config, RootConfig},
    fs::{Files, SearchOptions},
    git::{DiffScope, store::Store},
    output::Limits,
    server::{self, HttpGuard, ReaderServer},
    text::{self, ReadOptions},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink, path::Path, process::Command};
use tempfile::TempDir;
use tower::ServiceExt;

fn config(path: &Path) -> Config {
    Config {
        listen: "127.0.0.1:3210".parse().unwrap(),
        token_env: None,
        roots: vec![RootConfig {
            name: "projects".into(),
            path: path.to_owned(),
        }],
    }
}
fn files(path: &Path) -> Files {
    Files::new(&config(path)).unwrap()
}
fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "-b", "main"]);
}
fn commit(path: &Path, msg: &str) -> String {
    git(path, &["add", "."]);
    git(path, &["commit", "-m", msg]);
    git(path, &["rev-parse", "HEAD"])
}
fn fixture() -> (TempDir, Files, String, String) {
    let t = TempDir::new().unwrap();
    let p = t.path().join("repo-a");
    repo(&p);
    fs::write(p.join("hello.txt"), "  alpha\r\nbeta\nlast").unwrap();
    let first = commit(&p, "first\n\nbody");
    fs::write(p.join("hello.txt"), "  alpha\r\nBETA\nlast\nnew\n").unwrap();
    let second = commit(&p, "second");
    let f = files(t.path());
    (t, f, first, second)
}
#[test]
fn config_rejects_public_bind_and_duplicate_roots() {
    let t = TempDir::new().unwrap();
    let mut c = config(t.path());
    c.listen = "0.0.0.0:3210".parse().unwrap();
    assert!(c.validate().is_err());
    c.listen = "127.0.0.1:3210".parse().unwrap();
    c.roots.push(RootConfig {
        name: "projects".into(),
        path: t.path().into(),
    });
    assert!(c.validate().is_err());
}
#[test]
fn ordinary_root_contains_multiple_independent_repositories() {
    let (t, f, _, second) = fixture();
    let b = t.path().join("repo-b");
    repo(&b);
    fs::write(b.join("only.txt"), "repository b").unwrap();
    let id = commit(&b, "b");
    fs::write(t.path().join("notes.txt"), "outside repos but inside root").unwrap();
    let root = f.root("projects").unwrap();
    assert_eq!(
        root.text("notes.txt").unwrap().content,
        "outside repos but inside root"
    );
    assert!(Store::open(root, "").is_err());
    let a = Store::open(root, "repo-a").unwrap();
    let b = Store::open(root, "repo-b").unwrap();
    assert_eq!(a.resolve("HEAD").unwrap(), second);
    assert_eq!(b.resolve("HEAD").unwrap(), id);
    assert!(a.read_text("HEAD", "../repo-b/only.txt").is_err());
    assert!(Store::open(root, "../").is_err());
}
#[test]
fn hidden_ignored_files_and_directory_pagination() {
    let t = TempDir::new().unwrap();
    fs::write(t.path().join(".gitignore"), ".secret\n").unwrap();
    fs::write(t.path().join(".secret"), "hello\n").unwrap();
    fs::create_dir(t.path().join("nested")).unwrap();
    fs::write(t.path().join("nested/file.txt"), "nested").unwrap();
    let f = files(t.path());
    let root = f.root("projects").unwrap();
    assert_eq!(root.text(".secret").unwrap().content, "hello\n");
    let inv = root.inventory("", 1).unwrap();
    assert_eq!(inv.entries.len(), 3);
    assert_eq!(inv.gaps[0].reason, "depth_limit");
    assert_eq!(root.inventory("", 2).unwrap().entries.len(), 4);
}
#[test]
fn traversal_symlinks_devices_and_pipes_are_rejected() {
    let t = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret"), "DO NOT READ").unwrap();
    symlink(outside.path(), t.path().join("escape")).unwrap();
    symlink("/dev/zero", t.path().join("device")).unwrap();
    Command::new("mkfifo")
        .arg(t.path().join("fifo"))
        .status()
        .unwrap();
    let f = files(t.path());
    let r = f.root("projects").unwrap();
    for path in [
        "../secret",
        "/etc/passwd",
        "escape/secret",
        "device",
        "fifo",
        ".git/config",
        "foo\\bar",
    ] {
        assert!(r.text(path).is_err(), "{path}");
    }
    assert_eq!(r.inventory("", 2).unwrap().gaps.len(), 3);
}
#[test]
fn file_descriptor_boundary_survives_directory_swap() {
    let t = TempDir::new().unwrap();
    let authorized = t.path().join("authorized");
    fs::create_dir(&authorized).unwrap();
    fs::write(authorized.join("a"), "inside").unwrap();
    let f = files(&authorized);
    fs::rename(&authorized, t.path().join("moved")).unwrap();
    symlink("/etc", &authorized).unwrap();
    assert_eq!(
        f.root("projects").unwrap().text("a").unwrap().content,
        "inside"
    );
    assert!(f.root("projects").unwrap().text("passwd").is_err());
}
#[test]
fn text_encoding_media_and_binary_policy() {
    assert_eq!(
        text::decode("a", b"\xef\xbb\xbfhi\r\n").unwrap().content,
        "hi\r\n"
    );
    assert_eq!(
        text::decode("a", &[255, 254, 65, 0, 10, 0])
            .unwrap()
            .content,
        "A\n"
    );
    assert_eq!(text::decode("a", &[254, 255, 0, 65]).unwrap().content, "A");
    for (p, b) in [
        ("a", b"a\0b".as_slice()),
        ("a", b"\xffx"),
        ("a", b"%PDF-1.7"),
        ("a", b"<?xml version='1.0'?><!-- hi --><svg/>"),
        ("a.svg", b"text"),
        ("a.docx", b"text"),
        ("a", b"\x89PNG\r\n\x1a\n"),
        ("a", b"P3\n2 2\n255\n"),
    ] {
        assert!(text::decode(p, b).is_err(), "{p} {b:?}");
    }
    assert!(text::decode("a", &[255, 254, 0, 216]).is_err());
}
#[test]
fn numbered_read_reassembles_long_unicode_crlf_and_validates_cursor() {
    let content = format!("  first\r\n{}\nlast", "好\\\"".repeat(2000));
    let t = text::decode("a", content.as_bytes()).unwrap();
    let limits = Limits {
        max_lines: Some(2),
        max_bytes: Some(2048),
    };
    let mut cursor = None;
    let mut reconstructed = String::new();
    let mut loops = 0;
    loop {
        let result = text::read(
            "a",
            &t,
            ReadOptions {
                start: None,
                end: None,
                tail: None,
                limits: &limits,
                cursor: &cursor,
            },
        )
        .unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() <= 2048);
        for row in result["lines"].as_array().unwrap() {
            reconstructed.push_str(row["text"].as_str().unwrap());
        }
        cursor = result["next_cursor"].as_str().map(String::from);
        loops += 1;
        assert!(loops < 200);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(content, reconstructed);
    let result = text::read(
        "a",
        &t,
        ReadOptions {
            start: None,
            end: None,
            tail: Some(1),
            limits: &Limits::default(),
            cursor: &None,
        },
    )
    .unwrap();
    assert_eq!(result["lines"][0]["line"], 3);
    assert_eq!(result["lines"][0]["text"], "last");
}
#[test]
fn search_reports_binary_gaps_and_continues() {
    let paths = vec!["a".into(), "binary".into(), "b".into()];
    let limit = Limits {
        max_lines: Some(1),
        max_bytes: None,
    };
    let mut cursor = None;
    let mut hits = 0;
    let mut skips = 0;
    loop {
        let result = files_reader_mcp::fs::search(
            &paths,
            "stable",
            SearchOptions {
                pattern: "find",
                regex: false,
                context: 0,
                limits: &limit,
                cursor: &cursor,
            },
            |p| {
                text::decode(
                    p,
                    if p == "binary" {
                        b"\0"
                    } else {
                        b"find me\nfind again\n"
                    },
                )
            },
        )
        .unwrap();
        for item in result["items"].as_array().unwrap() {
            if item["kind"] == "match" {
                hits += 1;
            } else if item["kind"] == "not_searched" {
                skips += 1;
            }
        }
        cursor = result["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(hits, 4);
    assert_eq!(skips, 1);
}
#[test]
fn git_history_refs_diff_blame_and_merge_base() {
    let (t, f, first, second) = fixture();
    let p = t.path().join("repo-a");
    git(&p, &["tag", "-a", "v1", "-m", "release", &first]);
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    assert_eq!(s.resolve("HEAD~1").unwrap(), first);
    assert_eq!(s.peel(&s.resolve("v1").unwrap()).unwrap(), first);
    assert_eq!(s.resolve(&second[..8]).unwrap(), second);
    assert_eq!(s.log("HEAD", 20).unwrap().len(), 2);
    assert_eq!(s.merge_bases("v1", "HEAD").unwrap(), vec![first.clone()]);
    let diff = s
        .diff(DiffScope::Revisions, Some(&first), Some(&second), &None)
        .unwrap();
    assert!(
        diff.iter()
            .any(|r| r["sign"] == "+" && r["text"] == "BETA\n")
    );
    let blame = s.blame("HEAD", "hello.txt").unwrap();
    assert_eq!(blame[0]["commit"], first);
    assert_eq!(blame[1]["commit"], second);
    assert_eq!(blame[0]["line"], 1);
    assert!(
        !s.history_search("HEAD", "BETA", false, &None)
            .unwrap()
            .is_empty()
    );
    assert!(s.resolve("HEAD:hello.txt").is_err());
}
#[test]
fn git_status_staged_worktree_deleted_and_untracked() {
    let (t, f, _, _) = fixture();
    let p = t.path().join("repo-a");
    fs::write(p.join("hello.txt"), "staged\n").unwrap();
    git(&p, &["add", "hello.txt"]);
    fs::write(p.join("hello.txt"), "working\n").unwrap();
    fs::write(p.join("new.txt"), "new").unwrap();
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    let status = s.status().unwrap();
    assert!(
        status["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "hello.txt"
                && v["staged"] == "modified"
                && v["worktree"] == "modified")
    );
    assert!(
        s.diff(DiffScope::Staged, None, None, &None)
            .unwrap()
            .iter()
            .any(|v| v["text"] == "staged\n")
    );
    assert!(
        s.diff(DiffScope::Worktree, None, None, &None)
            .unwrap()
            .iter()
            .any(|v| v["text"] == "working\n")
    );
    fs::remove_file(p.join("hello.txt")).unwrap();
    assert!(
        s.status().unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["worktree"] == "deleted")
    );
}
#[test]
fn git_binary_media_and_historical_symlink_do_not_leak() {
    let (t, f, _, before) = fixture();
    let p = t.path().join("repo-a");
    fs::write(p.join("binary"), b"SECRET_BINARY\0PAYLOAD").unwrap();
    fs::write(p.join("drawing"), b"<svg>SECRET_SVG</svg>").unwrap();
    symlink("/etc/passwd", p.join("link")).unwrap();
    let after = commit(&p, "assets");
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    for path in ["binary", "drawing", "link"] {
        assert!(s.read_text("HEAD", path).is_err());
    }
    let diff = s
        .diff(DiffScope::Revisions, Some(&before), Some(&after), &None)
        .unwrap();
    let wire = serde_json::to_string(&diff).unwrap();
    assert!(
        !wire.contains("SECRET_BINARY")
            && !wire.contains("SECRET_SVG")
            && !wire.contains("/etc/passwd")
    );
    assert_eq!(
        diff.iter().filter(|r| r["kind"] == "patch_omitted").count(),
        3
    );
}
#[test]
fn packed_objects_deltas_and_packed_refs() {
    let (t, f, _, _) = fixture();
    let p = t.path().join("repo-a");
    let mut ids = Vec::new();
    for n in 0..12 {
        let content = format!(
            "{}\nversion {n}\n{}",
            "same prefix line\n".repeat(1000),
            "same suffix\n".repeat(1000)
        );
        fs::write(p.join("long.txt"), content).unwrap();
        ids.push(commit(&p, &format!("version {n}")));
    }
    git(&p, &["tag", "packed-tag"]);
    git(&p, &["gc", "--aggressive", "--prune=now"]);
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    assert_eq!(s.resolve("packed-tag").unwrap(), *ids.last().unwrap());
    for (n, id) in ids.iter().enumerate() {
        assert!(
            s.read_text(id, "long.txt")
                .unwrap()
                .content
                .contains(&format!("version {n}"))
        );
        assert_eq!(s.resolve(&id[..8]).unwrap(), *id);
    }
    assert_eq!(s.log("HEAD", 100).unwrap().len(), 14);
}
#[test]
fn index_v4_and_missing_objects() {
    let (t, f, _, _) = fixture();
    let p = t.path().join("repo-a");
    git(&p, &["update-index", "--index-version", "4"]);
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    assert!(s.index().unwrap().contains_key("hello.txt"));
    assert!(
        s.object("1111111111111111111111111111111111111111")
            .unwrap_err()
            .to_string()
            .contains("MISSING_OBJECT")
    );
}
#[test]
fn worktrees_and_alternates_are_explicitly_rejected() {
    let (t, f, _, _) = fixture();
    let p = t.path().join("repo-a");
    let linked = t.path().join("linked");
    git(
        &p,
        &["worktree", "add", "-b", "other", linked.to_str().unwrap()],
    );
    let r = f.root("projects").unwrap();
    assert!(
        Store::open(r, "linked")
            .err()
            .unwrap()
            .to_string()
            .contains("WORKTREE")
    );
    assert!(
        Store::open(r, "repo-a")
            .err()
            .unwrap()
            .to_string()
            .contains("WORKTREE")
    );
    assert!(r.text("linked/hello.txt").is_err());
    git(&p, &["worktree", "remove", linked.to_str().unwrap()]);
    fs::write(p.join(".git/objects/info/alternates"), "/outside/objects").unwrap();
    assert!(
        Store::open(r, "repo-a")
            .err()
            .unwrap()
            .to_string()
            .contains("UNSUPPORTED")
    );
}
#[test]
fn malicious_config_attributes_hooks_never_execute_and_repository_unchanged() {
    let (t, f, _, _) = fixture();
    let p = t.path().join("repo-a");
    let marker = t.path().join("EXECUTED");
    git(
        &p,
        &[
            "config",
            "diff.external",
            &format!("touch {}", marker.display()),
        ],
    );
    git(
        &p,
        &[
            "config",
            "diff.evil.textconv",
            &format!("touch {}", marker.display()),
        ],
    );
    git(
        &p,
        &[
            "config",
            "core.fsmonitor",
            &format!("touch {}", marker.display()),
        ],
    );
    git(&p, &["config", "include.path", "/not/allowed/config"]);
    fs::write(p.join(".gitattributes"), "* diff=evil filter=evil\n").unwrap();
    let index_before = fs::read(p.join(".git/index")).unwrap();
    let config_before = fs::read(p.join(".git/config")).unwrap();
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    s.status().unwrap();
    s.diff(DiffScope::Worktree, None, None, &None).unwrap();
    s.blame("HEAD", "hello.txt").unwrap();
    assert!(!marker.exists());
    assert_eq!(fs::read(p.join(".git/index")).unwrap(), index_before);
    assert_eq!(fs::read(p.join(".git/config")).unwrap(), config_before);
    assert!(!p.join(".git/index.lock").exists());
}

fn app(path: &Path, token: Option<String>) -> axum::Router {
    server::router(
        ReaderServer::new(files(path)),
        HttpGuard {
            authority: "127.0.0.1:3210".into(),
            token,
        },
        tokio_util::sync::CancellationToken::new(),
    )
}
async fn rpc(app: axum::Router, body: Value) -> (u16, Value) {
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "127.0.0.1:3210")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let b = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&b).unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&b)})),
    )
}

async fn output_schemas(
    app: axum::Router,
) -> std::collections::BTreeMap<String, jsonschema::Validator> {
    let (status, listing) = rpc(
        app,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(status, 200);
    let tools = listing["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 18);
    tools
        .iter()
        .map(|tool| {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["outputSchema"];
            assert_eq!(schema["type"], "object", "{name}");
            assert!(
                !schema["properties"].as_object().unwrap().is_empty(),
                "{name}"
            );
            let validator =
                jsonschema::validator_for(schema).unwrap_or_else(|e| panic!("{name}: {e}"));
            // A generic unconstrained object would not help clients understand results.
            assert!(!validator.is_valid(&json!({})), "{name}");
            (name.to_owned(), validator)
        })
        .collect()
}

fn validate_output(
    schemas: &std::collections::BTreeMap<String, jsonschema::Validator>,
    name: &str,
    result: &Value,
) -> Value {
    assert_eq!(result["isError"], false, "{name}: {result}");
    let data = &result["structuredContent"];
    assert!(data.is_object(), "{name}: missing structuredContent");
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        *data, text,
        "{name}: structured and legacy results must agree"
    );
    let errors: Vec<_> = schemas[name]
        .iter_errors(data)
        .map(|e| format!("{}: {e}", e.instance_path))
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:?}\n{data}");
    data.clone()
}

#[tokio::test]
async fn streamable_http_initialization_listing_and_call() {
    let t = TempDir::new().unwrap();
    fs::write(t.path().join("hello.txt"), "hello\n").unwrap();
    let a = app(t.path(), None);
    let(status,value)=rpc(a.clone(),json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["result"]["serverInfo"]["name"], "files-reader-mcp");
    let (status, value) = rpc(
        a.clone(),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(status, 200, "{value}");
    let tools = value["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 18);
    assert!(
        tools
            .iter()
            .all(|v| v["annotations"]["readOnlyHint"] == true)
    );
    let(status,value)=rpc(a,json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"root":"projects","path":"hello.txt"}}})).await;
    assert_eq!(status, 200, "{value}");
    assert_ne!(value["result"]["isError"], true, "{value}");
    let payload: Value =
        serde_json::from_str(value["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload["lines"][0]["text"], "hello\n");
}
#[tokio::test]
async fn http_rejects_bad_origin_host_token_and_oversize() {
    let t = TempDir::new().unwrap();
    for (host, origin, token, expected) in [
        ("evil.example", None, None, 403),
        ("127.0.0.1:3210", Some("https://evil.example"), None, 403),
        ("127.0.0.1:3210", None, Some("secret"), 401),
    ] {
        let a = app(t.path(), token.map(String::from));
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", host);
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        let result = a
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(result.status().as_u16(), expected);
    }
    let a = app(t.path(), None);
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "127.0.0.1:3210")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(axum::body::Body::from(" ".repeat(65537)))
        .unwrap();
    assert_eq!(a.oneshot(req).await.unwrap().status().as_u16(), 413);
}

#[test]
fn multiple_roots_remain_individually_scoped() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    fs::write(a.path().join("a.txt"), "a").unwrap();
    fs::write(b.path().join("b.txt"), "b").unwrap();
    let mut c = config(a.path());
    c.roots.push(RootConfig {
        name: "other".into(),
        path: b.path().into(),
    });
    let f = Files::new(&c).unwrap();
    assert_eq!(f.root("other").unwrap().text("b.txt").unwrap().content, "b");
    assert!(f.root("projects").unwrap().text("b.txt").is_err());
    assert!(f.root("missing").is_err());
}
#[test]
fn svg_document_detection_does_not_block_source_strings() {
    assert!(text::decode("component.rs", b"let icon = \"<svg/>\";\n").is_ok());
    assert!(
        text::decode(
            "asset.txt",
            b"<?xml version='1.0'?><!DOCTYPE svg [ <!ELEMENT svg ANY> ]><svg/>"
        )
        .is_err()
    );
    assert!(
        text::decode(
            "asset",
            b"<!--note--><x:svg xmlns:x='http://www.w3.org/2000/svg'/>"
        )
        .is_err()
    );
}
#[test]
fn stale_read_cursor_fails_instead_of_skipping_new_content() {
    let limit = Limits {
        max_lines: Some(1),
        max_bytes: None,
    };
    let old = text::decode("a", b"a\nb\n").unwrap();
    let value = text::read(
        "a",
        &old,
        ReadOptions {
            start: None,
            end: None,
            tail: None,
            limits: &limit,
            cursor: &None,
        },
    )
    .unwrap();
    let cursor = value["next_cursor"].as_str().map(String::from);
    let new = text::decode("a", b"new\na\nb\n").unwrap();
    let e = text::read(
        "a",
        &new,
        ReadOptions {
            start: None,
            end: None,
            tail: None,
            limits: &limit,
            cursor: &cursor,
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("STALE_CURSOR"));
}
#[test]
fn huge_patch_line_can_be_fully_reassembled_under_byte_limit() {
    let original = "中国\"\\".repeat(5000);
    let items = vec![json!({"kind":"patch_line","path":"a","new_line":10,"text":original})];
    let limits = Limits {
        max_lines: Some(1),
        max_bytes: Some(2048),
    };
    let mut cursor = None;
    let mut restored = String::new();
    let mut calls = 0;
    loop {
        let p = files_reader_mcp::output::page(items.clone(), &limits, &cursor, json!({})).unwrap();
        assert!(serde_json::to_vec(&p).unwrap().len() <= 2048);
        restored.push_str(p["items"][0]["text"].as_str().unwrap());
        cursor = p["next_cursor"].as_str().map(String::from);
        calls += 1;
        assert!(calls < 500);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(restored, original);
}
#[test]
fn missing_blob_fails_diff_and_search_without_silent_omission() {
    let (t, f, first, second) = fixture();
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    let tree = s.tree("HEAD").unwrap();
    let id = &tree["hello.txt"].oid;
    fs::remove_file(
        t.path()
            .join(format!("repo-a/.git/objects/{}/{}", &id[..2], &id[2..])),
    )
    .unwrap();
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    assert!(
        s.diff(DiffScope::Revisions, Some(&first), Some(&second), &None)
            .unwrap_err()
            .to_string()
            .contains("MISSING_OBJECT")
    );
    assert!(
        files_reader_mcp::fs::search(
            &["hello.txt".into()],
            "stable",
            SearchOptions {
                pattern: "x",
                regex: false,
                context: 0,
                limits: &Limits::default(),
                cursor: &None
            },
            |p| s.read_text("HEAD", p)
        )
        .unwrap_err()
        .to_string()
        .contains("MISSING_OBJECT")
    );
}
#[test]
fn git_metadata_symlink_is_never_followed() {
    let (t, f, _, _) = fixture();
    let p = t.path().join("repo-a/.git");
    fs::rename(p.join("objects"), t.path().join("object-copy")).unwrap();
    symlink(t.path().join("object-copy"), p.join("objects")).unwrap();
    assert!(Store::open(f.root("projects").unwrap(), "repo-a").is_err());
}
#[test]
fn mode_only_worktree_change_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let (t, f, _, _) = fixture();
    fs::set_permissions(
        t.path().join("repo-a/hello.txt"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let s = Store::open(f.root("projects").unwrap(), "repo-a").unwrap();
    let changes = s.diff(DiffScope::Worktree, None, None, &None).unwrap();
    assert!(
        changes
            .iter()
            .any(|v| v["old_mode"] == "100644" && v["new_mode"] == "100755")
    );
}
#[tokio::test]
async fn every_tool_returns_structured_content_matching_its_output_schema() {
    let (t, _, _, _) = fixture();
    let a = app(t.path(), None);
    let schemas = output_schemas(a.clone()).await;
    let calls = [
        ("roots", json!({})),
        (
            "file_info",
            json!({"root":"projects","path":"repo-a/hello.txt"}),
        ),
        ("list_directory", json!({"root":"projects"})),
        (
            "find_files",
            json!({"root":"projects","path_glob":"**/*.txt"}),
        ),
        ("search_text", json!({"root":"projects","pattern":"BETA"})),
        (
            "read_file",
            json!({"root":"projects","path":"repo-a/hello.txt"}),
        ),
        ("git_status", json!({"root":"projects","repo":"repo-a"})),
        ("git_refs", json!({"root":"projects","repo":"repo-a"})),
        ("git_resolve", json!({"root":"projects","repo":"repo-a"})),
        ("git_tree", json!({"root":"projects","repo":"repo-a"})),
        ("git_log", json!({"root":"projects","repo":"repo-a"})),
        ("git_show", json!({"root":"projects","repo":"repo-a"})),
        (
            "git_read_file",
            json!({"root":"projects","repo":"repo-a","path":"hello.txt"}),
        ),
        (
            "git_blame",
            json!({"root":"projects","repo":"repo-a","path":"hello.txt"}),
        ),
        (
            "git_search",
            json!({"root":"projects","repo":"repo-a","pattern":"BETA"}),
        ),
        (
            "git_search_history",
            json!({"root":"projects","repo":"repo-a","pattern":"BETA"}),
        ),
        (
            "git_merge_base",
            json!({"root":"projects","repo":"repo-a","a":"HEAD","b":"HEAD~1"}),
        ),
        (
            "git_diff",
            json!({"root":"projects","repo":"repo-a","scope":"revisions","from":"HEAD~1","to":"HEAD"}),
        ),
    ];
    for (name, args) in calls {
        let(status,v)=rpc(a.clone(),json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).await;
        assert_eq!(status, 200, "{name}: {v}");
        assert!(v.get("error").is_none(), "{name}: {v}");
        assert_ne!(v["result"]["isError"], true, "{name}: {v}");
        validate_output(&schemas, name, &v["result"]);
    }
    let(_,v)=rpc(a,json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"git_status","arguments":{"root":"projects","repo":"repo-a","command":"push"}}})).await;
    assert!(
        v.get("error").is_some() || v["result"]["isError"] == true,
        "{v}"
    );
}

#[tokio::test]
async fn output_schemas_cover_fragments_gaps_root_commits_and_errors() {
    let t = TempDir::new().unwrap();
    let p = t.path().join("repo");
    repo(&p);
    let long = format!("needle {}\r\n", "好".repeat(1000));
    fs::write(p.join("long.txt"), &long).unwrap();
    fs::write(p.join("binary.bin"), b"\0old").unwrap();
    fs::write(p.join("deleted.txt"), "needle deleted\n").unwrap();
    commit(&p, &format!("initial\n\n{long}"));
    fs::write(p.join("long.txt"), format!("needle changed\n{long}")).unwrap();
    fs::write(p.join("binary.bin"), b"\0new").unwrap();
    commit(&p, "second");
    fs::write(p.join("long.txt"), "needle staged\n").unwrap();
    git(&p, &["add", "long.txt"]);
    fs::write(p.join("long.txt"), &long).unwrap();
    fs::remove_file(p.join("deleted.txt")).unwrap();
    fs::write(p.join("untracked.txt"), "needle untracked\n").unwrap();
    symlink("/outside", p.join("unreadable")).unwrap();
    fs::create_dir(p.join("nested")).unwrap();
    fs::write(p.join("nested/hidden.txt"), "needle\n").unwrap();
    let a = app(t.path(), None);
    let schemas = output_schemas(a.clone()).await;
    let calls = [
        (
            "read_file",
            json!({"root":"projects","path":"repo/long.txt"}),
        ),
        (
            "list_directory",
            json!({"root":"projects","path":"repo","depth":1}),
        ),
        (
            "find_files",
            json!({"root":"projects","path":"repo","depth":1}),
        ),
        (
            "search_text",
            json!({"root":"projects","path":"repo","pattern":"needle"}),
        ),
        (
            "git_search",
            json!({"root":"projects","repo":"repo","pattern":"needle"}),
        ),
        (
            "git_read_file",
            json!({"root":"projects","repo":"repo","path":"long.txt"}),
        ),
        ("git_status", json!({"root":"projects","repo":"repo"})),
        (
            "git_show",
            json!({"root":"projects","repo":"repo","revision":"HEAD~1"}),
        ),
        ("git_show", json!({"root":"projects","repo":"repo"})),
        ("git_log", json!({"root":"projects","repo":"repo"})),
        (
            "git_blame",
            json!({"root":"projects","repo":"repo","path":"long.txt"}),
        ),
        (
            "git_search_history",
            json!({"root":"projects","repo":"repo","pattern":"needle"}),
        ),
        (
            "git_diff",
            json!({"root":"projects","repo":"repo","scope":"staged"}),
        ),
        (
            "git_diff",
            json!({"root":"projects","repo":"repo","scope":"worktree"}),
        ),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for (name, mut args) in calls {
        args["limits"] = json!({"max_bytes":2048,"max_lines":2});
        let mut finished = false;
        for _ in 0..200 {
            let (status, response) = rpc(a.clone(), json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).await;
            assert_eq!(status, 200);
            let data = validate_output(&schemas, name, &response["result"]);
            assert!(serde_json::to_vec(&data).unwrap().len() <= 2048);
            if data["truncated"] == true {
                seen.insert("pagination");
            }
            for item in data["items"].as_array().into_iter().flatten() {
                if item["kind"] == "not_inspected" {
                    seen.insert("discovery_gap");
                }
                if item["kind"] == "not_searched" {
                    seen.insert("search_gap");
                }
                if item["context_omitted"] == true {
                    seen.insert("omitted_context");
                }
                if item["text_complete"] == false {
                    seen.insert("text_fragment");
                }
                if item["kind"] == "patch_omitted" {
                    seen.insert("omitted_patch");
                }
                if item["coverage"] == "not_searched" && item.get("detail").is_some() {
                    seen.insert("history_gap");
                }
                if item["coverage"] == "not_searched" && item.get("path").is_some() {
                    seen.insert("root_history_gap");
                }
                if item.get("change").is_some() && item["change"].get("kind").is_none() {
                    seen.insert("root_change");
                }
                if item["worktree"] == "not_checked" && item.get("staged").is_none() {
                    seen.insert("status_gap");
                }
            }
            if data["next_cursor"].is_null() {
                assert_eq!(data["truncated"], false);
                finished = true;
                break;
            }
            assert_eq!(data["truncated"], true);
            args["cursor"] = data["next_cursor"].clone();
        }
        assert!(finished, "{name}: pagination did not terminate");
    }
    for expected in [
        "pagination",
        "discovery_gap",
        "search_gap",
        "omitted_context",
        "text_fragment",
        "omitted_patch",
        "history_gap",
        "root_history_gap",
        "root_change",
        "status_gap",
    ] {
        assert!(
            seen.contains(expected),
            "missing fixture coverage: {expected}"
        );
    }
    let (_, response) = rpc(a, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_file","arguments":{"root":"projects","path":"../outside"}}})).await;
    assert_eq!(response["result"]["isError"], true);
    assert!(response["result"].get("structuredContent").is_none());
    assert!(
        !response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .is_empty()
    );
}
