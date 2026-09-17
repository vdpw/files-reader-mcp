use crate::{
    config::Config,
    output::{self, FILE_LIMIT, SCAN_LIMIT},
    text,
};
use anyhow::{Context, Result, bail, ensure};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Component, Path},
    sync::Arc,
};

#[derive(Clone)]
pub struct Root {
    pub name: String,
    pub path: String,
    pub dir: Arc<File>,
}
#[derive(Clone)]
pub struct Files {
    pub roots: Arc<BTreeMap<String, Root>>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub modified_ns: u128,
}
#[derive(Clone, Debug, Serialize)]
pub struct Gap {
    pub path: String,
    pub reason: String,
}
#[derive(Debug, Serialize)]
pub struct Inventory {
    pub entries: Vec<Entry>,
    pub gaps: Vec<Gap>,
}
pub fn validate_path(path: &str, internal: bool) -> Result<Vec<&str>> {
    ensure!(
        path.len() <= 4096 && !path.contains(['\\', '\0']),
        "INVALID_PATH"
    );
    ensure!(!Path::new(path).is_absolute(), "ABSOLUTE_PATH_FORBIDDEN");
    let mut parts = Vec::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().context("NON_UTF8_PATH")?;
                ensure!(
                    internal || !s.eq_ignore_ascii_case(".git"),
                    "GIT_METADATA_PATH_FORBIDDEN"
                );
                parts.push(s);
            }
            Component::CurDir => (),
            _ => bail!("PATH_TRAVERSAL_FORBIDDEN"),
        }
    }
    Ok(parts)
}
pub fn directory_names(dir: &File, max: usize) -> Result<(Vec<String>, bool)> {
    let mut names = Vec::new();
    let mut exhausted = true;
    for item in rustix::fs::Dir::read_from(dir)? {
        let item = item?;
        let raw = item.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        if names.len() >= max {
            exhausted = false;
            break;
        }
        names.push(
            std::str::from_utf8(raw)
                .context("NON_UTF8_DIRECTORY_ENTRY")?
                .to_owned(),
        );
    }
    names.sort();
    Ok((names, exhausted))
}
fn no_worktree(dir: &File) -> Result<()> {
    match statat(dir, ".git", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => ensure!(
            FileType::from_raw_mode(st.st_mode) == FileType::Directory,
            "WORKTREE_OR_GIT_INDIRECTION_UNSUPPORTED"
        ),
        Err(rustix::io::Errno::NOENT) => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
impl Files {
    pub fn new(config: &Config) -> Result<Self> {
        config.validate()?;
        let mut roots = BTreeMap::new();
        for r in &config.roots {
            let canonical = std::fs::canonicalize(&r.path)?;
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
            let mut dir: File = rustix::fs::open("/", flags, Mode::empty())?.into();
            for component in canonical.components() {
                if let Component::Normal(name) = component {
                    dir = openat(&dir, name, flags, Mode::empty())?.into();
                }
            }
            roots.insert(
                r.name.clone(),
                Root {
                    name: r.name.clone(),
                    path: canonical.to_str().context("NON_UTF8_ROOT")?.into(),
                    dir: Arc::new(dir),
                },
            );
        }
        Ok(Self {
            roots: Arc::new(roots),
        })
    }
    pub fn root(&self, name: &str) -> Result<&Root> {
        self.roots.get(name).context("UNKNOWN_ROOT")
    }
}
impl Root {
    pub fn open(&self, path: &str, internal: bool) -> Result<File> {
        let parts = validate_path(path, internal)?;
        let mut dir = self.dir.try_clone()?;
        if !internal {
            no_worktree(&dir)?;
        }
        for (i, p) in parts.iter().enumerate() {
            let final_component = i + 1 == parts.len();
            let flags = OFlags::RDONLY
                | OFlags::CLOEXEC
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | if final_component {
                    OFlags::empty()
                } else {
                    OFlags::DIRECTORY
                };
            let file: File = openat(&dir, *p, flags, Mode::empty())
                .with_context(|| format!("PATH_OPEN_DENIED: {path}"))?
                .into();
            let meta = file.metadata()?;
            ensure!(
                meta.is_file() || meta.is_dir(),
                "NOT_REGULAR_FILE_OR_DIRECTORY"
            );
            if !internal && meta.is_dir() {
                no_worktree(&file)?;
            }
            dir = file;
        }
        Ok(dir)
    }
    pub fn bytes(&self, path: &str, internal: bool, limit: usize) -> Result<Vec<u8>> {
        let mut file = self.open(path, internal)?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "NOT_REGULAR_FILE");
        ensure!(meta.len() <= limit as u64, "FILE_TOO_LARGE");
        let mut data = Vec::new();
        (&mut file).take(limit as u64 + 1).read_to_end(&mut data)?;
        let after = file.metadata()?;
        ensure!(
            meta.len() == after.len()
                && meta.modified()? == after.modified()?
                && data.len() as u64 == after.len(),
            "FILE_CHANGED_DURING_READ: retry"
        );
        ensure!(data.len() <= limit, "FILE_TOO_LARGE");
        Ok(data)
    }
    pub fn text(&self, path: &str) -> Result<text::Text> {
        text::decode(path, &self.bytes(path, false, FILE_LIMIT)?)
    }
    pub fn info(&self, path: &str) -> Result<Entry> {
        let meta = self.open(path, false)?.metadata()?;
        Ok(Entry {
            path: path.into(),
            kind: if meta.is_dir() { "directory" } else { "file" }.into(),
            size: meta.len(),
            modified_ns: meta
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        })
    }
    pub fn inventory(&self, path: &str, depth: usize) -> Result<Inventory> {
        ensure!((1..=32).contains(&depth), "INVALID_DEPTH");
        ensure!(
            self.open(path, false)?.metadata()?.is_dir(),
            "NOT_DIRECTORY"
        );
        let mut out = Inventory {
            entries: Vec::new(),
            gaps: Vec::new(),
        };
        let mut stack = vec![(path.trim_end_matches('/').to_owned(), 0)];
        let mut visited = 0;
        while let Some((base, level)) = stack.pop() {
            if visited >= SCAN_LIMIT {
                out.gaps.push(Gap {
                    path: base,
                    reason: "entry_scan_limit".into(),
                });
                continue;
            }
            let dir = match self.open(&base, false) {
                Ok(f) => f,
                Err(e) => {
                    out.gaps.push(Gap {
                        path: base,
                        reason: format!("{e:#}"),
                    });
                    continue;
                }
            };
            let (names, exhausted) = match directory_names(&dir, SCAN_LIMIT - visited) {
                Ok(v) => v,
                Err(e) => {
                    out.gaps.push(Gap {
                        path: base,
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            if !exhausted {
                out.gaps.push(Gap {
                    path: base.clone(),
                    reason: "entry_scan_limit; narrow directory scope".into(),
                });
            }
            for name in names {
                visited += 1;
                if name.eq_ignore_ascii_case(".git") {
                    continue;
                }
                let relative = if base.is_empty() || base == "." {
                    name
                } else {
                    format!("{base}/{name}")
                };
                match self.info(&relative) {
                    Ok(entry) => {
                        if entry.kind == "directory" {
                            if level + 1 < depth {
                                stack.push((relative.clone(), level + 1));
                            } else {
                                out.gaps.push(Gap {
                                    path: relative,
                                    reason: "depth_limit".into(),
                                });
                            }
                        }
                        out.entries.push(entry);
                    }
                    Err(e) => out.gaps.push(Gap {
                        path: relative,
                        reason: format!("{e:#}"),
                    }),
                }
            }
        }
        out.entries.sort_by(|a, b| a.path.cmp(&b.path));
        out.gaps.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }
}

pub fn matcher(pattern: &str, regex: bool) -> Result<regex::Regex> {
    ensure!(pattern.len() <= 4096, "PATTERN_TOO_LONG");
    Ok(regex::RegexBuilder::new(&if regex {
        pattern.into()
    } else {
        regex::escape(pattern)
    })
    .size_limit(2 * 1024 * 1024)
    .build()?)
}
pub fn glob(pattern: &Option<String>) -> Result<Option<globset::GlobMatcher>> {
    pattern
        .as_ref()
        .map(|p| {
            ensure!(p.len() <= 4096, "GLOB_TOO_LONG");
            Ok(globset::Glob::new(p)?.compile_matcher())
        })
        .transpose()
}

pub struct SearchOptions<'a> {
    pub pattern: &'a str,
    pub regex: bool,
    pub context: usize,
    pub limits: &'a output::Limits,
    pub cursor: &'a Option<String>,
}
/// Each source occupies one cursor index. Skipped sources are returned as explicit items.
pub fn search<F>(
    paths: &[String],
    snapshot: &str,
    opts: SearchOptions<'_>,
    mut load: F,
) -> Result<serde_json::Value>
where
    F: FnMut(&str) -> Result<text::Text>,
{
    use serde_json::json;
    ensure!(opts.context <= 10, "CONTEXT_LIMIT");
    let matcher = matcher(opts.pattern, opts.regex)?;
    let fp = output::fingerprint(format!(
        "{snapshot}\0{}\0{}\0{}",
        opts.pattern, opts.regex, opts.context
    ));
    let cur = output::cursor(opts.cursor, &fp)?;
    ensure!(cur.index <= paths.len(), "INVALID_CURSOR");
    let (line_limit, byte_limit) = opts.limits.get()?;
    let mut i = cur.index;
    let mut start_line = cur.offset;
    let mut items = Vec::new();
    let mut used = 1300;
    let mut returned_lines = 0;
    let mut scanned_bytes = 0;
    let mut fully_scanned = 0;
    'files: while i < paths.len() {
        if returned_lines >= line_limit || scanned_bytes >= output::SCAN_BYTES {
            break;
        }
        let path = &paths[i];
        let t = match load(path) {
            Ok(t) => t,
            Err(e) => {
                if e.is::<crate::git::store::MissingObject>() {
                    return Err(e);
                }
                scanned_bytes += FILE_LIMIT; // Conservatively account for a rejected full-file read.
                let row = json!({"kind":"not_searched","path":path,"reason":format!("{e:#}")});
                let size = serde_json::to_vec(&row)?.len() + 1;
                if used + size > byte_limit {
                    ensure!(!items.is_empty(), "ITEM_METADATA_EXCEEDS_BYTE_LIMIT");
                    break;
                }
                items.push(row);
                used += size;
                returned_lines += 1;
                i += 1;
                start_line = 0;
                continue;
            }
        };
        scanned_bytes += t.content.len();
        let lines = text::lines(&t.content);
        ensure!(start_line <= lines.len(), "STALE_CURSOR: source changed");
        let mut line = start_line;
        while line < lines.len() {
            if matcher.is_match(lines[line]) {
                let before = line.saturating_sub(opts.context);
                let after = (line + opts.context + 1).min(lines.len());
                let count = after - before;
                if returned_lines + count > line_limit {
                    ensure!(!items.is_empty(), "LINE_LIMIT_TOO_SMALL_FOR_CONTEXT");
                    start_line = line;
                    break 'files;
                }
                let context: Vec<_> = (before..after)
                    .map(|n| json!({"line":n+1,"text":lines[n]}))
                    .collect();
                let mut row = json!({"kind":"match","path":path,"line":line+1,"context":context});
                let mut size = serde_json::to_vec(&row)?.len() + 1;
                if size + 1300 > byte_limit {
                    row = json!({"kind":"match","path":path,"line":line+1,"context_omitted":true,"reason":"matching line/context exceeds byte budget; read this line with read_file or git_read_file"});
                    size = serde_json::to_vec(&row)?.len() + 1;
                }
                if used + size > byte_limit {
                    ensure!(!items.is_empty(), "ITEM_METADATA_EXCEEDS_BYTE_LIMIT");
                    start_line = line;
                    break 'files;
                }
                used += size;
                items.push(row);
                returned_lines += count;
                if returned_lines >= line_limit {
                    start_line = line + 1;
                    break 'files;
                }
            }
            line += 1;
        }
        i += 1;
        fully_scanned += 1;
        start_line = 0;
    }
    let incomplete = i < paths.len();
    let result = json!({"items":items,"coverage":{"fully_searched_this_page":fully_scanned,"remaining_sources":paths.len()-i,"current_path":paths.get(i),"next_line":start_line+1},
        "truncated":incomplete,"reason":if incomplete {Some("response_or_scan_budget")}else{None},
        "next_cursor":if incomplete {Some(output::continuation(&fp,i,start_line))}else{None}});
    ensure!(
        serde_json::to_vec(&result)?.len() <= byte_limit,
        "RESPONSE_BUDGET_EXCEEDED"
    );
    Ok(result)
}
