use crate::output::{self, Limits};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

pub struct Text {
    pub content: String,
    pub encoding: &'static str,
    pub bom: bool,
}
pub fn decode(path: &str, data: &[u8]) -> Result<Text> {
    ensure!(data.len() <= output::FILE_LIMIT, "FILE_TOO_LARGE");
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        ![
            "svg", "svgz", "pdf", "ps", "eps", "ai", "rtf", "doc", "docx", "xls", "xlsx", "ppt",
            "pptx", "odt", "ods", "odp", "pages", "numbers", "key", "png", "jpg", "jpeg", "gif",
            "webp", "avif", "heic", "bmp", "ico", "tiff", "mp3", "wav", "flac", "ogg", "mp4",
            "mov", "avi", "mkv", "webm", "zip", "gz", "bz2", "xz", "7z", "rar", "tar", "woff",
            "woff2", "ttf", "otf", "exe", "dll", "so", "dylib", "wasm"
        ]
        .contains(&ext.as_str()),
        "EXCLUDED_FILE_TYPE"
    );
    ensure!(infer::get(data).is_none(), "BINARY_OR_MEDIA_SIGNATURE");
    let (content, encoding, bom) =
        if data.starts_with(&[0xff, 0xfe]) || data.starts_with(&[0xfe, 0xff]) {
            ensure!(data.len().is_multiple_of(2), "INVALID_UTF16");
            let little = data[0] == 0xff;
            let units: Vec<u16> = data[2..]
                .chunks_exact(2)
                .map(|b| {
                    if little {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    }
                })
                .collect();
            (
                String::from_utf16(&units)?,
                if little { "UTF-16LE" } else { "UTF-16BE" },
                true,
            )
        } else {
            let bom = data.starts_with(&[0xef, 0xbb, 0xbf]);
            (
                std::str::from_utf8(if bom { &data[3..] } else { data })?.to_owned(),
                "UTF-8",
                bom,
            )
        };
    ensure!(
        !content
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
        "BINARY_CONTROL_CHARACTER"
    );
    let lower = content.to_ascii_lowercase();
    // Parse only the document prolog/root. Source code containing an SVG string
    // is text, while standalone SVG remains excluded even with a misleading name.
    let mut xml = quick_xml::Reader::from_str(&content);
    loop {
        match xml.read_event() {
            Ok(quick_xml::events::Event::Start(e) | quick_xml::events::Event::Empty(e)) => {
                ensure!(
                    !e.local_name().as_ref().eq_ignore_ascii_case("svg"),
                    "SVG_CONTENT_EXCLUDED"
                );
                break;
            }
            Ok(
                quick_xml::events::Event::Decl(_)
                | quick_xml::events::Event::PI(_)
                | quick_xml::events::Event::Comment(_)
                | quick_xml::events::Event::DocType(_),
            ) => (),
            Ok(quick_xml::events::Event::Text(t))
                if t.as_ref().chars().all(char::is_whitespace) => {}
            _ => break,
        }
    }
    let start = lower.trim_start();
    ensure!(
        !start.starts_with("%pdf-")
            && !start.starts_with("%!ps")
            && !start.starts_with("{\\rtf")
            && !start.starts_with("gimp palette")
            && !start.starts_with("#?radiance")
            && !start.starts_with("#?rgbe")
            && !start.starts_with("begin 644 "),
        "DOCUMENT_OR_MEDIA_CONTENT_EXCLUDED"
    );
    ensure!(
        !regex::Regex::new(r"^p[1-7]\s").unwrap().is_match(start),
        "IMAGE_CONTENT_EXCLUDED"
    );
    Ok(Text {
        content,
        encoding,
        bom,
    })
}
pub fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

pub struct ReadOptions<'a> {
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub tail: Option<usize>,
    pub limits: &'a Limits,
    pub cursor: &'a Option<String>,
}
pub fn read(path: &str, text: &Text, opts: ReadOptions<'_>) -> Result<Value> {
    let ReadOptions {
        start,
        end,
        tail,
        limits,
        cursor: input,
    } = opts;
    let all = lines(&text.content);
    let (max_lines, max_bytes) = limits.get()?;
    ensure!(
        !(tail.is_some() && (start.is_some() || end.is_some())),
        "TAIL_RANGE_CONFLICT"
    );
    ensure!(tail != Some(0), "INVALID_TAIL");
    let first = if let Some(n) = tail {
        all.len().saturating_sub(n)
    } else {
        start
            .unwrap_or(1)
            .checked_sub(1)
            .ok_or_else(|| anyhow::anyhow!("INVALID_START_LINE"))?
    };
    let last = end.unwrap_or(all.len()).min(all.len());
    ensure!(
        end.is_none_or(|e| e >= start.unwrap_or(1)),
        "INVALID_LINE_RANGE"
    );
    ensure!(first <= all.len(), "START_LINE_OUT_OF_RANGE");
    let fp = output::fingerprint(serde_json::to_vec(&(
        path,
        &text.content,
        start,
        end,
        tail,
    ))?);
    let cur = output::cursor(input, &fp)?;
    let mut line = if input.is_some() { cur.index } else { first };
    let mut offset = cur.offset;
    ensure!(line >= first && line <= last, "INVALID_CURSOR");
    let mut rows = Vec::new();
    let mut used = 1200 + path.len() * 6;
    while line < last && rows.len() < max_lines {
        let s = all[line];
        ensure!(
            offset <= s.len() && s.is_char_boundary(offset),
            "INVALID_CURSOR"
        );
        let available = max_bytes.saturating_sub(used + 160);
        if available < 16 {
            break;
        }
        let mut finish = (offset + available / 6).min(s.len());
        while !s.is_char_boundary(finish) {
            finish -= 1;
        }
        if finish == offset && finish < s.len() {
            break;
        }
        let row = json!({"line":line+1,"text":&s[offset..finish],"byte_offset":offset,"complete":finish==s.len()});
        used += serde_json::to_vec(&row)?.len() + 1;
        rows.push(row);
        if finish == s.len() {
            line += 1;
            offset = 0;
        } else {
            offset = finish;
            break;
        }
    }
    let truncated = line < last;
    let value = json!({"path":path,"encoding":text.encoding,"bom":text.bom,"lines":rows,"total_lines":all.len(),
        "truncated":truncated,"reason":if truncated {Some("line_or_byte_limit")} else {None},
        "next_cursor":if truncated {Some(output::continuation(&fp,line,offset))} else {None},
        "next_line":if truncated {Some(line+1)} else {None}, "next_byte_offset":if truncated {Some(offset)} else {None}});
    ensure!(
        serde_json::to_vec(&value)?.len() <= max_bytes,
        "RESPONSE_BUDGET_EXCEEDED"
    );
    Ok(value)
}
