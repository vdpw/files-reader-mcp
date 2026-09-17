//! Read-only Git object store. All filesystem access goes through pinned directory
//! descriptors. No Git configuration, alternates, filters, hooks, or subprocesses.
use crate::{
    fs::{Root, directory_names},
    output::{FILE_LIMIT, SCAN_LIMIT},
};
use anyhow::{Context, Result, bail, ensure};
use flate2::read::ZlibDecoder;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
use sha1::{Digest, Sha1};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

#[derive(Debug)]
pub struct MissingObject;
impl std::fmt::Display for MissingObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MISSING_OBJECT: no network fetch will be attempted")
    }
}
impl std::error::Error for MissingObject {}

#[derive(Clone, Debug)]
pub struct Object {
    pub kind: String,
    pub data: Arc<Vec<u8>>,
}
pub struct Store {
    pub work: Root,
    pub meta: Root,
    pub refs: BTreeMap<String, String>,
    indexes: Vec<(String, Vec<u8>)>,
    cache: RefCell<BTreeMap<String, Object>>,
    bytes_read: Cell<usize>,
    refs_visited: usize,
}
pub fn hash_object(kind: &str, data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("{kind} {}\0", data.len()));
    h.update(data);
    hex::encode(h.finalize())
}
pub fn exists(root: &Root, path: &str) -> Result<bool> {
    let parts = crate::fs::validate_path(path, true)?;
    let (name, parents) = parts.split_last().context("INVALID_METADATA_PATH")?;
    let parent = match root.open(&parents.join("/"), true) {
        Ok(file) => file,
        Err(e) => {
            if e.chain()
                .any(|e| e.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT))
            {
                return Ok(false);
            }
            return Err(e);
        }
    };
    match statat(&parent, *name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
pub fn is_oid(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn read_zlib(reader: impl Read) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    ZlibDecoder::new(reader)
        .take(FILE_LIMIT as u64 + 129)
        .read_to_end(&mut data)?;
    ensure!(data.len() <= FILE_LIMIT + 128, "GIT_OBJECT_TOO_LARGE");
    Ok(data)
}
impl Store {
    pub fn open(root: &Root, repo: &str) -> Result<Self> {
        let dir = root.open(repo, false)?;
        ensure!(dir.metadata()?.is_dir(), "REPO_NOT_DIRECTORY");
        let st = statat(&dir, ".git", AtFlags::SYMLINK_NOFOLLOW).context(
            "NOT_A_REPOSITORY: repo must name a repository root beneath the authorized root",
        )?;
        ensure!(
            FileType::from_raw_mode(st.st_mode) == FileType::Directory,
            "WORKTREE_OR_GIT_INDIRECTION_UNSUPPORTED"
        );
        let meta_dir: File = openat(
            &dir,
            ".git",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?
        .into();
        let meta = Root {
            name: root.name.clone(),
            path: format!("{}/{repo}/.git", root.path),
            dir: Arc::new(meta_dir),
        };
        for name in ["commondir", "gitdir", "worktrees"] {
            ensure!(!exists(&meta, name)?, "WORKTREE_UNSUPPORTED: {name}");
        }
        for name in [
            "shallow",
            "objects/info/alternates",
            "objects/info/http-alternates",
            "reftable",
        ] {
            ensure!(!exists(&meta, name)?, "GIT_STORAGE_UNSUPPORTED: {name}");
        }
        // Never parse config (including include.path); format is checked structurally.
        let work = Root {
            name: root.name.clone(),
            path: format!("{}/{repo}", root.path),
            dir: Arc::new(dir),
        };
        let mut store = Self {
            work,
            meta,
            refs: BTreeMap::new(),
            indexes: Vec::new(),
            cache: RefCell::new(BTreeMap::new()),
            bytes_read: Cell::new(0),
            refs_visited: 0,
        };
        if exists(&store.meta, "packed-refs")? {
            let data = store.meta.bytes("packed-refs", true, FILE_LIMIT)?;
            for line in std::str::from_utf8(&data)?
                .lines()
                .filter(|l| !l.starts_with(['#', '^']) && !l.is_empty())
            {
                let (id, name) = line.split_once(' ').context("INVALID_PACKED_REFS")?;
                ensure!(is_oid(id), "UNSUPPORTED_OBJECT_FORMAT_OR_INVALID_REF");
                crate::fs::validate_path(name, true)?;
                ensure!(name.starts_with("refs/"), "INVALID_REF");
                store.refs.insert(name.into(), id.into());
            }
        }
        if exists(&store.meta, "refs")? {
            store.load_refs("refs", 0)?;
        }
        let head = String::from_utf8(store.meta.bytes("HEAD", true, 4096)?)?;
        store.refs.insert("HEAD".into(), head.trim().into());
        // Resolve symbolic references after all loose and packed refs are loaded.
        for name in store.refs.keys().cloned().collect::<Vec<_>>() {
            let mut value = store.refs[&name].clone();
            for _ in 0..16 {
                if let Some(target) = value.strip_prefix("ref: ") {
                    crate::fs::validate_path(target, true)?;
                    if let Some(v) = store.refs.get(target) {
                        value = v.clone();
                    } else {
                        value.clear();
                        break;
                    }
                } else {
                    break;
                }
            }
            if value.is_empty() {
                store.refs.remove(&name);
            } else {
                ensure!(is_oid(&value), "INVALID_OR_CYCLIC_REF");
                store.refs.insert(name, value.to_ascii_lowercase());
            }
        }
        if exists(&store.meta, "objects/pack")? {
            let dir = store.meta.open("objects/pack", true)?;
            let (names, complete) = directory_names(&dir, 512)?;
            ensure!(complete, "TOO_MANY_PACK_FILES");
            let mut total = 0;
            for n in names.iter().filter(|n| n.ends_with(".idx")) {
                let bytes =
                    store
                        .meta
                        .bytes(&format!("objects/pack/{n}"), true, 64 * 1024 * 1024)?;
                total += bytes.len();
                ensure!(total <= 128 * 1024 * 1024, "PACK_INDEX_BUDGET_EXCEEDED");
                validate_index(&bytes)?;
                store.indexes.push((
                    format!("objects/pack/{}.pack", n.trim_end_matches(".idx")),
                    bytes,
                ));
            }
        }
        Ok(store)
    }
    fn load_refs(&mut self, path: &str, depth: usize) -> Result<()> {
        ensure!(
            depth <= 32 && self.refs.len() < SCAN_LIMIT,
            "REF_SCAN_LIMIT"
        );
        let dir = self.meta.open(path, true)?;
        let (names, complete) = directory_names(&dir, SCAN_LIMIT - self.refs.len())?;
        ensure!(complete, "REF_SCAN_LIMIT");
        for n in names {
            self.refs_visited += 1;
            ensure!(self.refs_visited <= SCAN_LIMIT, "REF_SCAN_LIMIT");
            let p = format!("{path}/{n}");
            let file = self.meta.open(&p, true)?;
            if file.metadata()?.is_dir() {
                self.load_refs(&p, depth + 1)?;
            } else {
                let value = String::from_utf8(self.meta.bytes(&p, true, 4096)?)?;
                self.refs.insert(p, value.trim().into());
            }
        }
        Ok(())
    }
    pub fn object(&self, id: &str) -> Result<Object> {
        self.object_depth(id, 0)
    }
    fn object_depth(&self, id: &str, depth: usize) -> Result<Object> {
        ensure!(is_oid(id), "INVALID_OBJECT_ID");
        ensure!(depth < 64, "DELTA_DEPTH_LIMIT");
        if let Some(o) = self.cache.borrow().get(id) {
            return Ok(o.clone());
        }
        let loose = format!("objects/{}/{}", &id[..2], &id[2..]);
        let object = if exists(&self.meta, &loose)? {
            let file = self.meta.open(&loose, true)?;
            ensure!(file.metadata()?.is_file(), "NOT_REGULAR_GIT_OBJECT");
            let bytes = read_zlib(file.take(FILE_LIMIT as u64 + 1024))?;
            let end = bytes
                .iter()
                .position(|&c| c == 0)
                .context("INVALID_LOOSE_OBJECT")?;
            let header = std::str::from_utf8(&bytes[..end])?;
            let (kind, size) = header.split_once(' ').context("INVALID_OBJECT_HEADER")?;
            ensure!(
                size.parse::<usize>()? == bytes.len() - end - 1,
                "OBJECT_SIZE_MISMATCH"
            );
            Object {
                kind: kind.into(),
                data: Arc::new(bytes[end + 1..].to_vec()),
            }
        } else {
            let mut found = None;
            for (pack, index) in &self.indexes {
                if let Some(offset) = find_offset(index, id)? {
                    found = Some(self.pack_object(pack, offset, depth + 1)?);
                    break;
                }
            }
            found.ok_or(MissingObject)?
        };
        ensure!(
            ["blob", "tree", "commit", "tag"].contains(&object.kind.as_str()),
            "INVALID_OBJECT_KIND"
        );
        ensure!(object.data.len() <= FILE_LIMIT, "GIT_OBJECT_TOO_LARGE");
        ensure!(
            hash_object(&object.kind, &object.data) == id,
            "GIT_OBJECT_HASH_MISMATCH"
        );
        let total = self.bytes_read.get() + object.data.len();
        ensure!(
            total <= 128 * 1024 * 1024,
            "GIT_QUERY_OBJECT_BUDGET_EXCEEDED: narrow query"
        );
        self.bytes_read.set(total);
        if total <= 32 * 1024 * 1024 {
            self.cache.borrow_mut().insert(id.into(), object.clone());
        }
        Ok(object)
    }
    fn pack_object(&self, path: &str, offset: u64, depth: usize) -> Result<Object> {
        ensure!(depth < 64, "DELTA_DEPTH_LIMIT");
        let mut file = self.meta.open(path, true)?;
        ensure!(file.metadata()?.is_file(), "NOT_REGULAR_PACK_FILE");
        let size = file.metadata()?.len();
        ensure!(
            size >= 32 && offset >= 12 && offset < size - 20,
            "INVALID_PACK_OFFSET"
        );
        let mut header = [0; 12];
        file.read_exact(&mut header)?;
        ensure!(
            &header[..4] == b"PACK"
                && matches!(u32::from_be_bytes(header[4..8].try_into()?), 2 | 3),
            "INVALID_PACK_HEADER"
        );
        file.seek(SeekFrom::Start(offset))?;
        let first = byte(&mut file)?;
        let kind = (first >> 4) & 7;
        let mut len = (first & 15) as u64;
        let mut shift = 4;
        let mut b = first;
        while b & 128 != 0 {
            ensure!(shift < 32, "PACK_SIZE_OVERFLOW");
            b = byte(&mut file)?;
            len |= ((b & 127) as u64) << shift;
            shift += 7;
        }
        ensure!(len <= FILE_LIMIT as u64, "GIT_OBJECT_TOO_LARGE");
        let mut base = None;
        if kind == 6 {
            let mut b = byte(&mut file)?;
            let mut distance = (b & 127) as u64;
            let mut count = 0;
            while b & 128 != 0 {
                count += 1;
                ensure!(count < 8, "DELTA_OFFSET_OVERFLOW");
                b = byte(&mut file)?;
                distance = ((distance + 1) << 7) | (b & 127) as u64;
            }
            ensure!(distance > 0 && distance < offset, "INVALID_DELTA_OFFSET");
            base = Some(self.pack_object(path, offset - distance, depth + 1)?);
        } else if kind == 7 {
            let mut id = [0; 20];
            file.read_exact(&mut id)?;
            base = Some(self.object_depth(&hex::encode(id), depth + 1)?);
        }
        let data = read_zlib(file.take(FILE_LIMIT as u64 + 1024))?;
        let used = self.bytes_read.get() + data.len();
        ensure!(
            used <= 128 * 1024 * 1024,
            "GIT_QUERY_OBJECT_BUDGET_EXCEEDED"
        );
        self.bytes_read.set(used);
        ensure!(data.len() == len as usize, "PACK_OBJECT_SIZE_MISMATCH");
        if let Some(base) = base {
            Ok(Object {
                kind: base.kind,
                data: Arc::new(apply_delta(&base.data, &data)?),
            })
        } else {
            Ok(Object {
                kind: match kind {
                    1 => "commit",
                    2 => "tree",
                    3 => "blob",
                    4 => "tag",
                    _ => bail!("INVALID_PACK_OBJECT_KIND"),
                }
                .into(),
                data: Arc::new(data),
            })
        }
    }
    pub fn prefix(&self, prefix: &str) -> Result<String> {
        ensure!(
            (4..=40).contains(&prefix.len()) && prefix.bytes().all(|c| c.is_ascii_hexdigit()),
            "INVALID_REVISION"
        );
        let prefix = prefix.to_ascii_lowercase();
        let mut ids = std::collections::BTreeSet::new();
        let dir = format!("objects/{}", &prefix[..2]);
        if exists(&self.meta, &dir)? {
            let (names, complete) = directory_names(&self.meta.open(&dir, true)?, SCAN_LIMIT)?;
            ensure!(complete, "OBJECT_PREFIX_SCAN_LIMIT");
            for n in names {
                let id = format!("{}{n}", &prefix[..2]);
                if is_oid(&id) && id.starts_with(&prefix) {
                    ids.insert(id);
                }
            }
        }
        for (_, idx) in &self.indexes {
            let count = u32_at(idx, 8 + 255 * 4)? as usize;
            for n in 0..count {
                let id = hex::encode(slice(idx, 1032 + n * 20, 20)?);
                if id.starts_with(&prefix) {
                    ids.insert(id);
                }
            }
        }
        ensure!(ids.len() == 1, "REVISION_MISSING_OR_AMBIGUOUS");
        Ok(ids.into_iter().next().unwrap())
    }
}
fn byte(r: &mut impl Read) -> Result<u8> {
    let mut b = [0];
    r.read_exact(&mut b)?;
    Ok(b[0])
}
pub fn slice(b: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    b.get(offset..offset.checked_add(len).context("SIZE_OVERFLOW")?)
        .context("TRUNCATED_GIT_DATA")
}
pub fn u32_at(b: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(slice(b, offset, 4)?.try_into()?))
}
fn validate_index(b: &[u8]) -> Result<()> {
    ensure!(
        slice(b, 0, 4)? == b"\xfftOc" && u32_at(b, 4)? == 2,
        "PACK_INDEX_VERSION_UNSUPPORTED"
    );
    let n = u32_at(b, 1028)? as usize;
    ensure!(n <= 2_000_000, "PACK_INDEX_TOO_LARGE");
    ensure!(b.len() >= 1032 + n * 28 + 40, "TRUNCATED_PACK_INDEX");
    ensure!(
        Sha1::digest(&b[..b.len() - 20]).as_slice() == &b[b.len() - 20..],
        "PACK_INDEX_CHECKSUM_MISMATCH"
    );
    Ok(())
}
fn find_offset(idx: &[u8], id: &str) -> Result<Option<u64>> {
    let raw = hex::decode(id)?;
    let n = u32_at(idx, 1028)? as usize;
    let mut low = 0;
    let mut high = n;
    while low < high {
        let mid = (low + high) / 2;
        let candidate = slice(idx, 1032 + mid * 20, 20)?;
        match candidate.cmp(&raw) {
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
            std::cmp::Ordering::Equal => {
                let offset = u32_at(idx, 1032 + n * 24 + mid * 4)?;
                return Ok(Some(if offset & 0x80000000 == 0 {
                    offset as u64
                } else {
                    u64::from_be_bytes(
                        slice(idx, 1032 + n * 28 + (offset & 0x7fffffff) as usize * 8, 8)?
                            .try_into()?,
                    )
                }));
            }
        }
    }
    Ok(None)
}
fn varint(data: &[u8], pos: &mut usize) -> Result<usize> {
    let mut n = 0usize;
    let mut shift = 0;
    loop {
        ensure!(shift < 32, "DELTA_SIZE_OVERFLOW");
        let b = *data.get(*pos).context("TRUNCATED_DELTA")?;
        *pos += 1;
        n |= ((b & 127) as usize) << shift;
        if b & 128 == 0 {
            return Ok(n);
        }
        shift += 7;
    }
}
fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let mut p = 0;
    ensure!(
        varint(delta, &mut p)? == base.len(),
        "DELTA_BASE_SIZE_MISMATCH"
    );
    let len = varint(delta, &mut p)?;
    ensure!(len <= FILE_LIMIT, "GIT_OBJECT_TOO_LARGE");
    let mut out = Vec::with_capacity(len);
    while p < delta.len() {
        let op = delta[p];
        p += 1;
        ensure!(op != 0, "INVALID_DELTA_OPCODE");
        if op & 128 != 0 {
            let mut offset = 0;
            let mut size = 0;
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    offset |= (*delta.get(p).context("TRUNCATED_DELTA")? as usize) << (8 * i);
                    p += 1;
                }
            }
            for i in 0..3 {
                if op & (1 << (4 + i)) != 0 {
                    size |= (*delta.get(p).context("TRUNCATED_DELTA")? as usize) << (8 * i);
                    p += 1;
                }
            }
            if size == 0 {
                size = 65536;
            }
            ensure!(out.len() + size <= len, "DELTA_OUTPUT_OVERFLOW");
            out.extend_from_slice(slice(base, offset, size)?);
        } else {
            let n = op as usize;
            ensure!(out.len() + n <= len, "DELTA_OUTPUT_OVERFLOW");
            out.extend_from_slice(slice(delta, p, n)?);
            p += n;
        }
    }
    ensure!(out.len() == len, "DELTA_OUTPUT_SIZE_MISMATCH");
    Ok(out)
}
