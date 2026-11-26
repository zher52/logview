//! 链接识别:URL / POSIX 绝对路径 / Windows 路径 / 相对日志路径;JSON 行探测

use std::sync::OnceLock;
use regex::Regex;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Url,
    Path,
}

#[derive(Clone)]
pub struct Link {
    pub start: usize,
    pub end: usize,
    pub kind: LinkKind,
}

fn re_url() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"https?://[^\s"'`<>()\[\]{}\\]+"#).unwrap())
}
fn re_posix() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"/[\w.@+-]+(?:/[\w.@+-]+)+/?").unwrap())
}
fn re_win() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[A-Za-z]:\\(?:[\w.@-]+\\)+[\w.@-]+").unwrap())
}
fn re_rel() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?:\./|\.\./|(?:[\w.-]+/)+)[\w.-]+\.(?:log|txt|out|err)\b").unwrap())
}

/// 修剪 URL 尾部标点(含不成对的右括号)
fn trim_url_end(t: &str, s: usize, mut e: usize) -> usize {
    let b = t.as_bytes();
    loop {
        if e <= s + 3 {
            break;
        }
        let c = b[e - 1];
        if matches!(c, b'.' | b',' | b';' | b':' | b'!' | b'?' | b'"' | b'\'' | b'>' | b']' | b'}') {
            e -= 1;
        } else if c == b')' {
            let opens = b[s..e].iter().filter(|&&x| x == b'(').count();
            let closes_before = b[s..e - 1].iter().filter(|&&x| x == b')').count();
            if opens <= closes_before {
                e -= 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    e
}

/// 提取行内链接(URL 优先,重叠丢弃)
pub fn detect(text: &str) -> Vec<Link> {
    let mut spans: Vec<(usize, usize, LinkKind)> = Vec::new();
    for m in re_url().find_iter(text) {
        let e = trim_url_end(text, m.start(), m.end());
        if e > m.start() {
            spans.push((m.start(), e, LinkKind::Url));
        }
    }
    for m in re_win().find_iter(text) {
        spans.push((m.start(), m.end(), LinkKind::Path));
    }
    for m in re_posix().find_iter(text) {
        spans.push((m.start(), m.end(), LinkKind::Path));
    }
    for m in re_rel().find_iter(text) {
        spans.push((m.start(), m.end(), LinkKind::Path));
    }
    spans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut out: Vec<Link> = Vec::new();
    let mut last_end = 0usize;
    for (s, e, k) in spans {
        if e <= s || s < last_end {
            continue;
        }
        out.push(Link { start: s, end: e, kind: k });
        last_end = e;
    }
    out
}

/// 整行是否为合法 JSON(可 pretty 打开)
pub fn is_json_line(text: &str) -> bool {
    let t = text.trim();
    if t.len() < 2 || t.len() > 100_000 {
        return false;
    }
    if !(t.starts_with('{') || t.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(t).is_ok()
}
