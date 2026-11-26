// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 代码/配置文件识别与逐行语法高亮(轻量实现,不引入额外解析依赖)
//!
//! 只做「一眼可读」级别的着色:注释、字符串、数字、关键字、键名。
//! 不处理跨行字符串/块注释、模板字符串插值等复杂语法。

use std::ops::Range;

use crate::i18n;

use gpui_kit::base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
};
use gpui_kit::base::text::CodeBlock;
use gpui_kit::{Context, HighlightStyle, Hsla, SharedString, Window};
use ropey::Rope;

/// 代码视图的文件大小上限:超过此值按日志文件打开
///
/// 编辑器(带行号/语法着色)面向中小文件;超大文件交给日志视图的虚拟滚动,
/// 避免整份文本进入编辑器和着色扫描。
pub const MAX_CODE_BYTES: u64 = 2 * 1024 * 1024;

/// Markdown 预览的文本上限:超过后只读态回落到源码着色,避免整篇重排卡顿
pub const MAX_PREVIEW_BYTES: u64 = 512 * 1024;

/// 支持高亮的语言(按扩展名识别)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Json,
    Rust,
    Python,
    Js,
    Ts,
    Go,
    C,
    Cpp,
    Java,
    Sh,
    Toml,
    Yaml,
    Markdown,
}

impl Lang {
    /// 工具栏展示用的名称
    pub fn label(self) -> &'static str {
        match self {
            Lang::Json => "JSON",
            Lang::Rust => "Rust",
            Lang::Python => "Python",
            Lang::Js => "JavaScript",
            Lang::Ts => "TypeScript",
            Lang::Go => "Go",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::Java => "Java",
            Lang::Sh => "Shell",
            Lang::Toml => "TOML",
            Lang::Yaml => "YAML",
            Lang::Markdown => "Markdown",
        }
    }
}

/// 高亮类别(渲染时映射为具体颜色)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Comment,
    Str,
    Number,
    Keyword,
    Key,
}

/// 按路径扩展名识别语言;未知/无扩展名返回 None(走日志视图)
pub fn lang_for_path(path: &str) -> Option<Lang> {
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "json" => Lang::Json,
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "js" | "jsx" | "mjs" | "cjs" => Lang::Js,
        "ts" | "tsx" => Lang::Ts,
        "go" => Lang::Go,
        "c" | "h" => Lang::C,
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Lang::Cpp,
        "java" => Lang::Java,
        "sh" | "bash" | "zsh" => Lang::Sh,
        "toml" => Lang::Toml,
        "yml" | "yaml" => Lang::Yaml,
        "md" | "markdown" => Lang::Markdown,
        _ => return None,
    })
}

fn keywords(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "fn", "let", "mut", "pub", "use", "mod", "struct", "enum", "impl", "trait", "for",
            "while", "loop", "if", "else", "match", "return", "self", "Self", "crate", "super",
            "const", "static", "async", "await", "move", "ref", "type", "where", "dyn", "unsafe",
            "extern", "as", "in", "break", "continue", "true", "false",
        ],
        Lang::Python => &[
            "def", "class", "return", "if", "elif", "else", "for", "while", "import", "from", "as",
            "with", "try", "except", "finally", "raise", "pass", "lambda", "None", "True", "False",
            "and", "or", "not", "in", "is", "async", "await", "yield", "global", "nonlocal",
        ],
        Lang::Js | Lang::Ts => &[
            "function",
            "const",
            "let",
            "var",
            "return",
            "if",
            "else",
            "for",
            "while",
            "do",
            "switch",
            "case",
            "break",
            "continue",
            "class",
            "extends",
            "new",
            "this",
            "import",
            "export",
            "from",
            "default",
            "async",
            "await",
            "try",
            "catch",
            "finally",
            "throw",
            "typeof",
            "instanceof",
            "interface",
            "type",
            "enum",
            "implements",
            "public",
            "private",
            "readonly",
            "null",
            "undefined",
            "true",
            "false",
        ],
        Lang::Go => &[
            "func",
            "package",
            "import",
            "return",
            "if",
            "else",
            "for",
            "range",
            "switch",
            "case",
            "default",
            "break",
            "continue",
            "type",
            "struct",
            "interface",
            "map",
            "chan",
            "go",
            "defer",
            "select",
            "var",
            "const",
            "nil",
            "true",
            "false",
        ],
        Lang::C | Lang::Cpp | Lang::Java => &[
            "int",
            "char",
            "float",
            "double",
            "void",
            "long",
            "short",
            "unsigned",
            "signed",
            "struct",
            "union",
            "enum",
            "typedef",
            "const",
            "static",
            "extern",
            "return",
            "if",
            "else",
            "for",
            "while",
            "do",
            "switch",
            "case",
            "break",
            "continue",
            "class",
            "public",
            "private",
            "protected",
            "virtual",
            "override",
            "new",
            "delete",
            "namespace",
            "using",
            "template",
            "typename",
            "static_cast",
            "nullptr",
            "true",
            "false",
            "import",
            "package",
            "final",
            "throws",
        ],
        Lang::Sh => &[
            "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until",
            "case", "esac", "function", "return", "export", "local", "readonly", "local", "echo",
            "cd", "set", "shift", "exit", "trap",
        ],
        Lang::Toml | Lang::Yaml => &["true", "false", "null", "yes", "no", "on", "off"],
        Lang::Json => &["true", "false", "null"],
        Lang::Markdown => &[],
    }
}

/// 行注释标记(None = 该语言不用行注释)
fn line_comment(lang: Lang) -> Option<&'static str> {
    Some(match lang {
        Lang::Rust | Lang::Js | Lang::Ts | Lang::Go | Lang::C | Lang::Cpp | Lang::Java => "//",
        Lang::Python | Lang::Sh | Lang::Toml | Lang::Yaml => "#",
        Lang::Json => return None,
        Lang::Markdown => return None,
    })
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// 字符串字面量的结束位置(含引号);未闭合时到行尾
fn scan_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn next_non_space(bytes: &[u8], mut i: usize) -> Option<u8> {
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => i += 1,
            c => return Some(c),
        }
    }
    None
}

/// 单行高亮:返回按起点升序、互不重叠的 (start, end, 类别);偏移为字节偏移
pub fn line_spans(lang: Lang, line: &str) -> Vec<(usize, usize, Class)> {
    let bytes = line.as_bytes();
    let mut out: Vec<(usize, usize, Class)> = Vec::new();
    let kw = keywords(lang);
    let comment = line_comment(lang);
    let markdown_heading = lang == Lang::Markdown && line.trim_start().starts_with('#');

    // Markdown:标题整行按关键字处理,行内 `code` 按字符串
    if markdown_heading {
        out.push((0, line.len(), Class::Keyword));
        return out;
    }

    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(m) = comment {
            if line[i..].starts_with(m) {
                out.push((i, bytes.len(), Class::Comment));
                break;
            }
        }
        if c == b'"'
            || c == b'\''
            || (matches!(lang, Lang::Js | Lang::Ts | Lang::Markdown) && c == b'`')
        {
            let end = scan_string(bytes, i);
            let is_key = matches!(lang, Lang::Json | Lang::Toml | Lang::Yaml)
                && matches!(next_non_space(bytes, end), Some(b':') | Some(b'='));
            out.push((i, end, if is_key { Class::Key } else { Class::Str }));
            i = end;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric()
                    || matches!(bytes[j], b'.' | b'_' | b'x' | b'X'))
            {
                j += 1;
            }
            out.push((i, j, Class::Number));
            i = j;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i + 1;
            while j < bytes.len() && is_ident(bytes[j]) {
                j += 1;
            }
            let word = &line[i..j];
            if kw.contains(&word) {
                out.push((i, j, Class::Keyword));
            } else if matches!(lang, Lang::Toml | Lang::Yaml)
                && matches!(next_non_space(bytes, j), Some(b':') | Some(b'='))
                && bytes.get(j) != Some(&b' ')
            {
                // TOML/YAML 的键名(如 `name: x` / `name = x`)
                out.push((i, j, Class::Key));
            }
            i = j;
            continue;
        }
        // Markdown 行内代码
        if lang == Lang::Markdown && c == b'`' {
            let end = scan_string(bytes, i);
            out.push((i, end, Class::Str));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// 围栏代码块上的语言标签 → 内置语言(不认识的返回 None,按纯文本处理)
pub fn lang_for_tag(tag: &str) -> Option<Lang> {
    let t = tag.trim().to_ascii_lowercase();
    Some(match t.split(['{', ' ', ',']).next().unwrap_or("") {
        "rust" | "rs" => Lang::Rust,
        "json" | "jsonc" => Lang::Json,
        "python" | "py" => Lang::Python,
        "javascript" | "js" | "jsx" => Lang::Js,
        "typescript" | "ts" | "tsx" => Lang::Ts,
        "go" | "golang" => Lang::Go,
        "c" | "h" => Lang::C,
        "cpp" | "c++" | "cc" | "hpp" => Lang::Cpp,
        "java" => Lang::Java,
        "sh" | "bash" | "zsh" | "shell" | "console" => Lang::Sh,
        "toml" => Lang::Toml,
        "yaml" | "yml" => Lang::Yaml,
        "markdown" | "md" => Lang::Markdown,
        _ => return None,
    })
}

/// 整段文本(多行)的高亮区间:供 Markdown 代码块着色使用
pub fn highlight_text(
    lang: Lang,
    text: &str,
    palette: CodePalette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::new();
    let mut base = 0usize;
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        for (a, b, class) in line_spans(lang, content) {
            if b > a {
                out.push((base + a..base + b, palette.style(class)));
            }
        }
        base += line.len();
    }
    out
}

/// `%20` 这类 URL 转义解码;非法转义原样保留
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 图片语法:alt + 目标(可带标题)
fn image_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"!\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+"[^"]*")?\s*\)"#).expect("图片正则")
    })
}

/// 把指向「不存在的本地文件」的图片改写为一行提示(含源码路径)
///
/// 组件不提供图片错误态渲染,这样至少能让人看到缺的是哪张图;
/// 围栏代码块内的内容保持原样,http(s)/data 图片不动。
pub fn stub_missing_images(md: &str, base_dir: &std::path::Path) -> String {
    let re = image_re();
    let mut out = String::with_capacity(md.len());
    let mut in_fence = false;
    for line in md.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence || !line.contains("![") {
            out.push_str(line);
            continue;
        }
        let replaced = re.replace_all(line, |caps: &regex::Captures| {
            let whole = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let alt = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            if raw.starts_with("http://")
                || raw.starts_with("https://")
                || raw.starts_with("data:")
                || raw.is_empty()
            {
                return whole.to_string();
            }
            let p = raw.strip_prefix("file://").unwrap_or(raw);
            let p = p.split(['#', '?']).next().unwrap_or(p);
            let decoded = percent_decode(p);
            let path = std::path::Path::new(&decoded);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base_dir.join(path)
            };
            if full.is_file() {
                whole.to_string()
            } else if alt.is_empty() {
                i18n::tf("⚠ 图片缺失:{decoded}", &[("decoded", decoded)])
            } else {
                i18n::tf(
                    "⚠ 图片缺失:{decoded}({alt})",
                    &[("decoded", decoded), ("alt", alt.to_string())],
                )
            }
        });
        out.push_str(&replaced);
    }
    out
}

/// 代码块着色使用的全局配色:主题切换时由渲染路径刷新
static HL_PALETTE: std::sync::OnceLock<std::sync::RwLock<CodePalette>> = std::sync::OnceLock::new();

fn hl_palette() -> &'static std::sync::RwLock<CodePalette> {
    HL_PALETTE.get_or_init(|| {
        std::sync::RwLock::new(CodePalette::from_theme(crate::theme::palette(false)))
    })
}

/// 主题变化时更新代码块着色配色(预览每帧调用,开销可忽略)
pub fn set_highlight_palette(palette: CodePalette) {
    if let Ok(mut guard) = hl_palette().write() {
        *guard = palette;
    }
}

/// 安装全局 Text View 默认项:围栏代码块按语言着色
///
/// base 只提供全局注入点(`TextViewDefaults`),组件层不再单独挂 highlighter。
pub fn install_code_block_highlighter(cx: &mut gpui_kit::App) {
    let _ = HL_PALETTE.set(std::sync::RwLock::new(CodePalette::from_theme(
        crate::theme::palette(false),
    )));
    gpui_kit::base::TextViewDefaults::new()
        .with_code_block_highlighter(|cb: &CodeBlock| {
            let Some(lang) = cb.lang().as_deref().and_then(lang_for_tag) else {
                return Vec::new();
            };
            let palette = hl_palette()
                .read()
                .map(|p| *p)
                .unwrap_or_else(|e| *e.into_inner());
            highlight_text(lang, &cb.code(), palette)
        })
        .install(cx);
}

/// 无规则的级别引擎:代码文件不参与级别分析
pub fn no_level_engine() -> crate::level::Engine {
    crate::level::Engine::compile_selected(&[], &[])
}

/// 编辑器着色配色(取自当前主题;主题切换时重建编辑器即可换色)
#[derive(Clone, Copy)]
pub struct CodePalette {
    pub keyword: Hsla,
    pub string: Hsla,
    pub number: Hsla,
    pub key: Hsla,
    pub comment: Hsla,
}

impl CodePalette {
    /// 从主题配色表取一套代码着色颜色
    pub fn from_theme(pal: &crate::theme::Palette) -> Self {
        Self {
            keyword: crate::theme::hex(pal.code_kw),
            string: crate::theme::hex(pal.code_str),
            number: crate::theme::hex(pal.code_num),
            key: crate::theme::hex(pal.code_key),
            comment: crate::theme::hex(pal.code_comment),
        }
    }

    fn color(self, class: Class) -> Hsla {
        match class {
            Class::Comment => self.comment,
            Class::Str => self.string,
            Class::Number => self.number,
            Class::Keyword => self.keyword,
            Class::Key => self.key,
        }
    }

    pub(crate) fn style(self, class: Class) -> HighlightStyle {
        HighlightStyle {
            color: Some(self.color(class)),
            ..Default::default()
        }
    }
}

/// 编辑器着色适配:把 [`line_spans`] 的逐行结果翻译成编辑器要求的样式区间
///
/// 编辑器(base 的 code editor 模式)只管编辑与排版,语言着色通过本适配注入。
/// 文本在每次编辑后由 [`InputHighlighter::update`] 缓存,渲染时按可见区间取用。
pub struct CodeHighlighter {
    lang: Lang,
    palette: CodePalette,
    text: String,
    /// 每行起始字节偏移;行尾 = 下一个偏移减 1(跳掉 `\n`)
    line_starts: Vec<usize>,
}

impl CodeHighlighter {
    pub fn new(lang: Lang, palette: CodePalette) -> Self {
        Self {
            lang,
            palette,
            text: String::new(),
            line_starts: vec![0],
        }
    }

    /// 替换全文并重建行索引
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.line_starts.clear();
        self.line_starts.push(0);
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                self.line_starts.push(i + 1);
            }
        }
    }

    /// 当前全文长度(字节);仅测试用于校验样式区间覆盖
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// 当前全文;仅测试使用
    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 计算 `range` 内完整覆盖的样式区间(未着色的缺口用默认样式补齐)
    ///
    /// 编辑器要求返回按起点升序、互不重叠且铺满整个区间的样式段。
    pub fn styles_in(&self, range: Range<usize>) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let len = self.text.len();
        let mut start = range.start.min(len);
        let mut end = range.end.min(len);
        while start < end && !self.text.is_char_boundary(start) {
            start += 1;
        }
        while end > start && !self.text.is_char_boundary(end) {
            end -= 1;
        }
        if start >= end {
            return out;
        }

        let mut cursor = start;
        let mut li = self
            .line_starts
            .partition_point(|&s| s <= start)
            .saturating_sub(1);
        while li < self.line_starts.len() {
            let ls = self.line_starts[li];
            if ls >= end {
                break;
            }
            let le = self
                .line_starts
                .get(li + 1)
                .map(|&n| n - 1)
                .unwrap_or(len)
                .min(len);
            if ls < le && le > start {
                let line = &self.text[ls..le];
                for (a, b, class) in line_spans(self.lang, line) {
                    let a = (ls + a).max(start);
                    let b = (ls + b).min(le).min(end);
                    if a >= b {
                        continue;
                    }
                    let a = a.max(cursor);
                    if a > cursor {
                        out.push((cursor..a, HighlightStyle::default()));
                    }
                    if b > a {
                        out.push((a..b, self.palette.style(class)));
                        cursor = b;
                    }
                }
            }
            li += 1;
        }
        if cursor < end {
            out.push((cursor..end, HighlightStyle::default()));
        }
        out
    }
}

impl InputHighlighter for CodeHighlighter {
    fn language(&self) -> SharedString {
        self.lang.label().into()
    }

    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.set_text(&text.to_string());
    }

    fn styles(
        &self,
        range: &Range<usize>,
        _resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.styles_in(range.clone())
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_language_by_extension() {
        assert_eq!(lang_for_path("a/b/main.rs"), Some(Lang::Rust));
        assert_eq!(lang_for_path("config.JSON"), Some(Lang::Json));
        assert_eq!(lang_for_path("x.yaml"), Some(Lang::Yaml));
        assert_eq!(lang_for_path("app.log"), None);
        assert_eq!(lang_for_path("Makefile"), None);
    }

    #[test]
    fn json_line_highlights_key_string_number() {
        let spans = line_spans(Lang::Json, r#"  "name": "logview", "n": 42, "ok": true"#);
        assert!(spans.contains(&(2, 8, Class::Key)), "{spans:?}");
        assert!(spans.contains(&(10, 19, Class::Str)), "{spans:?}");
        assert!(spans.contains(&(26, 28, Class::Number)), "{spans:?}");
        assert!(spans.iter().any(|s| s.2 == Class::Keyword), "{spans:?}");
    }

    #[test]
    fn rust_line_highlights_keyword_fn_and_comment() {
        let spans = line_spans(Lang::Rust, "pub fn main() { // 入口");
        assert!(spans.contains(&(0, 3, Class::Keyword)), "{spans:?}");
        assert!(spans.contains(&(4, 6, Class::Keyword)), "{spans:?}");
        assert_eq!(spans.last().map(|s| s.2), Some(Class::Comment));
    }

    #[test]
    fn python_comment_and_string() {
        let spans = line_spans(Lang::Python, "def f(x):  # 注释\n");
        assert_eq!(spans.first().map(|s| s.2), Some(Class::Keyword));
        assert!(spans.iter().any(|s| s.2 == Class::Comment), "{spans:?}");
    }

    #[test]
    fn spans_are_ordered_and_within_line() {
        for (lang, line) in [
            (Lang::Rust, r#"let s = "a\"b"; // x"#),
            (Lang::Json, r#"{"k": [1, 2, true]}"#),
            (Lang::Yaml, "key: value # c"),
        ] {
            let spans = line_spans(lang, line);
            let mut last = 0;
            for (s, e, _) in spans {
                assert!(s >= last && e > s && e <= line.len(), "{lang:?} {line:?}");
                last = e;
            }
        }
    }

    fn test_palette() -> CodePalette {
        CodePalette::from_theme(crate::theme::palette(true))
    }

    #[test]
    fn highlighter_styles_cover_the_whole_range() {
        let mut hl = CodeHighlighter::new(Lang::Rust, test_palette());
        hl.set_text("fn main() {\n    let x = 1; // c\n}\n");
        let all = hl.styles_in(0..hl.len());
        let mut pos = 0usize;
        for (r, _) in &all {
            assert_eq!(r.start, pos, "{all:?}");
            assert!(r.end > r.start, "{all:?}");
            pos = r.end;
        }
        assert_eq!(pos, hl.len());
        // 行首 `fn` 是关键字色
        assert_eq!(
            all.first().and_then(|(_, s)| s.color),
            Some(hl.palette.keyword)
        );
    }

    #[test]
    fn highlighter_clips_to_requested_range() {
        let mut hl = CodeHighlighter::new(Lang::Json, test_palette());
        hl.set_text("{\n  \"name\": \"logview\",\n  \"n\": 42\n}\n");
        let seg = hl.styles_in(4..18);
        assert_eq!(seg.first().map(|(r, _)| r.start), Some(4), "{seg:?}");
        assert_eq!(seg.last().map(|(r, _)| r.end), Some(18), "{seg:?}");
        let mut pos = 4usize;
        for (r, _) in &seg {
            assert_eq!(r.start, pos, "{seg:?}");
            pos = r.end;
        }
    }

    #[test]
    fn highlighter_handles_multibyte_text() {
        let mut hl = CodeHighlighter::new(Lang::Rust, test_palette());
        hl.set_text("fn main() { // 中文注释 🚀\n}\n");
        let all = hl.styles_in(0..hl.len());
        assert_eq!(all.last().map(|(r, _)| r.end), Some(hl.len()));
        // 任意区间不 panic 且落在字符边界
        for start in 0..hl.len() {
            let seg = hl.styles_in(start..hl.len());
            assert!(hl
                .text()
                .is_char_boundary(seg.first().map(|(r, _)| r.start).unwrap_or(hl.len())));
        }
    }

    #[test]
    fn percent_decoding_handles_spaces_and_utf8() {
        assert_eq!(percent_decode("a%20b.png"), "a b.png");
        assert_eq!(percent_decode("%E4%B8%AD%20x"), "中 x");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn fence_tags_map_to_languages() {
        assert_eq!(lang_for_tag("rust"), Some(Lang::Rust));
        assert_eq!(lang_for_tag("JS"), Some(Lang::Js));
        assert_eq!(lang_for_tag("bash"), Some(Lang::Sh));
        assert_eq!(lang_for_tag("no-such-lang"), None);
    }

    #[test]
    fn missing_local_images_become_hint_text() {
        let dir = std::env::temp_dir();
        let md = "![图](./no-such-file-xyz.png)\n![](https://example.com/a.png)\n```\n![x](./no-such.png)\n```\n";
        let out = stub_missing_images(md, &dir);
        assert!(
            out.contains("⚠ 图片缺失:./no-such-file-xyz.png(图)"),
            "{out}"
        );
        assert!(out.contains("https://example.com/a.png"), "{out}");
        assert_eq!(out.matches('⚠').count(), 1, "围栏内不应改写:{out}");
    }

    #[test]
    fn code_block_highlights_use_absolute_offsets() {
        let pal = test_palette();
        let spans = highlight_text(Lang::Rust, "let a = 1;\nfn b() {}\n", pal);
        assert!(
            spans
                .iter()
                .any(|(r, st)| r.start >= 11 && st.color == Some(pal.keyword)),
            "{spans:?}"
        );
    }
}
