// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 检索:关键字(文本)/正则 × 大小写开关 × 高亮/过滤两种模式

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Highlight, // 全部行可见,匹配处加高亮,支持跳转
    Filter,    // 只显示匹配行
}

#[derive(Clone)]
pub struct SearchState {
    pub query: String,
    pub mode: Mode,
    pub regex: bool,
    pub case_sensitive: bool,
    pub rev: u64,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: Mode::Highlight,
            regex: false,
            case_sensitive: false,
            rev: 0,
        }
    }
}

impl SearchState {
    pub fn active(&self) -> bool {
        !self.query.is_empty()
    }
    fn bump(&mut self) {
        self.rev += 1;
    }
    /// TextEdit 直接改写 query 后调用,标记需要重编译
    pub fn mark_changed(&mut self) {
        self.bump();
    }
    pub fn set_query(&mut self, q: String) {
        if self.query != q {
            self.query = q;
            self.bump();
        }
    }
    pub fn set_mode(&mut self, m: Mode) {
        if self.mode != m {
            self.mode = m;
            self.bump();
        }
    }
    pub fn toggle_regex(&mut self) {
        self.regex = !self.regex;
        self.bump();
    }
    pub fn toggle_case(&mut self) {
        self.case_sensitive = !self.case_sensitive;
        self.bump();
    }
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

        let mut st = state("error");
        st.case_sensitive = true;
        let m = compile(&st).unwrap().unwrap();
        assert!(!m.matches("an ERROR occurred"));
        assert!(m.matches("an error occurred"));
    }

    #[test]
    fn text_spans_cover_all_occurrences() {
        let mut st = state("abc");
        st.case_sensitive = true;
        let m = compile(&st).unwrap().unwrap();
        let mut spans = Vec::new();
        m.spans("abcABCabc", &mut spans);
        assert_eq!(spans, vec![(0, 3), (6, 9)]);

        st.case_sensitive = false;
        let m = compile(&st).unwrap().unwrap();
        m.spans("abcABCabc", &mut spans);
        assert_eq!(spans, vec![(0, 3), (3, 6), (6, 9)]);
    }

    #[test]
    fn regex_spans_and_invalid_pattern() {
        let mut st = state(r"\d+");
        st.regex = true;
        let m = compile(&st).unwrap().unwrap();
        let mut spans = Vec::new();
        m.spans("a1b22", &mut spans);
        assert_eq!(spans, vec![(1, 2), (3, 5)]);

        st.query = "(".into();
        assert!(compile(&st).is_err());
    }

    #[test]
    fn revision_only_changes_on_edit() {
        let mut st = SearchState::default();
        st.set_query("a".into());
        let rev = st.rev;
        st.set_query("a".into());
        assert_eq!(st.rev, rev);

        st.toggle_regex();
        assert!(st.rev > rev);
        let rev = st.rev;
        st.mark_changed();
        assert!(st.rev > rev);

        st.set_mode(Mode::Filter);
        assert!(st.rev > rev);
    }
}
