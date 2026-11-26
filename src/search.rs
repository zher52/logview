// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 检索:关键字(文本)/正则 × 大小写开关;高亮/过滤模式由 UI 层维护

#[derive(Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub regex: bool,
    pub case_sensitive: bool,
}

#[derive(Clone)]
pub enum Matcher {
    Text { needle: String, cs: bool },
    Re { re: regex::Regex },
}

/// 编译搜索状态;正则非法时返回 Err(错误文本)
pub fn compile(s: &SearchState) -> Result<Option<Matcher>, String> {
    if s.query.is_empty() {
        return Ok(None);
    }
    if s.regex {
        let re = regex::RegexBuilder::new(&s.query)
            .case_insensitive(!s.case_sensitive)
            .size_limit(16 << 20)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Some(Matcher::Re { re }))
    } else {
        let needle = if s.case_sensitive {
            s.query.clone()
        } else {
            s.query.to_lowercase()
        };
        Ok(Some(Matcher::Text {
            needle,
            cs: s.case_sensitive,
        }))
    }
}

impl Matcher {
    pub fn matches(&self, t: &str) -> bool {
        match self {
            Matcher::Text { needle, cs } => {
                if *cs {
                    t.contains(needle.as_str())
                } else {
                    t.to_lowercase().contains(needle.as_str())
                }
            }
            Matcher::Re { re } => re.is_match(t),
        }
    }

    /// 计算高亮区间(升序,互不重叠)
    pub fn spans(&self, t: &str, out: &mut Vec<(usize, usize)>) {
        out.clear();
        match self {
            Matcher::Text { needle, cs } => {
                if needle.is_empty() {
                    return;
                }
                if *cs {
                    let mut from = 0;
                    while let Some(p) = t[from..].find(needle.as_str()) {
                        let s = from + p;
                        out.push((s, s + needle.len()));
                        from = s + needle.len().max(1);
                    }
                } else {
                    let lower = t.to_lowercase();
                    let mut from = 0;
                    while let Some(p) = lower[from..].find(needle.as_str()) {
                        let s = from + p;
                        out.push((s, s + needle.len()));
                        from = s + needle.len().max(1);
                    }
                }
            }
            Matcher::Re { re } => {
                for m in re.find_iter(t) {
                    out.push((m.start(), m.end()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(q: &str) -> SearchState {
        SearchState {
            query: q.into(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_query_compiles_to_none() {
        assert!(compile(&state("")).unwrap().is_none());
    }

    #[test]
    fn text_match_respects_case_switch() {
        let m = compile(&state("error")).unwrap().unwrap();
        assert!(m.matches("an ERROR occurred"));

        let st = SearchState {
            query: "error".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let m = compile(&st).unwrap().unwrap();
        assert!(!m.matches("an ERROR occurred"));
        assert!(m.matches("an error occurred"));
    }

    #[test]
    fn text_spans_cover_all_occurrences() {
        let st = SearchState {
            query: "abc".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let m = compile(&st).unwrap().unwrap();
        let mut spans = Vec::new();
        m.spans("abcABCabc", &mut spans);
        assert_eq!(spans, vec![(0, 3), (6, 9)]);

        let m = compile(&state("abc")).unwrap().unwrap();
        m.spans("abcABCabc", &mut spans);
        assert_eq!(spans, vec![(0, 3), (3, 6), (6, 9)]);
    }

    #[test]
    fn regex_spans_and_invalid_pattern() {
        let st = SearchState {
            query: r"\d+".into(),
            regex: true,
            ..Default::default()
        };
        let m = compile(&st).unwrap().unwrap();
        let mut spans = Vec::new();
        m.spans("a1b22", &mut spans);
        assert_eq!(spans, vec![(1, 2), (3, 5)]);

        let bad = SearchState {
            query: "(".into(),
            regex: true,
            ..Default::default()
        };
        assert!(compile(&bad).is_err());
    }
}
