use anyhow::{Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

pub const FILE_LIMIT: usize = 8 * 1024 * 1024;
pub const SCAN_LIMIT: usize = 20_000;
pub const SCAN_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Maximum source lines / entries, default 200, at most 2000.
    pub max_lines: Option<usize>,
    /// Maximum serialized result bytes, default 65536, range 2048..262144.
    pub max_bytes: Option<usize>,
}
impl Limits {
    pub fn get(&self) -> Result<(usize, usize)> {
        let lines = self.max_lines.unwrap_or(200);
        let bytes = self.max_bytes.unwrap_or(65536);
        ensure!((1..=2000).contains(&lines), "INVALID_LINE_LIMIT");
        ensure!((2048..=262144).contains(&bytes), "INVALID_BYTE_LIMIT");
        Ok((lines, bytes))
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub fingerprint: String,
    pub index: usize,
    pub offset: usize,
}
pub fn fingerprint(data: impl AsRef<[u8]>) -> String {
    hex::encode(Sha1::digest(data.as_ref()))
}
pub fn cursor(input: &Option<String>, fp: &str) -> Result<Cursor> {
    let c = match input {
        Some(s) => {
            ensure!(s.len() < 512, "INVALID_CURSOR");
            serde_json::from_str(s)?
        }
        None => Cursor {
            fingerprint: fp.into(),
            ..Default::default()
        },
    };
    ensure!(
        c.fingerprint == fp,
        "STALE_CURSOR: content or query changed; restart without cursor"
    );
    Ok(c)
}
pub fn continuation(fp: &str, index: usize, offset: usize) -> String {
    serde_json::to_string(&Cursor {
        fingerprint: fp.into(),
        index,
        offset,
    })
    .unwrap()
}
pub fn page(
    items: Vec<Value>,
    limits: &Limits,
    input: &Option<String>,
    metadata: Value,
) -> Result<Value> {
    let fp = fingerprint(serde_json::to_vec(&(&items, &metadata))?);
    let cur = cursor(input, &fp)?;
    ensure!(cur.index <= items.len(), "INVALID_CURSOR");
    let (max, bytes) = limits.get()?;
    let mut out = Vec::new();
    let mut index = cur.index;
    let mut offset = cur.offset;
    let mut used = serde_json::to_vec(&metadata)?.len() + 1024;
    while index < items.len() && out.len() < max {
        let mut row = items[index].clone();
        let field = if row.pointer("/text").is_some_and(Value::is_string) {
            Some("/text")
        } else if row.pointer("/change/text").is_some_and(Value::is_string) {
            Some("/change/text")
        } else {
            None
        };
        let size = serde_json::to_vec(&row)?.len() + 1;
        if size + used > bytes || offset > 0 {
            if let Some(field) = field {
                let original = row.pointer(field).unwrap().as_str().unwrap().to_owned();
                ensure!(
                    offset <= original.len() && original.is_char_boundary(offset),
                    "INVALID_CURSOR"
                );
                *row.pointer_mut(field).unwrap() = Value::String(String::new());
                let overhead = serde_json::to_vec(&row)?.len() + 128;
                let available = bytes.saturating_sub(used + overhead);
                let mut end = (offset + available / 6).min(original.len());
                while !original.is_char_boundary(end) {
                    end -= 1;
                }
                if end == offset && end < original.len() {
                    ensure!(!out.is_empty(), "ITEM_METADATA_EXCEEDS_BYTE_LIMIT");
                    break;
                }
                *row.pointer_mut(field).unwrap() = Value::String(original[offset..end].to_owned());
                row["text_byte_offset"] = json!(offset);
                row["text_complete"] = json!(end == original.len());
                used += serde_json::to_vec(&row)?.len() + 1;
                out.push(row);
                if end < original.len() {
                    offset = end;
                    break;
                }
                index += 1;
                offset = 0;
                continue;
            }
            if out.is_empty() {
                bail!("ITEM_METADATA_EXCEEDS_BYTE_LIMIT: increase max_bytes");
            }
            break;
        }
        ensure!(offset == 0, "INVALID_CURSOR");
        used += size;
        out.push(row);
        index += 1;
    }
    let truncated = index < items.len();
    let result = json!({"items":out,"metadata":metadata,"truncated":truncated,
        "reason":if truncated {Some("response_limit")} else {None},
        "next_cursor":if truncated {Some(continuation(&fp,index,offset))} else {None},"total_items":items.len()});
    ensure!(
        serde_json::to_vec(&result)?.len() <= bytes,
        "RESPONSE_BUDGET_EXCEEDED"
    );
    Ok(result)
}
