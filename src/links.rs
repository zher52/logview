// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 链接识别:URL / POSIX 绝对路径 / Windows 路径 / 相对日志路径;JSON 行探测

use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    R.get_or_init(|| {
        Regex::new(r"(?:\./|\.\./|(?:[\w.-]+/)+)[\w.-]+\.(?:log|txt|out|err)\b").unwrap()
    })
}

/// 修剪 URL 尾部标点(含不成对的右括号)
fn trim_url_end(t: &str, s: usize, mut e: usize) -> usize {
    let b = t.as_bytes();
    loop {
        if e <= s + 3 {
            break;
        }
        let c = b[e - 1];
        if matches!(
            c,
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b'"' | b'\'' | b'>' | b']' | b'}'
        ) {
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
        out.push(Link {
            start: s,
            end: e,
            kind: k,
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_punctuation_from_url() {
        let text = "see https://example.com/x.";
        let links = detect(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Url);
        assert_eq!(&text[links[0].start..links[0].end], "https://example.com/x");
    }

    #[test]
    fn url_wins_over_overlapping_path() {
        let text = "open https://example.com/a/b now";
        let links = detect(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Url);
        assert_eq!(
            &text[links[0].start..links[0].end],
            "https://example.com/a/b"
        );
    }

    #[test]
    fn detects_posix_windows_and_relative_paths() {
        let text = "/var/log/app.log C:\\Users\\me\\app.log logs/run.err";
        let got: Vec<&str> = detect(text).iter().map(|l| &text[l.start..l.end]).collect();
        assert!(got.contains(&"/var/log/app.log"), "{got:?}");
        assert!(
            got.iter()
                .any(|p| p.contains(":\\") && p.ends_with("app.log")),
            "{got:?}"
        );
        assert!(got.contains(&"logs/run.err"), "{got:?}");
    }

    #[test]
    fn json_line_detection() {
        assert!(is_json_line(r#"{"a":1}"#));
        assert!(is_json_line("[1,2,3]"));
        assert!(!is_json_line("{not json}"));
        assert!(!is_json_line("plain text"));
        assert!(!is_json_line("{"));
    }
}
