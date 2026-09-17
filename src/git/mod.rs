pub mod store;
use crate::{
    fs,
    output::{self, FILE_LIMIT, SCAN_LIMIT},
    text,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};
use store::{Store, hash_object, is_oid, slice, u32_at};

#[derive(Clone, Debug, Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub mode: u32,
    pub oid: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Commit {
    pub oid: String,
    pub tree: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committer: String,
    pub message: String,
}
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffScope {
    Worktree,
    Staged,
    Revisions,
}
impl Store {
    pub fn resolve(&self, revision: &str) -> Result<String> {
        ensure!(
            !revision.is_empty()
                && revision.len() <= 1024
                && !revision.contains([':', '@', '\\', '\0', ' ']),
            "INVALID_REVISION"
        );
        let split = revision.find(['~', '^']).unwrap_or(revision.len());
        let name = &revision[..split];
        let mut ids = BTreeSet::new();
        for candidate in [
            name.to_owned(),
            format!("refs/{name}"),
            format!("refs/heads/{name}"),
            format!("refs/tags/{name}"),
            format!("refs/remotes/{name}"),
        ] {
            if let Some(id) = self.refs.get(&candidate) {
                ids.insert(id.clone());
            }
        }
        let mut id = if is_oid(name) {
            name.to_ascii_lowercase()
        } else if ids.len() == 1 {
            ids.into_iter().next().unwrap()
        } else {
            ensure!(ids.is_empty(), "AMBIGUOUS_REVISION");
            self.prefix(name)?
        };
        let mut suffix = &revision[split..];
        while !suffix.is_empty() {
            let op = suffix.as_bytes()[0];
            let end = suffix[1..].find(['~', '^']).map_or(suffix.len(), |n| n + 1);
            let count = if end == 1 {
                1
            } else {
                suffix[1..end].parse::<usize>()?
            };
            ensure!(count <= 10_000, "REVISION_WALK_LIMIT");
            id = self.peel(&id)?;
            if op == b'~' {
                for _ in 0..count {
                    id = self
                        .commit(&id)?
                        .parents
                        .first()
                        .context("MISSING_PARENT")?
                        .clone();
                }
            } else if count > 0 {
                id = self
                    .commit(&id)?
                    .parents
                    .get(count - 1)
                    .context("MISSING_PARENT")?
                    .clone();
            }
            suffix = &suffix[end..];
        }
        self.object(&id)?;
        Ok(id)
    }
    pub fn peel(&self, id: &str) -> Result<String> {
        let mut id = id.to_owned();
        for _ in 0..16 {
            let object = self.object(&id)?;
            if object.kind != "tag" {
                return Ok(id);
            }
            let header = std::str::from_utf8(&object.data)?;
            id = header
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("object "))
                .context("INVALID_TAG")?
                .into();
        }
        bail!("TAG_DEPTH_LIMIT")
    }
    pub fn commit(&self, id: &str) -> Result<Commit> {
        let id = self.peel(id)?;
        let object = self.object(&id)?;
        ensure!(object.kind == "commit", "NOT_COMMIT");
        let data = std::str::from_utf8(&object.data).context("COMMIT_ENCODING_UNSUPPORTED")?;
        ensure!(
            !data
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
            "BINARY_COMMIT_METADATA"
        );
        let (header, message) = data.split_once("\n\n").context("INVALID_COMMIT")?;
        let mut c = Commit {
            oid: id,
            tree: String::new(),
            parents: Vec::new(),
            author: String::new(),
            committer: String::new(),
            message: message.into(),
        };
        for line in header.lines() {
            if let Some(v) = line.strip_prefix("tree ") {
                ensure!(is_oid(v), "INVALID_TREE_ID");
                c.tree = v.into();
            }
            if let Some(v) = line.strip_prefix("parent ") {
                ensure!(is_oid(v), "INVALID_PARENT_ID");
                c.parents.push(v.into());
            }
            if let Some(v) = line.strip_prefix("author ") {
                c.author = v.into();
            }
            if let Some(v) = line.strip_prefix("committer ") {
                c.committer = v.into();
            }
            if let Some(v) = line.strip_prefix("encoding ") {
                ensure!(
                    v.eq_ignore_ascii_case("utf-8"),
                    "COMMIT_ENCODING_UNSUPPORTED"
                );
            }
        }
        ensure!(is_oid(&c.tree), "INVALID_COMMIT_TREE");
        Ok(c)
    }
    pub fn tree(&self, revision: &str) -> Result<BTreeMap<String, TreeEntry>> {
        let id = self.peel(&self.resolve(revision)?)?;
        let root = if self.object(&id)?.kind == "tree" {
            id
        } else {
            self.commit(&id)?.tree
        };
        let mut out = BTreeMap::new();
        self.walk_tree(&root, "", 0, &mut out)?;
        Ok(out)
    }
    fn walk_tree(
        &self,
        id: &str,
        prefix: &str,
        depth: usize,
        out: &mut BTreeMap<String, TreeEntry>,
    ) -> Result<()> {
        ensure!(
            depth <= 32 && out.len() < SCAN_LIMIT,
            "GIT_TREE_SCAN_LIMIT: tree too large"
        );
        let object = self.object(id)?;
        ensure!(object.kind == "tree", "NOT_TREE");
        let mut p = 0;
        while p < object.data.len() {
            let space = object.data[p..]
                .iter()
                .position(|&c| c == b' ')
                .context("INVALID_TREE")?
                + p;
            let mode = u32::from_str_radix(std::str::from_utf8(&object.data[p..space])?, 8)?;
            let nul = object.data[space + 1..]
                .iter()
                .position(|&c| c == 0)
                .context("INVALID_TREE")?
                + space
                + 1;
            let name =
                std::str::from_utf8(&object.data[space + 1..nul]).context("NON_UTF8_GIT_PATH")?;
            ensure!(
                !name.contains('/') && name != "." && !name.is_empty(),
                "INVALID_TREE_PATH"
            );
            let path = if prefix.is_empty() {
                name.into()
            } else {
                format!("{prefix}/{name}")
            };
            fs::validate_path(&path, false)?;
            let oid = hex::encode(slice(&object.data, nul + 1, 20)?);
            p = nul + 21;
            if mode == 0o40000 {
                self.walk_tree(&oid, &path, depth + 1, out)?;
            } else {
                ensure!(out.len() < SCAN_LIMIT, "GIT_TREE_SCAN_LIMIT");
                out.insert(path.clone(), TreeEntry { path, mode, oid });
            }
        }
        Ok(())
    }
    pub fn blob_text(&self, entry: &TreeEntry) -> Result<text::Text> {
        fs::validate_path(&entry.path, false)?;
        ensure!(
            matches!(entry.mode, 0o100644 | 0o100755),
            "GIT_SYMLINK_OR_SUBMODULE_UNSUPPORTED"
        );
        let o = self.object(&entry.oid)?;
        ensure!(o.kind == "blob", "NOT_BLOB");
        text::decode(&entry.path, &o.data)
    }
    /// Walk only the requested historical path, without scanning unrelated subtrees.
    pub fn lookup(&self, revision: &str, path: &str) -> Result<Option<TreeEntry>> {
        let parts = fs::validate_path(path, false)?;
        ensure!(
            !parts.is_empty() && parts.len() <= 32,
            "INVALID_GIT_FILE_PATH"
        );
        let id = self.peel(&self.resolve(revision)?)?;
        let mut tree_id = if self.object(&id)?.kind == "tree" {
            id
        } else {
            self.commit(&id)?.tree
        };
        for (level, part) in parts.iter().enumerate() {
            let tree = self.object(&tree_id)?;
            ensure!(tree.kind == "tree", "NOT_TREE");
            let mut pos = 0;
            let mut found = None;
            while pos < tree.data.len() {
                let space = tree.data[pos..]
                    .iter()
                    .position(|&c| c == b' ')
                    .context("INVALID_TREE")?
                    + pos;
                let mode = u32::from_str_radix(std::str::from_utf8(&tree.data[pos..space])?, 8)?;
                let end = tree.data[space + 1..]
                    .iter()
                    .position(|&c| c == 0)
                    .context("INVALID_TREE")?
                    + space
                    + 1;
                let name = &tree.data[space + 1..end];
                let oid = hex::encode(slice(&tree.data, end + 1, 20)?);
                pos = end + 21;
                if name == part.as_bytes() {
                    found = Some((mode, oid));
                    break;
                }
            }
            let Some((mode, oid)) = found else {
                return Ok(None);
            };
            if level + 1 == parts.len() {
                return Ok(Some(TreeEntry {
                    path: parts.join("/"),
                    mode,
                    oid,
                }));
            }
            ensure!(mode == 0o40000, "GIT_SYMLINK_OR_SUBMODULE_UNSUPPORTED");
            tree_id = oid;
        }
        unreachable!("nonempty path checked above")
    }
    pub fn read_text(&self, revision: &str, path: &str) -> Result<text::Text> {
        self.blob_text(
            &self
                .lookup(revision, path)?
                .context("PATH_NOT_IN_REVISION")?,
        )
    }
    pub fn log(&self, revision: &str, max: usize) -> Result<Vec<Commit>> {
        let first = self.peel(&self.resolve(revision)?)?;
        let mut queue = VecDeque::from([first]);
        let mut seen = BTreeSet::new();
        let mut commits = Vec::new();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            ensure!(
                commits.len() < max,
                "HISTORY_WALK_LIMIT: narrow the starting revision"
            );
            let c = self.commit(&id)?;
            for p in &c.parents {
                queue.push_back(p.clone());
            }
            commits.push(c);
        }
        Ok(commits)
    }
    pub fn index(&self) -> Result<BTreeMap<String, TreeEntry>> {
        if !store::exists(&self.meta, "index")? {
            return Ok(BTreeMap::new());
        }
        let b = self.meta.bytes("index", true, FILE_LIMIT)?;
        ensure!(
            b.len() >= 32 && slice(&b, 0, 4)? == b"DIRC",
            "INVALID_INDEX"
        );
        ensure!(
            Sha1::digest(&b[..b.len() - 20]).as_slice() == &b[b.len() - 20..],
            "INDEX_CHECKSUM_MISMATCH"
        );
        let version = u32_at(&b, 4)?;
        ensure!((2..=4).contains(&version), "INDEX_VERSION_UNSUPPORTED");
        let count = u32_at(&b, 8)? as usize;
        ensure!(count <= SCAN_LIMIT, "INDEX_ENTRY_LIMIT");
        let mut p = 12;
        let mut previous = String::new();
        let mut out = BTreeMap::new();
        for _ in 0..count {
            let start = p;
            let mode = u32_at(&b, p + 24)?;
            let oid = hex::encode(slice(&b, p + 40, 20)?);
            let flags = u16::from_be_bytes(slice(&b, p + 60, 2)?.try_into()?);
            p += 62;
            ensure!(flags & 0x3000 == 0, "UNMERGED_INDEX_UNSUPPORTED");
            if flags & 0x4000 != 0 {
                let ext = u16::from_be_bytes(slice(&b, p, 2)?.try_into()?);
                p += 2;
                ensure!(ext & 0x2000 == 0, "INTENT_TO_ADD_UNSUPPORTED");
            }
            let strip = if version == 4 {
                let mut c = *b.get(p).context("TRUNCATED_INDEX")?;
                p += 1;
                let mut n = (c & 127) as usize;
                let mut steps = 0;
                while c & 128 != 0 {
                    steps += 1;
                    ensure!(steps < 5, "INDEX_PREFIX_OVERFLOW");
                    c = *b.get(p).context("TRUNCATED_INDEX")?;
                    p += 1;
                    n = ((n + 1) << 7) | (c & 127) as usize;
                }
                n
            } else {
                0
            };
            let end = b
                .get(p..)
                .context("TRUNCATED_INDEX")?
                .iter()
                .position(|&c| c == 0)
                .context("INVALID_INDEX_PATH")?
                + p;
            let suffix = std::str::from_utf8(&b[p..end]).context("NON_UTF8_INDEX_PATH")?;
            let path = if version == 4 {
                ensure!(
                    strip <= previous.len() && previous.is_char_boundary(previous.len() - strip),
                    "INVALID_INDEX_PREFIX"
                );
                format!("{}{suffix}", &previous[..previous.len() - strip])
            } else {
                suffix.into()
            };
            fs::validate_path(&path, false)?;
            ensure!(mode != 0o40000, "SPARSE_INDEX_UNSUPPORTED");
            previous = path.clone();
            p = end + 1;
            if version < 4 {
                p = start + (p - start).div_ceil(8) * 8;
            }
            out.insert(path.clone(), TreeEntry { path, mode, oid });
        }
        while p < b.len() - 20 {
            let kind = slice(&b, p, 4)?;
            let len = u32_at(&b, p + 4)? as usize;
            ensure!(
                kind[0].is_ascii_uppercase(),
                "REQUIRED_INDEX_EXTENSION_UNSUPPORTED"
            );
            slice(&b, p + 8, len)?;
            p += 8 + len;
        }
        ensure!(p == b.len() - 20, "INVALID_INDEX_LENGTH");
        Ok(out)
    }
    pub fn status(&self) -> Result<Value> {
        let index = self.index()?;
        let head = if let Some(id) = self.refs.get("HEAD") {
            self.tree(id)?
        } else {
            BTreeMap::new()
        };
        let inv = self.work.inventory("", 32)?;
        let files: BTreeMap<_, _> = inv
            .entries
            .iter()
            .filter(|e| e.kind == "file")
            .map(|e| (e.path.clone(), e))
            .collect();
        let paths: BTreeSet<_> = head
            .keys()
            .chain(index.keys())
            .chain(files.keys())
            .cloned()
            .collect();
        let mut rows = Vec::new();
        let mut read_bytes = 0usize;
        for path in paths {
            let h = head.get(&path);
            let i = index.get(&path);
            let staged = match (h, i) {
                (None, Some(_)) => Some("added"),
                (Some(_), None) => Some("deleted"),
                (Some(a), Some(b)) if a.oid != b.oid || a.mode != b.mode => Some("modified"),
                _ => None,
            };
            let (worktree, reason) = match (i, files.get(&path)) {
                (None, Some(_)) => (Some("untracked"), None),
                (Some(_), None) => {
                    let blocked = inv.gaps.iter().find(|g| {
                        path == g.path
                            || path.starts_with(&format!("{}/", g.path))
                            || g.path.is_empty()
                    });
                    if let Some(g) = blocked {
                        (Some("not_checked"), Some(g.reason.clone()))
                    } else {
                        (Some("deleted"), None)
                    }
                }
                (Some(_), Some(_)) if read_bytes >= output::SCAN_BYTES => {
                    (Some("not_checked"), Some("status_scan_byte_limit".into()))
                }
                (Some(entry), Some(_)) => match self.work.bytes(&path, false, FILE_LIMIT) {
                    Ok(b) => {
                        read_bytes += b.len();
                        #[cfg(unix)]
                        let mode = {
                            use std::os::unix::fs::PermissionsExt;
                            if self
                                .work
                                .open(&path, false)?
                                .metadata()?
                                .permissions()
                                .mode()
                                & 0o111
                                != 0
                            {
                                0o100755
                            } else {
                                0o100644
                            }
                        };
                        if hash_object("blob", &b) != entry.oid || mode != entry.mode {
                            (Some("modified"), None)
                        } else {
                            (None, None)
                        }
                    }
                    Err(e) => {
                        read_bytes += FILE_LIMIT;
                        (Some("not_checked"), Some(format!("{e:#}")))
                    }
                },
                _ => (None, None),
            };
            if staged.is_some() || worktree.is_some() {
                rows.push(json!({"path":path,"staged":staged,"worktree":worktree,"reason":reason}));
            }
        }
        for gap in inv.gaps {
            rows.push(json!({"path":gap.path,"worktree":"not_checked","reason":gap.reason}));
        }
        Ok(
            json!({"items":rows,"semantics":"raw bytes; no gitignore, attributes, filters, or rename detection"}),
        )
    }
    pub fn diff(
        &self,
        scope: DiffScope,
        from: Option<&str>,
        to: Option<&str>,
        pattern: &Option<String>,
    ) -> Result<Vec<Value>> {
        let old = match scope {
            DiffScope::Worktree => self.index()?,
            DiffScope::Staged => {
                if self.refs.contains_key("HEAD") {
                    self.tree("HEAD")?
                } else {
                    BTreeMap::new()
                }
            }
            DiffScope::Revisions => self.tree(from.context("FROM_REQUIRED")?)?,
        };
        let new = match scope {
            DiffScope::Staged => Some(self.index()?),
            DiffScope::Revisions => Some(self.tree(to.context("TO_REQUIRED")?)?),
            DiffScope::Worktree => None,
        };
        let mut paths: BTreeSet<String> = old.keys().cloned().collect();
        if let Some(new) = &new {
            paths.extend(new.keys().cloned());
        }
        let filter = fs::glob(pattern)?;
        let mut rows = Vec::new();
        let mut budget = 0;
        for path in paths {
            if filter.as_ref().is_some_and(|g| !g.is_match(&path)) {
                continue;
            }
            let before = old.get(&path);
            let after = new.as_ref().and_then(|m| m.get(&path));
            if new.is_some()
                && before
                    .zip(after)
                    .is_some_and(|(a, b)| a.oid == b.oid && a.mode == b.mode)
            {
                continue;
            }
            let mut work_mode = None;
            let left = before.map(|e| self.blob_text(e)).transpose();
            let right = if new.is_some() {
                after.map(|e| self.blob_text(e)).transpose()
            } else {
                match self.work.open(&path, false) {
                    Ok(file) => {
                        use std::os::unix::fs::PermissionsExt;
                        let meta = file.metadata()?;
                        work_mode = Some(if meta.permissions().mode() & 0o111 != 0 {
                            0o100755
                        } else {
                            0o100644
                        });
                        budget += meta.len().min(FILE_LIMIT as u64) as usize;
                        ensure!(
                            budget <= output::SCAN_BYTES,
                            "DIFF_SCAN_BUDGET_EXCEEDED: narrow path_glob"
                        );
                        self.work.text(&path).map(Some)
                    }
                    Err(e) => {
                        if e.chain().any(|e| {
                            e.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT)
                        }) {
                            Ok(None)
                        } else {
                            Err(e)
                        }
                    }
                }
            };
            for error in [left.as_ref().err(), right.as_ref().err()]
                .into_iter()
                .flatten()
            {
                if error.is::<store::MissingObject>() {
                    return Err(store::MissingObject.into());
                }
            }
            let (a, b) = match (left, right) {
                (Ok(a), Ok(b)) => (a, b),
                (a, b) => {
                    rows.push(json!({"kind":"patch_omitted","path":path,"reason":format!("left: {}; right: {}",a.err().map(|e|e.to_string()).unwrap_or_default(),b.err().map(|e|e.to_string()).unwrap_or_default())}));
                    continue;
                }
            };
            let a = a.map(|t| t.content).unwrap_or_default();
            let b = b.map(|t| t.content).unwrap_or_default();
            if a == b && before.map(|e| e.mode) == after.map(|e| e.mode) && new.is_some() {
                continue;
            }
            if a == b && new.is_none() && before.map(|e| e.mode) == work_mode {
                continue;
            }
            budget += a.len() + b.len();
            ensure!(
                budget <= output::SCAN_BYTES,
                "DIFF_SCAN_BUDGET_EXCEEDED: narrow path_glob"
            );
            rows.push(json!({"kind":"file","path":path,"old_mode":before.map(|e|format!("{:o}",e.mode)),"new_mode":after.map(|e|e.mode).or(work_mode).map(|m|format!("{m:o}"))}));
            // A deadline keeps adversarial input from monopolizing a worker. A timed-out
            // diff is still exact but may use a coarser edit sequence.
            let diff = similar::TextDiff::configure()
                .timeout(Duration::from_millis(250))
                .diff_lines(&a, &b);
            for group in diff.grouped_ops(3) {
                rows.push(json!({"kind":"hunk","path":path}));
                for op in group {
                    for change in diff.iter_changes(&op) {
                        rows.push(json!({"kind":"patch_line","path":path,"old_line":change.old_index().map(|n|n+1),"new_line":change.new_index().map(|n|n+1),"sign":match change.tag(){similar::ChangeTag::Delete=>"-",similar::ChangeTag::Insert=>"+",similar::ChangeTag::Equal=>" "},"text":change.value()}));
                    }
                }
            }
            ensure!(
                rows.len() <= 100_000,
                "DIFF_OUTPUT_SCAN_LIMIT: narrow path_glob"
            );
        }
        Ok(rows)
    }
    pub fn merge_bases(&self, a: &str, b: &str) -> Result<Vec<String>> {
        let left = self.log(a, 10_000)?;
        let right = self.log(b, 10_000)?;
        let left_ids: BTreeSet<_> = left.iter().map(|c| c.oid.clone()).collect();
        let mut common: BTreeSet<_> = right
            .iter()
            .filter(|c| left_ids.contains(&c.oid))
            .map(|c| c.oid.clone())
            .collect();
        let graph: BTreeMap<_, _> = left
            .into_iter()
            .chain(right)
            .map(|c| (c.oid, c.parents))
            .collect();
        let mut queue: VecDeque<String> = common.iter().flat_map(|id| graph[id].clone()).collect();
        let mut ancestors = BTreeSet::new();
        while let Some(id) = queue.pop_front() {
            if ancestors.insert(id.clone()) {
                queue.extend(graph.get(&id).context("MISSING_ANCESTOR")?.iter().cloned());
            }
        }
        common.retain(|id| !ancestors.contains(id));
        Ok(common.into_iter().collect())
    }
    pub fn blame(&self, revision: &str, path: &str) -> Result<Vec<Value>> {
        let mut commit = self.commit(&self.resolve(revision)?)?;
        let original = self.read_text(&commit.oid, path)?;
        let mut current = original.content.clone();
        let originals = text::lines(&original.content);
        let mut positions: Vec<Option<usize>> = (0..originals.len()).map(Some).collect();
        let mut owners = vec![String::new(); originals.len()];
        let mut source_lines: Vec<_> = (1..=originals.len()).collect();
        let mut steps = 0;
        loop {
            steps += 1;
            ensure!(steps <= 1000, "BLAME_HISTORY_LIMIT");
            let parent = commit
                .parents
                .first()
                .map(|id| self.commit(id))
                .transpose()?;
            let previous = if let Some(p) = &parent {
                self.lookup(&p.oid, path)?
                    .as_ref()
                    .map(|e| self.blob_text(e))
                    .transpose()?
                    .map(|t| t.content)
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let diff = similar::TextDiff::configure()
                .timeout(Duration::from_millis(250))
                .diff_lines(&previous, &current);
            let mut mapping = BTreeMap::new();
            for change in diff.iter_all_changes() {
                if change.tag() == similar::ChangeTag::Equal {
                    mapping.insert(change.new_index().unwrap(), change.old_index().unwrap());
                }
            }
            for n in 0..positions.len() {
                if let Some(line) = positions[n] {
                    if let Some(old) = mapping.get(&line) {
                        positions[n] = Some(*old);
                        source_lines[n] = old + 1;
                    } else {
                        owners[n] = commit.oid.clone();
                        positions[n] = None;
                    }
                }
            }
            if positions.iter().all(Option::is_none) {
                break;
            }
            match parent {
                Some(p) => {
                    current = previous;
                    commit = p;
                }
                None => {
                    for n in 0..positions.len() {
                        if positions[n].is_some() {
                            owners[n] = commit.oid.clone();
                        }
                    }
                    break;
                }
            }
        }
        Ok(originals.iter().enumerate().map(|(n,line)|json!({"path":path,"line":n+1,"text":line,"commit":owners[n],"original_line":source_lines[n]})).collect())
    }
    pub fn history_search(
        &self,
        revision: &str,
        pattern: &str,
        is_regex: bool,
        path_glob: &Option<String>,
    ) -> Result<Vec<Value>> {
        let matcher = fs::matcher(pattern, is_regex)?;
        let mut c = self.commit(&self.resolve(revision)?)?;
        let mut rows = Vec::new();
        let mut steps = 0;
        loop {
            steps += 1;
            ensure!(
                steps <= 1000,
                "HISTORY_SEARCH_LIMIT: start at an older revision"
            );
            if let Some(parent) = c.parents.first() {
                let changes =
                    self.diff(DiffScope::Revisions, Some(parent), Some(&c.oid), path_glob)?;
                for row in changes {
                    if row["kind"] == "patch_omitted" {
                        rows.push(json!({"commit":c.oid,"coverage":"not_searched","detail":row}));
                    } else if (row["sign"] == "+" || row["sign"] == "-")
                        && row["text"].as_str().is_some_and(|s| matcher.is_match(s))
                    {
                        rows.push(json!({"commit":c.oid,"change":row}));
                    }
                }
                c = self.commit(parent)?;
            } else {
                let filter = fs::glob(path_glob)?;
                for (path, entry) in self.tree(&c.oid)? {
                    if filter.as_ref().is_some_and(|g| !g.is_match(&path)) {
                        continue;
                    }
                    match self.blob_text(&entry) {
                        Ok(t)=>{for (n,line) in text::lines(&t.content).iter().enumerate(){if matcher.is_match(line){rows.push(json!({"commit":c.oid,"change":{"path":path,"sign":"+","new_line":n+1,"text":line}}));}}},
                        Err(e) if e.is::<store::MissingObject>()=>return Err(e),
                        Err(e)=>rows.push(json!({"commit":c.oid,"coverage":"not_searched","path":path,"reason":e.to_string()})),
                    }
                }
                break;
            }
            ensure!(
                rows.len() <= 100_000,
                "HISTORY_RESULT_LIMIT: narrow path/pattern"
            );
        }
        Ok(rows)
    }
}
