//! Wire schemas for the JSON values produced by the read-only tools.
//! Keep item variants aligned with fs, text, git and output::page.

use serde_json::{Map, Value, json};
use std::sync::Arc;

fn object(fields: &[(&str, Value)]) -> Value {
    let properties: Map<String, Value> = fields
        .iter()
        .map(|(name, schema)| ((*name).into(), schema.clone()))
        .collect();
    json!({"type":"object", "properties":properties,
        "required":fields.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "additionalProperties":false})
}

fn optional(mut schema: Value, fields: &[(&str, Value)]) -> Value {
    for (name, field) in fields {
        schema["properties"][*name] = field.clone();
    }
    schema
}

fn described(mut schema: Value, description: &str) -> Value {
    schema["description"] = json!(description);
    schema
}

fn string(description: &str) -> Value {
    json!({"type":"string", "description":description})
}

fn boolean(description: &str) -> Value {
    json!({"type":"boolean", "description":description})
}

fn integer(minimum: usize, description: &str) -> Value {
    json!({"type":"integer", "minimum":minimum, "description":description})
}

fn literal(value: Value) -> Value {
    let kind = if value.is_boolean() {
        "boolean"
    } else {
        "string"
    };
    json!({"type":kind, "const":value})
}

fn choices(values: &[&str], description: &str) -> Value {
    json!({"type":"string", "enum":values, "description":description})
}

fn nullable(schema: Value) -> Value {
    json!({"anyOf":[schema, {"type":"null"}]})
}

fn array(items: Value) -> Value {
    json!({"type":"array", "items":items})
}

fn variants(items: Vec<Value>) -> Value {
    json!({"oneOf":items})
}

fn wire(mut schema: Value) -> Arc<Map<String, Value>> {
    schema["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    Arc::new(schema.as_object().unwrap().clone())
}

fn path() -> Value {
    string("Path relative to the selected filesystem root or Git repository.")
}

fn oid() -> Value {
    json!({"type":"string", "pattern":"^[0-9a-f]{40}$", "description":"Full SHA-1 Git object ID."})
}

fn mode() -> Value {
    json!({"type":"string", "pattern":"^[0-7]+$", "description":"Git file mode in octal, e.g. 100644, 100755, 120000 or 160000."})
}

fn line() -> Value {
    integer(1, "Original 1-based line number.")
}

fn text() -> Value {
    string(
        "Original decoded text, preserving indentation and newline characters; may be a fragment when continuation fields are present.",
    )
}

fn pagination(mut schema: Value, reason: &str) -> Value {
    for (name, field) in [
        (
            "truncated",
            boolean(
                "True when more results remain; continue with next_cursor before drawing conclusions.",
            ),
        ),
        (
            "reason",
            described(
                nullable(literal(json!(reason))),
                "Why this page stopped; null when complete.",
            ),
        ),
        (
            "next_cursor",
            described(
                nullable(string(
                    "Opaque continuation; resend the same query with this cursor.",
                )),
                "Null when no more pages remain.",
            ),
        ),
    ] {
        schema["properties"][name] = field;
        schema["required"].as_array_mut().unwrap().push(json!(name));
    }
    schema
}

fn page(items: Value, metadata: Value) -> Arc<Map<String, Value>> {
    wire(pagination(
        object(&[
            (
                "items",
                described(
                    array(items),
                    "Ordered items in this page; different kinds have different fields.",
                ),
            ),
            ("metadata", metadata),
            (
                "total_items",
                integer(
                    0,
                    "Total logical items across all pages, not just this page; text fragments can repeat one logical item.",
                ),
            ),
        ]),
        "response_limit",
    ))
}

fn repo_metadata(extra: &[(&str, Value)]) -> Value {
    let mut fields = vec![
        ("root", string("Configured authorized root name.")),
        (
            "repo",
            string("Repository directory relative to the root; empty for the root repository."),
        ),
    ];
    fields.extend_from_slice(extra);
    object(&fields)
}

fn fragment(schema: Value) -> Value {
    optional(
        schema,
        &[
            (
                "text_byte_offset",
                integer(
                    0,
                    "UTF-8 byte offset in the original text field (change.text for history results); present on split text items.",
                ),
            ),
            (
                "text_complete",
                boolean(
                    "Whether this fragment reaches the end of the original text field; absent for unsplit items.",
                ),
            ),
        ],
    )
}

fn entry() -> Value {
    object(&[
        ("path", path()),
        (
            "kind",
            choices(&["file", "directory"], "Filesystem entry type."),
        ),
        (
            "size",
            integer(
                0,
                "Filesystem metadata length in bytes, not decoded text length.",
            ),
        ),
        (
            "modified_ns",
            integer(0, "Modification time as nanoseconds since the Unix epoch."),
        ),
    ])
}

pub fn file_info() -> Arc<Map<String, Value>> {
    wire(entry())
}

pub fn roots() -> Arc<Map<String, Value>> {
    page(
        object(&[
            (
                "name",
                string("Authorized root name to pass as the root argument."),
            ),
            (
                "path",
                string("Canonical absolute filesystem path of this authorized root."),
            ),
        ]),
        object(&[
            ("paths", literal(json!("relative to selected root"))),
            ("git_repo", literal(json!("select with root + repo"))),
            (
                "gitignore",
                described(
                    literal(json!(false)),
                    "Ignored and hidden files remain visible.",
                ),
            ),
            ("symlinks", literal(json!("rejected"))),
            ("worktrees", literal(json!("unsupported"))),
        ]),
    )
}

fn discovery_item(kind: &str) -> Value {
    variants(vec![
        object(&[("kind", literal(json!(kind))), ("entry", entry())]),
        object(&[
            ("kind", literal(json!("not_inspected"))),
            ("path", path()),
            (
                "reason",
                string(
                    "Why discovery skipped this path, e.g. depth_limit or entry_scan_limit; this is not evidence of absence.",
                ),
            ),
        ]),
    ])
}

fn discovery_metadata(scope: bool) -> Value {
    let mut fields = vec![
        ("root", string("Authorized root name.")),
        (
            "discovery_complete",
            boolean(
                "False if any discovery gaps exist, even when this result page is not truncated.",
            ),
        ),
        ("git_metadata_excluded", literal(json!(true))),
    ];
    if scope {
        fields.push(("scope", string("Requested directory relative to the root.")));
    }
    object(&fields)
}

pub fn list_directory() -> Arc<Map<String, Value>> {
    page(discovery_item("entry"), discovery_metadata(true))
}

pub fn find_files() -> Arc<Map<String, Value>> {
    page(discovery_item("file"), discovery_metadata(false))
}

pub fn read() -> Arc<Map<String, Value>> {
    wire(pagination(
        object(&[
            (
                "path",
                string(
                    "Source identifier: root:path for filesystem reads; root:repo/path@resolved_oid for historical reads.",
                ),
            ),
            (
                "encoding",
                choices(
                    &["UTF-8", "UTF-16LE", "UTF-16BE"],
                    "Detected source encoding; output text is Unicode.",
                ),
            ),
            (
                "bom",
                boolean("Whether the source has a byte-order mark, excluded from returned text."),
            ),
            (
                "lines",
                array(object(&[
                    ("line", line()),
                    ("text", text()),
                    (
                        "byte_offset",
                        integer(
                            0,
                            "UTF-8 byte offset within the decoded original line; zero for the first fragment.",
                        ),
                    ),
                    (
                        "complete",
                        boolean("True if this fragment reaches the end of the original line."),
                    ),
                ])),
            ),
            (
                "total_lines",
                integer(
                    0,
                    "Line count of the entire file, before the requested range or tail filter.",
                ),
            ),
            (
                "next_line",
                described(
                    nullable(line()),
                    "Next line to resume; null when the requested range is complete.",
                ),
            ),
            (
                "next_byte_offset",
                described(
                    nullable(integer(0, "UTF-8 byte offset within the next line.")),
                    "Null when complete.",
                ),
            ),
        ]),
        "line_or_byte_limit",
    ))
}

pub fn search() -> Arc<Map<String, Value>> {
    let matched = |extra: &[(&str, Value)]| {
        let mut fields = vec![
            ("kind", literal(json!("match"))),
            ("path", path()),
            ("line", line()),
        ];
        fields.extend_from_slice(extra);
        object(&fields)
    };
    wire(pagination(
        object(&[
            (
                "items",
                array(variants(vec![
                    matched(&[(
                        "context",
                        described(
                            array(object(&[("line", line()), ("text", text())])),
                            "Includes the matching line and requested surrounding lines.",
                        ),
                    )]),
                    matched(&[
                        ("context_omitted", literal(json!(true))),
                        (
                            "reason",
                            string(
                                "Context exceeded the byte budget; use read_file or git_read_file for the matching line.",
                            ),
                        ),
                    ]),
                    object(&[
                        ("kind", literal(json!("not_searched"))),
                        ("path", path()),
                        (
                            "reason",
                            string(
                                "Why this source was not searched; never treat this as no matches.",
                            ),
                        ),
                    ]),
                ])),
            ),
            (
                "coverage",
                object(&[
                    (
                        "fully_searched_this_page",
                        integer(
                            0,
                            "Sources completely searched on this page; excludes rejected sources.",
                        ),
                    ),
                    (
                        "remaining_sources",
                        integer(
                            0,
                            "Sources still pending, including a partially searched current source.",
                        ),
                    ),
                    (
                        "current_path",
                        described(
                            nullable(path()),
                            "Source at the continuation position; null when finished.",
                        ),
                    ),
                    (
                        "next_line",
                        described(
                            line(),
                            "Next line in current_path; ignore when current_path is null.",
                        ),
                    ),
                ]),
            ),
        ]),
        "response_or_scan_budget",
    ))
}

pub fn git_refs() -> Arc<Map<String, Value>> {
    page(
        object(&[
            (
                "name",
                string("Local ref name, including HEAD, branches, tags or remote-tracking refs."),
            ),
            ("oid", oid()),
        ]),
        repo_metadata(&[]),
    )
}

pub fn git_resolve() -> Arc<Map<String, Value>> {
    let kind = || choices(&["commit", "tree", "blob", "tag"], "Git object type.");
    let mut result = repo_metadata(&[
        ("revision", string("Requested local revision expression.")),
        ("oid", oid()),
        ("kind", kind()),
        (
            "peeled_oid",
            described(oid(), "Object ID after recursively peeling annotated tags."),
        ),
        ("peeled_kind", kind()),
    ]);
    result["description"] = json!("Resolved local Git revision and its peeled target.");
    wire(result)
}

pub fn git_tree() -> Arc<Map<String, Value>> {
    page(
        object(&[
            ("path", path()),
            ("mode", mode()),
            ("oid", oid()),
            (
                "readable_type",
                boolean(
                    "True for regular file modes only; does not guarantee text encoding or content is readable.",
                ),
            ),
        ]),
        repo_metadata(&[("revision", described(oid(), "Resolved revision object ID."))]),
    )
}

pub fn git_status() -> Arc<Map<String, Value>> {
    page(
        variants(vec![
            object(&[
                ("path", path()),
                (
                    "staged",
                    nullable(choices(
                        &["added", "deleted", "modified"],
                        "Index change relative to HEAD; null means unchanged.",
                    )),
                ),
                (
                    "worktree",
                    nullable(choices(
                        &["untracked", "deleted", "modified", "not_checked"],
                        "Worktree change relative to index; null means unchanged.",
                    )),
                ),
                (
                    "reason",
                    nullable(string(
                        "Reason worktree comparison could not be performed; otherwise null.",
                    )),
                ),
            ]),
            object(&[
                ("path", path()),
                ("worktree", literal(json!("not_checked"))),
                (
                    "reason",
                    string(
                        "Discovery gap; staged status is absent because this path was not inspected.",
                    ),
                ),
            ]),
        ]),
        repo_metadata(&[(
            "semantics",
            string("Status comparison policy and exclusions."),
        )]),
    )
}

fn patch_line() -> Value {
    object(&[
        ("kind", literal(json!("patch_line"))),
        ("path", path()),
        ("old_line", nullable(line())),
        ("new_line", nullable(line())),
        (
            "sign",
            choices(
                &["+", "-", " "],
                "Added (+), deleted (-), or unchanged context (space); the absent side has a null line number.",
            ),
        ),
        ("text", text()),
    ])
}

fn patch_omitted() -> Value {
    object(&[
        ("kind", literal(json!("patch_omitted"))),
        ("path", path()),
        (
            "reason",
            string(
                "Why the patch is unavailable, e.g. binary/media or unsupported mode; not evidence of no changes.",
            ),
        ),
    ])
}

fn patch_items() -> Vec<Value> {
    vec![
        object(&[
            ("kind", literal(json!("file"))),
            ("path", path()),
            ("old_mode", nullable(mode())),
            ("new_mode", nullable(mode())),
        ]),
        described(
            object(&[("kind", literal(json!("hunk"))), ("path", path())]),
            "Start of a patch hunk; line positions are on subsequent patch_line items.",
        ),
        fragment(patch_line()),
        patch_omitted(),
    ]
}

pub fn git_diff() -> Arc<Map<String, Value>> {
    page(
        variants(patch_items()),
        repo_metadata(&[
            ("rename_detection", literal(json!(false))),
            ("attributes_and_filters", literal(json!(false))),
        ]),
    )
}

fn commit_items() -> Vec<Value> {
    vec![
        object(&[
            ("kind", literal(json!("commit"))),
            ("oid", oid()),
            ("parents", array(oid())),
            (
                "author",
                string("Raw Git author header: name, email, Unix timestamp and timezone."),
            ),
            (
                "committer",
                string("Raw Git committer header: name, email, Unix timestamp and timezone."),
            ),
        ]),
        fragment(object(&[
            ("kind", literal(json!("message"))),
            ("oid", oid()),
            ("line", line()),
            ("text", text()),
        ])),
    ]
}

pub fn git_log() -> Arc<Map<String, Value>> {
    page(
        variants(commit_items()),
        repo_metadata(&[("order", literal(json!("breadth_first_ancestry")))]),
    )
}

pub fn git_show() -> Arc<Map<String, Value>> {
    let mut items = commit_items();
    items.extend(patch_items());
    page(
        variants(items),
        repo_metadata(&[("parent_semantics", literal(json!("first_parent")))]),
    )
}

pub fn git_blame() -> Arc<Map<String, Value>> {
    page(
        fragment(object(&[
            ("path", path()),
            ("line", line()),
            ("text", text()),
            (
                "commit",
                described(
                    oid(),
                    "Commit that introduced this line under first-parent tracing.",
                ),
            ),
            (
                "original_line",
                described(line(), "Line number in the introducing commit."),
            ),
        ])),
        repo_metadata(&[(
            "semantics",
            literal(json!("first_parent; no rename/copy following")),
        )]),
    )
}

pub fn git_merge_base() -> Arc<Map<String, Value>> {
    page(
        object(&[(
            "oid",
            described(
                oid(),
                "One best common ancestor commit; the item list is empty for unrelated histories.",
            ),
        )]),
        repo_metadata(&[]),
    )
}

pub fn git_search_history() -> Arc<Map<String, Value>> {
    let mut changed_line = patch_line();
    changed_line["properties"]["sign"] =
        choices(&["+", "-"], "Added or deleted line matching the query.");
    page(
        variants(vec![
            fragment(object(&[
                ("commit", oid()),
                (
                    "change",
                    variants(vec![
                        changed_line,
                        object(&[
                            ("path", path()),
                            ("sign", literal(json!("+"))),
                            ("new_line", line()),
                            ("text", text()),
                        ]),
                    ]),
                ),
            ])),
            object(&[
                ("commit", oid()),
                ("coverage", literal(json!("not_searched"))),
                ("detail", patch_omitted()),
            ]),
            object(&[
                ("commit", oid()),
                ("coverage", literal(json!("not_searched"))),
                ("path", path()),
                (
                    "reason",
                    string("Why this root-commit file was not searched."),
                ),
            ]),
        ]),
        repo_metadata(&[(
            "semantics",
            literal(json!("first_parent changed-line search")),
        )]),
    )
}
