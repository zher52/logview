// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! GPUI 版主视图:标签栏 + 工具栏 + 日志区(虚拟滚动)+ 状态栏
//!
//! 视图管线:隐藏级别/过滤检索 → `Tab.view`(可见行索引);检索高亮 → 按行计算区间。
//! 追加时增量维护 view/matched;检索与级别变化时全量重建。

use std::rc::Rc;
use std::time::{Duration, Instant};

use std::cell::RefCell;
use std::collections::BTreeMap;

use gpui_kit::base::input::{Editor, EditorState, InputHighlighter, InputHighlighterFactory};
use gpui_kit::base::ElementExt as _;
use gpui_kit::base::SelectableText;
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{
    Input, InputContentType, InputEvent, InputState, Textarea, TextareaState,
};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::text::TextView;
use gpui_kit::component::Colorize;
use gpui_kit::component::{
    v_virtual_list, ActiveTheme as _, Sizable as _, TitleBar, VirtualListScrollHandle,
};
use gpui_kit::{
    div, point, px, svg, AppContext as _, Bounds, ClickEvent, Context, CursorStyle, Div, ElementId,
    Entity, ExternalPaths, FocusHandle, Focusable as _, HighlightStyle, Hsla, ImageSource,
    InteractiveElement as _, IntoElement, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Render, ScrollStrategy, SharedString, SharedUri, Size, Stateful,
    StatefulInteractiveElement as _, Styled as _, TextAlign, TextRun, UnderlineStyle, Window,
};
use gpui_kit::{AnyElement, ClipboardItem, PathPromptOptions};

use crate::command::{self, CommandPlan, CommandPreset, PresetField};
use crate::i18n;
use crate::level::{level_names, Engine, LevelConfig, LEVEL_COUNT, SEP};
use crate::lines::LineStore;
use crate::links::{self, LinkKind};
use crate::search::{self, Matcher, SearchState};
use crate::secrets::{self, Secret};
use crate::source::{ExecTarget, SourceParams, SshAuth};
use crate::store::{self, Recent, Settings};
use crate::tail;
use crate::tail::{Cmd, Msg};
use crate::theme::{self, Palette};

/// 设置页分类数(外观 / 日志 / 分析配置 / 连接 / 快捷键 / 关于)
/// 新增分类时:同步更新 page_titles、page_descs、侧栏导航与 groups_all 的页码
const SETTINGS_PAGE_COUNT: usize = 6;

/// 项目坐标:发布仓库、「关于」页链接与检查更新的唯一来源
const REPO_URL: &str = "https://github.com/zher52/logview";
const RELEASES_API: &str = "https://api.github.com/repos/zher52/logview/releases/latest";

/// 远端最新发布(检查更新的结果)
#[derive(Clone)]
struct Release {
    tag: String,
    url: String,
}

/// 「关于」页的更新检查状态
#[derive(Clone)]
enum UpdateState {
    /// 尚未检查
    Idle,
    /// 请求进行中:结果由 poll 从通道取回
    Checking,
    /// 已是最新(附远端标签,便于排查)
    Latest(String),
    /// 有更新
    Available(Release),
    /// 检查失败(网络、限流、仓库未公开等)
    Failed(String),
}

/// 单个日志标签:worker 通道 + 行存储 + 检索/过滤状态 + 视图管线
pub struct Tab {
    id: u64,
    /// 日志源/文件标签的显示名
    title: String,
    /// 固定语义标题（新标签页、设置页）；None 表示使用 `title`
    title_id: Option<&'static str>,
    /// 日志源参数;空白标签页(尚未选择来源)为 None
    params: Option<SourceParams>,
    cmd_tx: crossbeam_channel::Sender<Cmd>,
    msg_rx: crossbeam_channel::Receiver<Msg>,
    store: LineStore,
    engine: Engine,
    // 检索
    query: String,
    regex: bool,
    case_sensitive: bool,
    filter_mode: bool,
    matcher: Option<Matcher>,
    match_err: Option<String>,
    matched: Vec<u32>,
    /// 检索结果跳转游标(▲/▼ 循环定位)
    match_cursor: usize,
    /// 行选中:锚点与焦点(真实行号,None = 未选中)
    sel_anchor: Option<usize>,
    sel_focus: Option<usize>,
    // 分析配置:本标签页启用的配置 id(设置里的 enabled 只是新标签页的默认值)
    enabled_ids: Vec<String>,
    /// 已经见过的配置 id(用于设置里新增配置时补默认值)
    known_ids: Vec<String>,
    /// 自动换行(标签页独立,初始值来自设置)
    wrap: bool,
    /// 自动换行时的行高缓存(与 store.lines 一一对应)
    heights: Vec<f32>,
    /// heights 对应的可用文本宽度
    heights_w: f32,
    // 级别筛选与视图
    hidden: [bool; LEVEL_COUNT],
    view: Vec<u32>,
    /// 视图重建计数(行高缓存失效用)
    view_gen: u64,
    // 跟随/加载状态
    follow: bool,
    paused: bool,
    pending_new: usize,
    loading: Option<(u64, u64)>,
    no_follow: bool,
    err: Option<String>,
    /// 跳尾 banner:(起始字节, 文件大小);Some = 显示「已跳至末尾」
    tail_banner: Option<(u64, u64)>,
    /// SeekEnd 已发出,等 Replace 回来后再显示 banner
    pending_tail_banner: bool,
    /// 配置变更后的分帧重判定起点(Some = 进行中)
    relevel: Option<usize>,
    /// 空白标签页:内容为「打开日志源」欢迎页
    welcome: bool,
    /// 设置标签页:内容为设置面板
    settings: bool,
    /// 代码文件标签页(Some = 只读语法高亮视图,可切换编辑)
    code: Option<CodeDoc>,
    /// 命令源(单向流):大小/历史来自输出缓存(spool),状态栏显示已接收字节
    stream: bool,
    /// 已加载全部后待滚到顶部(等 Replace 回来再滚,避免被旧视图高度夹住)
    pending_scroll_top: bool,
    /// 命令退出提示(文案, 是否异常);有新输出时清除
    exit_note: Option<(String, bool)>,
}

/// 代码文件标签页的状态
pub struct CodeDoc {
    /// 磁盘路径(保存写回此处)
    path: std::path::PathBuf,
    /// 识别出的语言(None = 纯文本)
    lang: Option<crate::code::Lang>,
    /// 当前文本(进入编辑的初始值,保存后同步)
    text: String,
    /// 代码编辑器(首次进入编辑时创建,带行号与语法着色)
    editor: Option<Entity<EditorState>>,
    /// 当前是否处于编辑态
    editing: bool,
}

impl Tab {
    /// 新建标签页时的配置启用集合(快照设置里的全局默认值)
    fn default_enabled(settings: &Settings) -> Vec<String> {
        settings
            .configs
            .iter()
            .filter(|c| c.enabled)
            .map(|c| c.id.clone())
            .collect()
    }

    fn all_config_ids(settings: &Settings) -> Vec<String> {
        settings.configs.iter().map(|c| c.id.clone()).collect()
    }

    /// 命令源按条件需要启用的级别配置 id(预设声明 + argv 含 logcat 的启发式)
    fn command_level_configs(settings: &Settings, params: &SourceParams) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let SourceParams::Command { argv, preset, .. } = params else {
            return out;
        };
        // 本机自定义命令已按引号拆成 argv;SSH 自定义命令是整条原文,再按空白拆一层
        let is_logcat = argv
            .iter()
            .any(|a| a == "logcat" || a.split_whitespace().any(|w| w == "logcat"))
            || preset
                .as_deref()
                .is_some_and(|id| id.starts_with("adb-logcat"));
        if is_logcat {
            out.push("builtin-android-logcat".to_string());
        }
        if let Some(id) = preset
            .as_deref()
            .and_then(|id| command::find(&settings.command_presets, id))
            .and_then(|p| p.level_config_id.clone())
        {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// 标签条显示名:固定语义标题每次取词,文件/日志源标题原样显示
    fn display_title(&self) -> String {
        self.title_id
            .map(|id| i18n::t(id).to_string())
            .unwrap_or_else(|| self.title.clone())
    }

    fn open(settings: &Settings, params: SourceParams) -> Tab {
        let plan = command_plan(settings, &params);
        let (tx, rx) = tail::spawn(
            params.clone(),
            plan,
            settings.poll_ms,
            settings.initial_mb << 20,
            settings.tail_mb << 20,
        );
        let title = params.label();
        let mut enabled_ids = Self::default_enabled(settings);
        // 命令源按条件启用对应级别配置:
        //  1) 预设声明的格式(如 Android logcat);
        //  2) 自定义/重开命令的 argv 含 logcat 时也启用,避免必须选预设才能正确解析。
        for id in Self::command_level_configs(settings, &params) {
            if !enabled_ids.iter().any(|x| x == &id) {
                enabled_ids.push(id);
            }
        }
        // logcat 规则是行首锚定的格式规则,必须优先于「通用单词」,否则正文里的
        // error/warn 会覆盖行首的真实级别。
        let prefer: &[&str] = if enabled_ids.iter().any(|x| x == "builtin-android-logcat") {
            &["builtin-android-logcat"]
        } else {
            &[]
        };
        let engine = Engine::compile_ids_preferring(&settings.configs, &enabled_ids, prefer);
        let stream = matches!(&params, SourceParams::Command { .. });
        Tab {
            id: 0,
            title,
            title_id: None,
            params: Some(params),
            cmd_tx: tx,
            msg_rx: rx,
            store: LineStore::new(settings.max_lines),
            engine,
            query: String::new(),
            regex: false,
            case_sensitive: false,
            filter_mode: false,
            matcher: None,
            match_err: None,
            matched: Vec::new(),
            match_cursor: 0,
            sel_anchor: None,
            sel_focus: None,
            enabled_ids,
            known_ids: Self::all_config_ids(settings),
            wrap: settings.wrap,
            heights: Vec::new(),
            heights_w: 0.0,
            hidden: [false; LEVEL_COUNT],
            view: Vec::new(),
            view_gen: 0,
            follow: true,
            paused: false,
            pending_new: 0,
            loading: None,
            no_follow: false,
            err: None,
            tail_banner: None,
            pending_tail_banner: false,
            relevel: None,
            welcome: false,
            settings: false,
            code: None,
            stream,
            pending_scroll_top: false,
            exit_note: None,
        }
    }

    /// 空白标签页:不启动 worker,内容为内嵌的「打开日志源」欢迎页
    fn blank(settings: &Settings) -> Tab {
        let (tx, _cmd_rx) = crossbeam_channel::unbounded::<Cmd>();
        let (_msg_tx, rx) = crossbeam_channel::unbounded::<Msg>();
        let enabled_ids = Self::default_enabled(settings);
        Tab {
            id: 0,
            title: String::new(),
            title_id: Some("tab.new"),
            params: None,
            cmd_tx: tx,
            msg_rx: rx,
            store: LineStore::new(settings.max_lines),
            engine: Engine::compile_ids(&settings.configs, &enabled_ids),
            query: String::new(),
            regex: false,
            case_sensitive: false,
            filter_mode: false,
            matcher: None,
            match_err: None,
            matched: Vec::new(),
            match_cursor: 0,
            sel_anchor: None,
            sel_focus: None,
            enabled_ids,
            known_ids: Self::all_config_ids(settings),
            wrap: settings.wrap,
            heights: Vec::new(),
            heights_w: 0.0,
            hidden: [false; LEVEL_COUNT],
            view: Vec::new(),
            view_gen: 0,
            follow: true,
            paused: false,
            pending_new: 0,
            loading: None,
            no_follow: false,
            err: None,
            tail_banner: None,
            pending_tail_banner: false,
            relevel: None,
            welcome: true,
            settings: false,
            code: None,
            stream: false,
            pending_scroll_top: false,
            exit_note: None,
        }
    }

    /// 设置标签页:与空白页相同的占位结构,内容由 AppView.settings_panel 渲染
    fn settings(settings: &Settings) -> Tab {
        let mut t = Tab::blank(settings);
        t.title_id = Some("settings.title");
        t.welcome = false;
        t.settings = true;
        t
    }

    /// 纯文本标签页(JSON prettify 等):无 worker,内容一次性灌入行存储
    fn text(settings: &Settings, title: String, text: &str) -> Tab {
        let mut t = Tab::blank(settings);
        t.welcome = false;
        t.title = title;
        t.title_id = None;
        t.follow = false;
        t.no_follow = true;
        t.store.push_bytes(text.as_bytes(), &t.engine, None);
        t.rebuild_view();
        t.rebuild_matched();
        t
    }

    /// 代码/文本文件标签页:不启动 worker,按语言逐行高亮(只读;可按需切换编辑)
    fn code_file(settings: &Settings, path: &str, lang: crate::code::Lang, text: &str) -> Tab {
        let mut t = Tab::blank(settings);
        t.welcome = false;
        t.title_id = None;
        t.title = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        t.follow = false;
        t.no_follow = true;
        // 代码文件不做级别分析,否则会被行内单词误染色
        t.store
            .push_bytes(text.as_bytes(), &crate::code::no_level_engine(), None);
        t.rebuild_view();
        t.rebuild_matched();
        t.code = Some(CodeDoc {
            path: std::path::PathBuf::from(path),
            lang: Some(lang),
            text: text.to_string(),
            editor: None,
            editing: false,
        });
        t
    }

    /// 可识别为代码/配置文件时构造代码标签;读不到文本(二进制/无权限)返回 None
    fn try_code_tab(settings: &Settings, path: &str) -> Option<Tab> {
        let lang = crate::code::lang_for_path(path)?;
        // 大文件按日志文件加载:编辑器和整份文本着色不适合超大文件
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > crate::code::MAX_CODE_BYTES {
                return None;
            }
        }
        let text = std::fs::read_to_string(path).ok()?;
        Some(Tab::code_file(settings, path, lang, &text))
    }

    fn compile_search(&mut self) {
        let st = SearchState {
            query: self.query.clone(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
        };
        match search::compile(&st) {
            Ok(m) => {
                self.matcher = m;
                self.match_err = None;
            }
            Err(e) => {
                self.matcher = None;
                self.match_err = Some(e);
            }
        }
        self.rebuild_matched();
        self.rebuild_view();
    }

    fn rebuild_matched(&mut self) {
        self.matched.clear();
        let Some(m) = &self.matcher else { return };
        for (i, line) in self.store.lines.iter().enumerate() {
            if m.matches(&line.text) {
                self.matched.push(i as u32);
            }
        }
    }

    fn rebuild_view(&mut self) {
        self.view.clear();
        self.view.reserve(self.store.lines.len());
        for (i, line) in self.store.lines.iter().enumerate() {
            if self.passes(line.level, &line.text) {
                self.view.push(i as u32);
            }
        }
        self.view_gen = self.view_gen.wrapping_add(1);
    }

    /// 行是否可见:分隔行恒显;隐藏级别与过滤模式叠加
    fn passes(&self, level: u8, text: &str) -> bool {
        if level == SEP {
            return true;
        }
        let l = level as usize;
        if l < LEVEL_COUNT && self.hidden[l] {
            return false;
        }
        if self.filter_mode {
            return self.matcher.as_ref().is_none_or(|m| m.matches(text));
        }
        true
    }

    /// 本标签页启用的配置数量
    /// 切换本标签页的一项分析配置(不影响设置里的全局默认值)
    fn toggle_config(&mut self, configs: &[LevelConfig], id: &str) {
        if let Some(pos) = self.enabled_ids.iter().position(|x| x == id) {
            self.enabled_ids.remove(pos);
        } else {
            self.enabled_ids.push(id.to_string());
        }
        self.recompile(configs);
        self.heights.clear();
        self.heights_w = 0.0;
        self.relevel = Some(0);
    }

    fn recompile(&mut self, configs: &[LevelConfig]) {
        self.engine = Engine::compile_ids(configs, &self.enabled_ids);
    }

    fn enabled_count(&self) -> usize {
        self.enabled_ids.len()
    }

    /// 设置里的配置列表变化后,按 id 对齐本标签页的启用集合
    fn reconcile_configs(&mut self, configs: &[LevelConfig]) {
        let ids: Vec<&str> = configs.iter().map(|c| c.id.as_str()).collect();
        self.enabled_ids.retain(|id| ids.contains(&id.as_str()));
        // 新增的配置使用设置里的默认启用值(已见过的配置保持标签页自己的选择)
        for c in configs {
            if !self.known_ids.iter().any(|k| k == &c.id) && c.enabled {
                self.enabled_ids.push(c.id.clone());
            }
        }
        self.known_ids = configs.iter().map(|c| c.id.clone()).collect();
        self.recompile(configs);
        self.relevel = Some(0);
    }

    fn set_wrap(&mut self, on: bool) {
        if self.wrap != on {
            self.wrap = on;
            self.heights.clear();
            self.heights_w = 0.0;
            self.view_gen = self.view_gen.wrapping_add(1);
        }
    }
}

// ---- 对话框状态 ----

/// 顶部弹层种类(手绘模态,不依赖组件弹窗)
enum Dialog {
    None,
    Ssh(SshForm),
    Url(UrlForm),
    Cmd(CmdForm),
    /// 未知 SSH 主机:首次连接时核对指纹(TOFU)
    HostKey(HostKeyForm),
}

/// SSH 连接配置弹窗状态
struct SshForm {
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    path: Entity<InputState>,
    /// true=密码,false=Agent
    auth_password: bool,
    /// 「记住连接」:凭据写入系统钥匙串,设置文件始终不存凭据
    remember: bool,
    /// 认证失败后重试的目标标签页 id(Some = 重新提交时替换该标签页内容)
    retry: Option<u64>,
    err: Option<String>,
    /// 0 = 打开文件,1 = 执行命令
    mode: usize,
    /// 命令模式的预设/字段
    cmd: CmdFields,
}

impl SshForm {
    fn new(window: &mut Window, cx: &mut Context<AppView>) -> SshForm {
        let mut mk = |ph: &'static str| cx.new(|cx| InputState::new(window, cx).placeholder(ph));
        let host = mk(i18n::t("ssh.host.placeholder"));
        let port = mk("22");
        let user = mk(i18n::t("ssh.username"));
        let password = mk(i18n::t("ssh.password.agent_placeholder"));
        let path = mk("/var/log/app.log");
        port.update(cx, |s, cx| s.set_value("22", window, cx));
        SshForm {
            host,
            port,
            user,
            password,
            path,
            auth_password: true,
            remember: false,
            retry: None,
            err: None,
            mode: 0,
            cmd: CmdFields::new(window, cx),
        }
    }
}

/// 命令表单的字段部分(本机命令弹窗与 SSH 命令模式共用)
#[derive(Clone)]
struct CmdFields {
    /// 0 = 自定义;n = 预设列表第 n-1 项
    preset: usize,
    /// 当前预设的字段输入框
    inputs: Vec<(PresetField, Entity<InputState>)>,
    /// 自定义命令行
    text: Entity<InputState>,
    /// 退出后自动重试
    restart: bool,
}

impl CmdFields {
    fn new(window: &mut Window, cx: &mut Context<AppView>) -> CmdFields {
        CmdFields {
            preset: 0,
            inputs: Vec::new(),
            text: cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t("command.example"))),
            restart: false,
        }
    }
}

/// 本机命令弹窗
struct CmdForm {
    fields: CmdFields,
    err: Option<String>,
}

/// 未知 SSH 主机:首次连接时核对指纹(TOFU)
struct HostKeyForm {
    tab: u64,
    info: crate::source::HostKeyInfo,
}

/// 按预设重建字段输入框(填默认值)
fn build_preset_inputs(
    presets: &[CommandPreset],
    local: bool,
    preset: usize,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> Vec<(PresetField, Entity<InputState>)> {
    let list = command::presets_for(presets, local);
    let Some(p) = preset.checked_sub(1).and_then(|i| list.get(i)).copied() else {
        return Vec::new();
    };
    p.fields
        .iter()
        .map(|f| {
            let ph = f.placeholder.clone();
            let e = cx.new(|cx| {
                let s = InputState::new(window, cx);
                if ph.is_empty() {
                    s
                } else {
                    s.placeholder(SharedString::from(i18n::td(&ph)))
                }
            });
            let d = f.default.clone();
            e.update(cx, |s, cx| s.set_value(d, window, cx));
            (f.clone(), e)
        })
        .collect()
}

/// 从参数回填命令表单(认证失败重试、凭据提示时用)
#[allow(clippy::too_many_arguments)]
fn fill_command_fields(
    presets: &[CommandPreset],
    local: bool,
    preset_id: &Option<String>,
    values: &BTreeMap<String, String>,
    argv: &[String],
    raw: bool,
    fields: &mut CmdFields,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    let list = command::presets_for(presets, local);
    let idx = preset_id
        .as_ref()
        .and_then(|id| list.iter().position(|p| &p.id == id));
    fields.preset = idx.map(|i| i + 1).unwrap_or(0);
    fields.inputs = build_preset_inputs(presets, local, fields.preset, window, cx);
    if idx.is_some() {
        for (f, e) in &fields.inputs {
            if let Some(v) = values.get(&f.key) {
                let v = v.clone();
                e.update(cx, |s, cx| s.set_value(v, window, cx));
            }
        }
    } else {
        let text = if raw {
            argv.join(" ")
        } else {
            command::display(argv)
        };
        fields
            .text
            .update(cx, |s, cx| s.set_value(text, window, cx));
    }
}

/// URL(HTTP)打开弹窗状态:网址 + 可选 Basic 认证
struct UrlForm {
    url: Entity<InputState>,
    /// 0 无 / 1 Basic / 2 Bearer / 3 API Key / 4 自定义头
    auth: usize,
    user: Entity<InputState>,
    password: Entity<InputState>,
    token: Entity<InputState>,
    header_name: Entity<InputState>,
    header_value: Entity<InputState>,
    /// 「记住凭据」:凭据写入系统钥匙串
    remember: bool,
    /// 认证失败后重试的目标标签页 id(Some = 重新提交时替换该标签页内容)
    retry: Option<u64>,
    err: Option<String>,
}

impl UrlForm {
    fn new(window: &mut Window, cx: &mut Context<AppView>) -> UrlForm {
        let mut mk = |ph: &'static str| cx.new(|cx| InputState::new(window, cx).placeholder(ph));
        UrlForm {
            url: mk("https://example.com/app.log"),
            auth: 0,
            user: mk(i18n::t("ssh.username")),
            password: mk(i18n::t("ssh.password")),
            token: mk("Bearer token"),
            header_name: mk("X-API-Key"),
            header_value: mk(i18n::t("http.header.value")),
            remember: false,
            retry: None,
            err: None,
        }
    }
}

/// 设置面板状态:改动直接写入 `AppView.settings` 并立即生效(VS Code 式,无保存按钮)
struct SettingsDialog {
    /// 当前分类:0 外观与显示 1 日志与性能 2 分析配置 3 连接
    tab: usize,
    /// 设置搜索框:跨分类过滤;为空时按分类浏览
    search: Entity<InputState>,
    sel_config: usize,
    /// 规则编辑输入框:(正则代码框, 标志),与选中配置的 rules 对齐
    rule_inputs: Vec<(Entity<TextareaState>, Entity<InputState>)>,
    /// 正则着色装饰基于哪个主题生成(None = 尚未生成,主题切换后据此重算)
    deco_light: Option<bool>,
    // 数值参数(文本输入,停止输入后防抖生效)
    poll_ms: Entity<InputState>,
    max_lines: Entity<InputState>,
    initial_mb: Entity<InputState>,
    tail_mb: Entity<InputState>,
    /// 最近打开保存条数
    recents_limit: Entity<InputState>,
    // 字形:界面/日志字号 + 字体族(空 = 系统默认)
    ui_font_size: Entity<InputState>,
    log_font_size: Entity<InputState>,
    ui_font_family: Entity<InputState>,
    log_font_family: Entity<InputState>,
    // 级别颜色(暗/亮各 7 个 hex 输入)
    color_dark: Vec<Entity<InputState>>,
    color_light: Vec<Entity<InputState>>,
    hl_dark: Entity<InputState>,
    hl_light: Entity<InputState>,
    // 取色器状态:与上面的 hex 输入一一对应,点击色块打开取色器
    pick_dark: Vec<Entity<ColorPickerState>>,
    pick_light: Vec<Entity<ColorPickerState>>,
    pick_hl_dark: Entity<ColorPickerState>,
    pick_hl_light: Entity<ColorPickerState>,
    /// 最近一次自动保存时间(页头「已自动保存」提示)
    saved_at: Option<Instant>,
    // —— 已保存的 SSH 主机管理 ——
    /// 正在编辑的条目(None = 只浏览)
    host_edit: Option<HostEdit>,
    h_name: Entity<InputState>,
    h_host: Entity<InputState>,
    h_port: Entity<InputState>,
    h_user: Entity<InputState>,
    host_err: Option<String>,
    /// 导入等操作的提示(成功/失败)
    host_notice: Option<String>,
    /// URL 凭据删除等操作的提示
    url_cred_notice: Option<String>,
    /// 配置导入/导出的提示
    cfg_notice: Option<String>,
    /// 内置配置是否已解锁编辑(默认只读;切换配置或重开面板时回到只读)
    cfg_unlock: bool,
    /// 正在录制快捷键的动作 id(None = 未录制)
    capture: Option<String>,
    /// 快捷键相关提示
    sc_notice: Option<String>,
}

/// 已保存主机的编辑目标
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostEdit {
    /// 新增(未落盘)
    New,
    /// 编辑第 i 条
    Existing(usize),
}

/// 取色器的写回目标
#[derive(Clone, Copy)]
enum PickTarget {
    LevelDark(usize),
    LevelLight(usize),
    HighlightDark,
    HighlightLight,
}

/// 需要防抖生效的设置项(文本输入停止后才写入设置,避免打字中间值生效)
#[derive(Clone, Copy, PartialEq, Eq)]
enum SetField {
    PollMs,
    MaxLines,
    InitialMb,
    TailMb,
    UiFontSize,
    LogFontSize,
    FontFamily,
    RuleText,
    /// 最近打开保存条数
    RecentsLimit,
}

/// 虚拟列表行尺寸缓存
struct RowsCache {
    /// (tab_id, 可见行数, 视图代次, 换行, 可用宽度, 已测量高度数)
    key: (u64, usize, u64, bool, f32, usize),
    sizes: Rc<Vec<Size<Pixels>>>,
}

impl RowsCache {
    fn new() -> Self {
        Self {
            key: (0, usize::MAX, 0, false, 0.0, 0),
            sizes: Rc::new(Vec::new()),
        }
    }

    fn invalidate(&mut self) {
        self.key = (0, usize::MAX, 0, false, 0.0, 0);
    }
}

/// 配置变更后每帧重判定的行数
const RELEVEL_CHUNK: usize = 100_000;

/// 每帧最多测量多少行自动换行高度
const MEASURE_CHUNK: usize = 800;

/// 「显示时间戳」列宽(px):13pt 等宽下 "HH:MM:SS.mmm" 留足余量
const TS_W: f32 = 100.0;

fn is_hex_color(s: &str) -> bool {
    let t = s.trim().trim_start_matches('#');
    t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit())
}

/// 为选中配置重建规则输入框(切换配置/增删规则后调用);文本改动防抖重判级别
/// 正则一侧是「代码框」:多行输入框 + 语法着色装饰,Enter 提交而不换行
fn dlg_rebuild_rules(
    d: &mut SettingsDialog,
    rules: &[crate::level::Rule],
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    d.rule_inputs.clear();
    let light = !cx.theme().mode.is_dark();
    for r in rules {
        let re = {
            let text = r.regex.clone();
            let e = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(1)
                    .soft_wrap(false) // 正则不折行:超长时在框内横向滚动
                    .submit_on_enter(true) // Enter 提交而不插入换行
            });
            e.update(cx, |s, cx| {
                s.set_value(text.clone(), window, cx);
                s.set_decorations(crate::code_input::decorations(&text, light), cx);
            });
            e
        };
        let fl = {
            let e = cx.new(|cx| InputState::new(window, cx));
            e.update(cx, |s, cx| s.set_value(r.flags.clone(), window, cx));
            e
        };
        // 正则框:每次改动重算着色 + 防抖写回设置;Enter/失焦立即提交
        cx.subscribe(&re, |v, e, ev: &InputEvent, cx| match ev {
            InputEvent::Change => {
                let light = !cx.theme().mode.is_dark();
                e.update(cx, |s, cx| {
                    let text = s.value().to_string();
                    let deco = crate::code_input::decorations(&text, light);
                    s.set_decorations(deco, cx);
                });
                v.schedule_set_field(SetField::RuleText, cx);
            }
            InputEvent::Blur | InputEvent::PressEnter { .. } => {
                v.commit_set_field(SetField::RuleText, cx)
            }
            _ => {}
        })
        .detach();
        cx.subscribe(&fl, |v, _e, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                v.schedule_set_field(SetField::RuleText, cx);
            }
        })
        .detach();
        d.rule_inputs.push((re, fl));
    }
    d.deco_light = Some(light);
}

/// 取色器颜色 → 六位 hex(忽略透明度,与设置文件里的 #rrggbb 对齐)
fn color_hex6(color: Hsla) -> String {
    let s = color.to_hex();
    let t = s.trim_start_matches('#');
    format!("#{}", &t[..t.len().min(6)])
}

/// 按 hex 创建取色器状态(供色块使用)
fn new_color_picker(
    hex: &str,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> Entity<ColorPickerState> {
    let color = theme::hex(hex);
    cx.new(|cx| ColorPickerState::new(window, cx).default_value(color))
}

/// 字号显示:去掉多余小数(13 → "13",12.5 → "12.5")
fn format_size(v: f32) -> String {
    format!("{:.2}", v)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// 字节量可读化(小文件显示 KB)
fn fmt_size(bytes: u64) -> String {
    if bytes < (1 << 20) {
        format!("{:.1} KB", bytes as f64 / (1 << 10) as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / (1 << 20) as f64)
    }
}

/// 估算自动换行后的行高(按字符宽度比例折行)
fn estimate_row_height(text: &str, wrap_w: f32, char_w: f32, row_h: f32) -> f32 {
    if wrap_w <= 1.0 || char_w <= 0.0 {
        return row_h;
    }
    let cols = (wrap_w / char_w).max(1.0) as usize;
    let mut lines = 1usize;
    let mut cur = 0usize;
    for ch in text.chars() {
        let w = if (ch as u32) >= 0x2E80 { 2 } else { 1 };
        if cur + w > cols {
            lines += 1;
            cur = 0;
        }
        cur += w;
    }
    row_h * lines as f32
}

/// 测量单行高度(保留 window/font 形参以便后续精确测量)
fn measure_row_height(
    _window: &mut Window,
    text: &str,
    _font: &gpui_kit::Font,
    font_size: f32,
    row_h: f32,
    wrap_w: f32,
) -> f32 {
    estimate_row_height(text, wrap_w, font_size * 0.6, row_h)
}

/// 合并检索高亮与链接样式:按边界切分,输出升序、互不重叠的区间
/// (SelectableText::highlights 要求区间有序且不重叠)
/// 代码视图一行的高亮:语法制色(不叠加链接)
fn code_row_highlights(
    lang: crate::code::Lang,
    text: &str,
    pal: &'static Palette,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    use crate::code::Class;
    crate::code::line_spans(lang, text)
        .into_iter()
        .map(|(s, e, class)| {
            let color = match class {
                Class::Comment => pal.code_comment,
                Class::Str => pal.code_str,
                Class::Number => pal.code_num,
                Class::Keyword => pal.code_kw,
                Class::Key => pal.code_key,
            };
            (
                s..e,
                HighlightStyle {
                    color: Some(theme::hex(color)),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn merge_row_highlights(
    searched: &[(usize, usize)],
    links: &[links::Link],
    link_color: Hsla,
    hl_bg: Hsla,
    hl_fg: Hsla,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut cuts: Vec<usize> = Vec::with_capacity(searched.len() * 2 + links.len() * 2);
    for &(s, e) in searched {
        cuts.push(s);
        cuts.push(e);
    }
    for l in links {
        cuts.push(l.start);
        cuts.push(l.end);
    }
    if cuts.is_empty() {
        return Vec::new();
    }
    cuts.sort_unstable();
    cuts.dedup();

    let underline = UnderlineStyle {
        thickness: px(1.0),
        color: Some(link_color),
        wavy: false,
    };
    let mut out: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    for w in cuts.windows(2) {
        let (s, e) = (w[0], w[1]);
        if s >= e {
            continue;
        }
        let in_link = links.iter().any(|l| l.start <= s && e <= l.end);
        let in_search = searched.iter().any(|&(a, b)| a <= s && e <= b);
        if !in_link && !in_search {
            continue;
        }
        let style = HighlightStyle {
            // 链接优先保留主题色;检索命中用高对比文字色 + 加粗,保证在多种底色上都能看清
            color: if in_link {
                Some(link_color)
            } else if in_search {
                Some(hl_fg)
            } else {
                None
            },
            underline: in_link.then_some(underline),
            background_color: in_search.then_some(hl_bg),
            font_weight: (in_search && !in_link).then_some(gpui_kit::FontWeight::BOLD),
            ..Default::default()
        };
        // 与上一段样式相同且首尾相接时合并
        if let Some((range, prev)) = out.last_mut() {
            if *prev == style && range.end == s {
                range.end = e;
                continue;
            }
        }
        out.push((s..e, style));
    }
    out
}

/// 菜单里的链接标签过长时截断(按字符边界,避免撑宽菜单)
fn short_link(text: &str) -> String {
    const MAX: usize = 48;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let mut s: String = text.chars().take(MAX - 1).collect();
    s.push('…');
    s
}

/// 把窗口坐标映射为行内字节位置,返回鼠标下的链接(无命中返回 None)
fn link_at_cursor(
    window: &Window,
    text: &str,
    links: &[links::Link],
    bounds: Bounds<Pixels>,
    wrap: bool,
    mono: &SharedString,
) -> Option<links::Link> {
    if links.is_empty() {
        return None;
    }
    let pointer = window.mouse_position();
    if !bounds.contains(&pointer) {
        return None;
    }
    // bounds 为文本容器边界,文字从容器左上角开始
    let local_x = pointer.x - bounds.left();
    if local_x < px(0.0) {
        return None;
    }
    let font_size = px(theme::log_scaled(13.0));
    let line_height = px(theme::row_h());
    let font = gpui_kit::font(mono.clone());
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color: gpui_kit::transparent_black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    let index = if wrap {
        // 换行模式:按文本区宽度重新排版,再用行内坐标定位(支持多视觉行)
        let text_w = bounds.size.width;
        if text_w <= px(1.0) {
            return None;
        }
        let lines = window
            .text_system()
            .shape_text(
                SharedString::from(text.to_string()),
                font_size,
                &[run],
                Some(text_w),
                None,
            )
            .ok()?;
        let line = lines.first()?;
        let local_y = pointer.y - bounds.top();
        line.index_for_position(point(local_x, local_y), line_height)
            .ok()?
    } else {
        let layout = window
            .text_system()
            .layout_line(text, font_size, &[run], None);
        layout.index_for_x(local_x)?
    };

    links
        .iter()
        .find(|l| index >= l.start && index < l.end)
        .cloned()
}

/// 主视图:持有全部标签与全局 UI 状态
pub struct AppView {
    tabs: Vec<Tab>,
    active: usize,
    settings: Settings,
    next_tab_id: u64,
    search_input: Entity<InputState>,
    /// 日志区滚动句柄(粘底/跳转)
    list: VirtualListScrollHandle,
    /// 虚拟列表行尺寸缓存
    rows_cache: RowsCache,
    /// 强制贴底后的宽限窗口:期间不解除跟随(吸收触板惯性产生的残余滚动事件)
    suppress_follow_until: Option<Instant>,
    /// 离开底部的连续轮询次数(去抖:连续 2 次才解除跟随)
    off_bottom_frames: u32,
    /// 当前弹层(SSH / URL)
    dialog: Dialog,
    /// 设置面板状态(仅在设置标签页存在时有效)
    settings_panel: Option<SettingsDialog>,
    /// 配置多选下拉框的弹出位置(窗口坐标)
    config_menu: Option<(f32, f32)>,
    /// 设置版本号:变更后驱动各标签重编译引擎
    settings_rev: u64,
    /// 凭据相关的一次性提示(状态栏展示,成功后清空)
    cred_notice: Option<String>,
    /// 欢迎页「清除最近打开」的二次确认状态
    recents_confirm: bool,
    /// 是否已有一次防抖落盘在排队(避免每次改动都写文件)
    save_scheduled: bool,
    /// 文本输入防抖的代数:同一设置项只有最后一次定时器生效
    debounce_gen: u64,
    /// 已排队的(设置项, 代数),用于丢弃过期提交
    pending_commits: Vec<(SetField, u64)>,
    /// 子线程报告认证失败后的待处理重试(标签页 id, 参数, 错误);下一帧弹出连接表单
    auth_retry: Option<(u64, SourceParams, String)>,
    /// 等待确认的未知主机(标签页 id, 主机密钥);按到达顺序逐个弹窗
    host_key_queue: Vec<(u64, crate::source::HostKeyInfo)>,
    /// 根节点焦点:快捷键(⌘T/⌘K… )的接收者
    focus_handle: FocusHandle,
    /// 鼠标上方的短提示(如「已复制」),到期由 poll 清理
    toast: Option<Toast>,
    /// 「关于」页的更新检查状态
    update_state: UpdateState,
    /// 更新检查的结果通道(None = 没有在途请求)
    update_rx: Option<crossbeam_channel::Receiver<Result<Option<Release>, String>>>,
}

/// 轻量 toast:在鼠标位置上方短暂提示一条消息
struct Toast {
    text: String,
    /// 触发时的鼠标位置(窗口坐标)
    at: gpui_kit::Point<Pixels>,
    /// 过期时间
    until: Instant,
}

impl AppView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        paths: Vec<std::path::PathBuf>,
        settings: Settings,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(i18n::t("search.placeholder")));

        // 字号来自设置:必须在首帧渲染前写入主题运行时值
        theme::set_font_sizes(settings.ui_font_size, settings.log_font_size);
        // 启动期凭据迁移的提示(设置文件不持久化该字段)
        let cred_notice = settings.cred_notice.clone();

        let mut view = Self {
            tabs: Vec::new(),
            active: 0,
            settings,
            next_tab_id: 1,
            search_input: search_input.clone(),
            list: VirtualListScrollHandle::new(),
            rows_cache: RowsCache::new(),
            suppress_follow_until: None,
            off_bottom_frames: 0,
            dialog: Dialog::None,
            settings_panel: None,
            config_menu: None,
            settings_rev: 0,
            cred_notice,
            recents_confirm: false,
            save_scheduled: false,
            debounce_gen: 0,
            pending_commits: Vec::new(),
            auth_retry: None,
            host_key_queue: Vec::new(),
            focus_handle: cx.focus_handle(),
            toast: None,
            update_state: UpdateState::Idle,
            update_rx: None,
        };

        for p in paths {
            if let Some(s) = p.to_str() {
                view.open_local(s.to_string());
            }
        }

        // 无 CLI 参数时默认打开一个空白标签页(VS Code 式欢迎页)
        if view.tabs.is_empty() {
            view.add_blank_tab(window, cx);
        }

        // 卡顿诊断(可选):LOGVIEW_DIAG=1 时把主线程超过 200ms 的停顿追加到 stall.log
        if std::env::var("LOGVIEW_DIAG").is_ok() {
            let path = dirs::config_dir().map(|d| d.join("logview").join("stall.log"));
            cx.spawn(async move |_this, cx| {
                let mut last = Instant::now();
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let now = Instant::now();
                    let gap = now.duration_since(last);
                    if gap > Duration::from_millis(200) {
                        if let Some(path) = path.as_ref() {
                            use std::io::Write;
                            let stamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis())
                                .unwrap_or(0);
                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(path)
                            {
                                let _ = writeln!(f, "t={stamp} stall_ms={}", gap.as_millis());
                            }
                        }
                    }
                    last = now;
                }
            })
            .detach();
        }

        // 测试便利：LOGVIEW_PREVIEW_SETTINGS=1 启动即展开设置页
        if std::env::var("LOGVIEW_PREVIEW_SETTINGS").is_ok() {
            view.open_settings_tab(None, window, cx);
        }

        // 检索输入 → 当前标签查询(增量重编译 + 全量重建匹配集);Enter = 下一处
        cx.subscribe(
            &search_input,
            |view, _input, ev: &InputEvent, cx| match ev {
                InputEvent::Change => view.apply_search(cx),
                InputEvent::PressEnter { .. } => view.jump_match(true, cx),
                _ => {}
            },
        )
        .detach();

        // 轮询 worker 消息:gpui 不会因 channel 有消息而出帧,定时唤起 update
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                this.update(cx, |v, cx| {
                    v.poll();
                    // 有消息或测量进度时需要重绘(gpui 不会因 channel 自动出帧)
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();

        // 根节点默认拿焦点,否则无输入框聚焦时快捷键收不到事件
        window.focus(&view.focus_handle, cx);

        view
    }

    // ---- 标签管理 ----

    fn open_local(&mut self, path: String) {
        self.open_source(SourceParams::Local { path }, false);
    }

    /// 新建空白标签页:内嵌「打开日志源」表单,不启动 worker
    /// 已存在空白标签页时直接切过去,避免重复创建
    fn add_blank_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.tabs.iter().position(|t| t.welcome) {
            self.dialog = Dialog::None;
            self.switch_tab(i, window, cx);
            return;
        }
        let mut t = Tab::blank(&self.settings);
        t.id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
        self.rows_cache.invalidate();
        self.dialog = Dialog::None;
        self.search_input
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
    }

    /// 把空白标签页 ti 替换为已加载的日志标签页(保留 tab id)
    fn open_in_tab(&mut self, ti: usize, params: SourceParams, remember: bool) {
        if ti >= self.tabs.len() {
            return;
        }
        let id = self.tabs[ti].id;
        // 代码/配置文件:直接用代码视图替换空白标签
        if let SourceParams::Local { path } = &params {
            if let Some(mut t) = Tab::try_code_tab(&self.settings, path) {
                t.id = id;
                self.tabs[ti] = t;
                self.active = ti;
                self.rows_cache.invalidate();
                self.dialog = Dialog::None;
                self.record_recent_opt(params, remember);
                return;
            }
        }
        let mut t = Tab::open(&self.settings, params.clone());
        t.id = id;
        self.tabs[ti] = t;
        self.active = ti;
        self.rows_cache.invalidate();
        self.record_recent_opt(params, remember);
    }

    /// 从系统拖入本地文件:每个文件单独在新标签页打开(目录与非法路径忽略)
    fn handle_file_drop(&mut self, paths: &ExternalPaths) {
        for p in paths.paths() {
            if p.is_dir() {
                continue;
            }
            if let Some(s) = p.to_str() {
                self.open_local(s.to_string());
            }
        }
    }

    /// 日志内链接:URL 以 HTTP 日志源在新标签页打开;路径按当前源解析后在新标签页打开
    fn open_from_link(&mut self, kind: LinkKind, text: String) {
        match kind {
            LinkKind::Url => {
                // 非 http/https 无法作为日志源,交给系统默认程序处理
                if !text.starts_with("http://") && !text.starts_with("https://") {
                    let _ = open::that(&text);
                    return;
                }
                self.open_source(
                    SourceParams::Http {
                        url: text,
                        auth: crate::source::HttpAuthKind::None,
                        user: None,
                        password: None,
                        token: None,
                        header_name: None,
                        header_value: None,
                    },
                    false,
                );
            }
            LinkKind::Path => {
                let params = match self.tabs.get(self.active).and_then(|t| t.params.clone()) {
                    Some(SourceParams::Ssh {
                        host,
                        port,
                        user,
                        auth,
                        path,
                    }) => {
                        // 相对路径按当前远端日志所在目录解析
                        let is_abs = text.starts_with('/')
                            || (text.len() >= 2 && text.as_bytes()[1] == b':');
                        let p = if is_abs {
                            text.clone()
                        } else {
                            let dir = std::path::Path::new(&path)
                                .parent()
                                .map(|d| d.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            format!("{dir}/{text}")
                        };
                        SourceParams::Ssh {
                            host,
                            port,
                            user,
                            auth,
                            path: p,
                        }
                    }
                    _ => SourceParams::Local { path: text },
                };
                self.open_source(params, false);
            }
        }
    }

    /// 打开任意源:新标签 + 记录最近打开。
    /// 参数缺凭据时先尝试从系统钥匙串回填;取不到则继续打开,由 worker 报错(欢迎页路径会弹表单补录)。
    fn open_source(&mut self, mut params: SourceParams, remember: bool) {
        if let Some(msg) = self.fill_from_keyring(&mut params) {
            self.cred_notice = Some(msg);
        }
        // 本地代码/配置文件:用只读高亮视图打开,不启动 tail worker
        if let SourceParams::Local { path } = &params {
            if let Some(mut t) = Tab::try_code_tab(&self.settings, path) {
                t.id = self.next_tab_id;
                self.next_tab_id += 1;
                self.tabs.push(t);
                self.active = self.tabs.len() - 1;
                self.dialog = Dialog::None;
                self.rows_cache.invalidate();
                self.record_recent_opt(params, remember);
                return;
            }
        }
        let mut t = Tab::open(&self.settings, params.clone());
        t.id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
        self.dialog = Dialog::None;
        self.record_recent_opt(params, remember);
    }

    /// 用钥匙串补齐参数中缺失的凭据;返回 None 表示无需凭据或已补齐
    fn fill_from_keyring(&mut self, params: &mut SourceParams) -> Option<String> {
        if !params.needs_secret() {
            return None;
        }
        let key = key_of_params(params)?;
        match secrets::load(&key) {
            Ok(Some(sec)) if sec.apply(params) => None,
            Ok(_) => Some(i18n::t("ssh.creds.missing").into()),
            Err(e) => Some(i18n::tf("ssh.creds.reenter", &[("e", e)])),
        }
    }

    /// 记录最近打开:设置文件里永远只存脱敏参数;勾选「记住连接」时把凭据写入系统钥匙串
    fn record_recent_opt(&mut self, params: SourceParams, remember: bool) {
        let secret = Secret::from_params(&params);
        let key = key_of_params(&params);
        let mut remembered = false;
        if remember {
            if let Some(k) = key.as_deref() {
                if !secret.is_empty() {
                    match secrets::save(k, &secret) {
                        Ok(()) => {
                            remembered = true;
                            self.cred_notice = None;
                        }
                        Err(e) => {
                            self.cred_notice =
                                Some(i18n::tf("ssh.creds.session_only", &[("e", e)]));
                        }
                    }
                }
            }
        }
        // 未新写入时,以钥匙串中是否已有条目为准(决定欢迎页是否提供「清除凭据」)
        if !remembered && !params.has_secret() {
            if let Some(k) = key.as_deref() {
                remembered = matches!(secrets::load(k), Ok(Some(_)));
            }
        }
        let stored = params.sanitized();
        let label = stored.label();
        let recents = &mut self.settings.recents;
        recents.retain(|r| r.label != label);
        recents.insert(
            0,
            Recent {
                label,
                params: stored,
                remembered,
            },
        );
        recents.truncate(self.settings.recents_limit.max(1));
        store::save(&self.settings);
    }

    fn switch_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() || i == self.active {
            return;
        }
        self.active = i;
        self.rows_cache.invalidate(); // 强制重建行尺寸
        let q = self.tabs[i].query.clone();
        self.search_input
            .update(cx, |s, cx| s.set_value(q, window, cx));
    }

    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        let was_settings = self.tabs.get(i).is_some_and(|t| t.settings);
        let closed_id = self.tabs.get(i).map(|t| t.id);
        if let Some(t) = self.tabs.get(i) {
            let _ = t.cmd_tx.send(Cmd::Stop);
        }
        self.tabs.remove(i);
        if let Some(id) = closed_id {
            // 关闭标签页时丢弃它的待确认主机,并关闭对应弹窗
            self.host_key_queue.retain(|(t, _)| *t != id);
            if let Dialog::HostKey(f) = &self.dialog {
                if f.tab == id {
                    self.dialog = Dialog::None;
                    self.show_next_host_key();
                }
            }
        }
        if was_settings {
            // 关闭设置页:面板状态释放(改动已实时写入设置)
            self.settings_panel = None;
        }
        // 关闭最后一个标签页后自动补一个空白标签页,不再显示空状态页
        if self.tabs.is_empty() {
            self.active = 0;
            self.add_blank_tab(window, cx);
            return;
        }
        if i < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        let q = self
            .tabs
            .get(self.active)
            .map(|t| t.query.clone())
            .unwrap_or_default();
        self.search_input
            .update(cx, |s, cx| s.set_value(q, window, cx));
    }

    /// 关闭其他标签页(保留第 keep 个)
    fn close_other_tabs(&mut self, keep: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(keep_id) = self.tabs.get(keep).map(|t| t.id) else {
            return;
        };
        let mut i = self.tabs.len();
        while i > 0 {
            i -= 1;
            if self.tabs.get(i).map(|t| t.id) != Some(keep_id) {
                self.close_tab(i, window, cx);
            }
        }
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// 界面字体族:设置为空时跟随主题内置 UI 字体
    fn ui_font(&self, cx: &gpui_kit::App) -> SharedString {
        let f = self.settings.ui_font_family.trim();
        if f.is_empty() {
            cx.theme().font_family.clone()
        } else {
            SharedString::from(f.to_string())
        }
    }

    /// 日志字体族:设置为空时跟随主题内置等宽字体
    fn log_font(&self, cx: &gpui_kit::App) -> SharedString {
        let f = self.settings.log_font_family.trim();
        if f.is_empty() {
            cx.theme().mono_font_family.clone()
        } else {
            SharedString::from(f.to_string())
        }
    }

    // ---- 打开文件 ----

    /// 空白标签页的「浏览…」:选一个本地文件后直接在该标签页打开
    fn welcome_browse(&mut self, ti: usize, cx: &mut Context<Self>) {
        let id = self.tabs.get(ti).map(|t| t.id);
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let path = paths
                    .into_iter()
                    .find(|p| p.is_file())
                    .and_then(|p| p.to_str().map(str::to_string));
                if let Some(path) = path {
                    this.update(cx, |v, _cx| {
                        // 标签页可能已被关闭:按 id 找回,找不到就新开一个
                        let target =
                            id.and_then(|id| v.tabs.iter().position(|t| t.id == id && t.welcome));
                        let params = SourceParams::Local { path };
                        if let Some(ti) = target {
                            v.open_in_tab(ti, params, false);
                        } else {
                            v.open_source(params, false);
                        }
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    // ---- 对话框管理 ----

    fn close_dialog(&mut self) {
        self.dialog = Dialog::None;
    }

    /// 构建设置面板:输入框直接绑定 live 设置,改动即时(或防抖)生效
    fn make_settings_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> SettingsDialog {
        let mk = |v: String, window: &mut Window, cx: &mut Context<Self>| {
            let e = cx.new(|cx| InputState::new(window, cx));
            e.update(cx, |s, cx| s.set_value(v, window, cx));
            e
        };
        let s = &self.settings;
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::t("settings.search.placeholder"))
        });
        let color_dark: Vec<Entity<InputState>> = s
            .level_colors_hex(false)
            .into_iter()
            .map(|c| mk(c, window, cx))
            .collect();
        let color_light: Vec<Entity<InputState>> = s
            .level_colors_hex(true)
            .into_iter()
            .map(|c| mk(c, window, cx))
            .collect();
        let pick_dark: Vec<Entity<ColorPickerState>> = s
            .level_colors_hex(false)
            .iter()
            .map(|c| new_color_picker(c, window, cx))
            .collect();
        let pick_light: Vec<Entity<ColorPickerState>> = s
            .level_colors_hex(true)
            .iter()
            .map(|c| new_color_picker(c, window, cx))
            .collect();
        let mut dlg = SettingsDialog {
            tab: 0,
            search: search.clone(),
            sel_config: 0,
            rule_inputs: Vec::new(),
            deco_light: None,
            poll_ms: mk(s.poll_ms.to_string(), window, cx),
            max_lines: mk(s.max_lines.to_string(), window, cx),
            initial_mb: mk(s.initial_mb.to_string(), window, cx),
            tail_mb: mk(s.tail_mb.to_string(), window, cx),
            recents_limit: mk(s.recents_limit.to_string(), window, cx),
            ui_font_size: mk(format_size(s.ui_font_size), window, cx),
            log_font_size: mk(format_size(s.log_font_size), window, cx),
            ui_font_family: mk(s.ui_font_family.clone(), window, cx),
            log_font_family: mk(s.log_font_family.clone(), window, cx),
            color_dark,
            color_light,
            hl_dark: mk(s.highlight_dark.clone(), window, cx),
            hl_light: mk(s.highlight_light.clone(), window, cx),
            pick_dark,
            pick_light,
            pick_hl_dark: new_color_picker(&s.highlight_dark, window, cx),
            pick_hl_light: new_color_picker(&s.highlight_light, window, cx),
            saved_at: None,
            host_edit: None,
            h_name: mk(String::new(), window, cx),
            h_host: mk(String::new(), window, cx),
            h_port: mk("22".to_string(), window, cx),
            h_user: mk(String::new(), window, cx),
            host_err: None,
            host_notice: None,
            url_cred_notice: None,
            cfg_notice: None,
            cfg_unlock: false,
            capture: None,
            sc_notice: None,
        };
        for (i, p) in dlg.pick_dark.clone().iter().enumerate() {
            self.watch_picker(p, PickTarget::LevelDark(i), window, cx);
        }
        for (i, p) in dlg.pick_light.clone().iter().enumerate() {
            self.watch_picker(p, PickTarget::LevelLight(i), window, cx);
        }
        self.watch_picker(
            &dlg.pick_hl_dark.clone(),
            PickTarget::HighlightDark,
            window,
            cx,
        );
        self.watch_picker(
            &dlg.pick_hl_light.clone(),
            PickTarget::HighlightLight,
            window,
            cx,
        );
        // 文本类输入:防抖 450ms 生效;颜色输入:合法值随输入即时生效
        self.watch_set_field(&dlg.poll_ms, SetField::PollMs, cx);
        self.watch_set_field(&dlg.max_lines, SetField::MaxLines, cx);
        self.watch_set_field(&dlg.initial_mb, SetField::InitialMb, cx);
        self.watch_set_field(&dlg.tail_mb, SetField::TailMb, cx);
        self.watch_set_field(&dlg.recents_limit, SetField::RecentsLimit, cx);
        self.watch_set_field(&dlg.ui_font_size, SetField::UiFontSize, cx);
        self.watch_set_field(&dlg.log_font_size, SetField::LogFontSize, cx);
        self.watch_set_field(&dlg.ui_font_family, SetField::FontFamily, cx);
        self.watch_set_field(&dlg.log_font_family, SetField::FontFamily, cx);
        for e in &dlg.color_dark {
            self.watch_color(e, false, window, cx);
        }
        for e in &dlg.color_light {
            self.watch_color(e, true, window, cx);
        }
        self.watch_highlight(&dlg.hl_dark, window, cx);
        self.watch_highlight(&dlg.hl_light, window, cx);
        // 搜索框:输入即重绘(过滤在渲染时完成)
        cx.subscribe(&search, |_v, _e, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let rules = s
            .configs
            .first()
            .map(|c| c.rules.clone())
            .unwrap_or_default();
        dlg_rebuild_rules(&mut dlg, &rules, window, cx);
        dlg
    }

    /// 打开设置标签页:convert = Some(ti) 把该空白页替换为设置页;None 新建
    fn open_settings_tab(
        &mut self,
        convert: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 只保留一个设置标签页,再次打开时直接切换过去
        if let Some(pos) = self.tabs.iter().position(|t| t.settings) {
            self.active = pos;
            self.rows_cache.invalidate();
            let q = self.tabs[pos].query.clone();
            self.search_input
                .update(cx, |s, cx| s.set_value(q, window, cx));
            return;
        }
        self.settings_panel = Some(self.make_settings_dialog(window, cx));
        self.dialog = Dialog::None;
        match convert.filter(|ti| *ti < self.tabs.len()) {
            Some(ti) => {
                let id = self.tabs[ti].id;
                let mut t = Tab::settings(&self.settings);
                t.id = id;
                self.tabs[ti] = t;
                self.active = ti;
            }
            None => {
                let mut t = Tab::settings(&self.settings);
                t.id = self.next_tab_id;
                self.next_tab_id += 1;
                self.tabs.push(t);
                self.active = self.tabs.len() - 1;
            }
        }
        self.rows_cache.invalidate();
        self.search_input
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
    }

    /// 把规则输入框内容写回设置(切换配置/增删规则/文本改动时调用)
    /// 把规则输入框内容写回设置（切换配置/增删规则/文本改动时调用）；返回是否有实际改动
    fn sync_rule_inputs(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(d) = self.settings_panel.as_ref() else {
            return false;
        };
        let Some(cfg) = self.settings.configs.get_mut(d.sel_config) else {
            return false;
        };
        let mut changed = false;
        for (i, (re, fl)) in d.rule_inputs.iter().enumerate() {
            let Some(rule) = cfg.rules.get_mut(i) else {
                break;
            };
            let regex = re.read(cx).value().to_string();
            let flags = fl.read(cx).value().to_string();
            if rule.regex != regex || rule.flags != flags {
                rule.regex = regex;
                rule.flags = flags;
                changed = true;
            }
        }
        changed
    }

    fn rebuild_rule_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.settings_panel.as_mut() else {
            return;
        };
        let rules = self
            .settings
            .configs
            .get(d.sel_config)
            .map(|c| c.rules.clone())
            .unwrap_or_default();
        dlg_rebuild_rules(d, &rules, window, cx);
    }

    /// 设置改动统一落地:参数夹取 → 主题字号 → 落盘 → 各标签重新对齐 → 自动保存提示
    fn settings_apply(&mut self, configs_changed: bool, cx: &mut Context<Self>) {
        self.settings.poll_ms = self.settings.poll_ms.max(200);
        self.settings.max_lines = self.settings.max_lines.max(1000);
        self.settings.initial_mb = self.settings.initial_mb.clamp(1, 512);
        self.settings.tail_mb = self.settings.tail_mb.clamp(1, 512);
        self.settings.recents_limit = self.settings.recents_limit.clamp(1, 500);
        self.settings.ui_font_size = self
            .settings
            .ui_font_size
            .clamp(theme::FONT_SIZE_MIN, theme::FONT_SIZE_MAX);
        self.settings.log_font_size = self
            .settings
            .log_font_size
            .clamp(theme::FONT_SIZE_MIN, theme::FONT_SIZE_MAX);
        self.settings_rev += 1;
        // 字号变化影响所有行高(含换行测量),必须清空各标签的行高缓存
        let size_changed =
            theme::set_font_sizes(self.settings.ui_font_size, self.settings.log_font_size);
        if size_changed {
            for tab in &mut self.tabs {
                tab.heights.clear();
                tab.heights_w = 0.0;
            }
        }
        self.schedule_save(cx);
        for tab in &mut self.tabs {
            tab.store.max_lines = self.settings.max_lines;
            if configs_changed {
                tab.reconcile_configs(&self.settings.configs);
            }
        }
        self.rows_cache.invalidate();
        if let Some(d) = self.settings_panel.as_mut() {
            d.saved_at = Some(Instant::now());
        }
        cx.notify();
    }

    /// 文本输入防抖：停止输入 450ms 后落到设置（避免打字过程中的中间值生效）
    /// 同一设置项的后一次输入会作废前一次的定时器，避免连打时反复提交
    fn schedule_set_field(&mut self, field: SetField, cx: &mut Context<Self>) {
        self.debounce_gen = self.debounce_gen.wrapping_add(1);
        let generation = self.debounce_gen;
        self.pending_commits.retain(|(f, _)| *f != field);
        self.pending_commits.push((field, generation));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(450))
                .await;
            this.update(cx, |v, cx| {
                let Some(pos) = v
                    .pending_commits
                    .iter()
                    .position(|(f, g)| *f == field && *g == generation)
                else {
                    return; // 已有更新的输入，本次作废
                };
                v.pending_commits.remove(pos);
                v.commit_set_field(field, cx);
            })
            .ok();
        })
        .detach();
    }

    /// 设置落盘：500ms 内多次改动合并为一次，并在后台线程写文件（不阻塞 UI）
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.save_scheduled {
            return;
        }
        self.save_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let snapshot = this
                .update(cx, |v, _| {
                    v.save_scheduled = false;
                    v.settings.clone()
                })
                .ok();
            if let Some(snapshot) = snapshot {
                cx.background_executor()
                    .spawn(async move { store::save(&snapshot) })
                    .detach();
            }
        })
        .detach();
    }

    /// 提交单个设置项：非法输入保留原值，字体族留空 = 跟随系统
    fn commit_set_field(&mut self, field: SetField, cx: &mut Context<Self>) {
        if field == SetField::RuleText {
            // 仅内容真的变了才重分析：切换配置/点到其他控件引起的失焦不再触发全量重判
            if self.sync_rule_inputs(cx) {
                self.settings_apply(true, cx);
            }
            return;
        }
        let vals = {
            let Some(d) = self.settings_panel.as_ref() else {
                return;
            };
            let read = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
            (
                read(&d.poll_ms),
                read(&d.max_lines),
                read(&d.initial_mb),
                read(&d.tail_mb),
                read(&d.ui_font_size),
                read(&d.log_font_size),
                read(&d.ui_font_family),
                read(&d.log_font_family),
                read(&d.recents_limit),
            )
        };
        let (poll, max, initial, tail, ui_size, log_size, ui_fam, log_fam, recents_n) = vals;
        match field {
            SetField::PollMs => {
                if let Ok(v) = poll.parse::<u64>() {
                    self.settings.poll_ms = v.max(200);
                }
            }
            SetField::MaxLines => {
                if let Ok(v) = max.parse::<usize>() {
                    self.settings.max_lines = v.max(1000);
                }
            }
            SetField::InitialMb => {
                if let Ok(v) = initial.parse::<u64>() {
                    self.settings.initial_mb = v.clamp(1, 512);
                }
            }
            SetField::TailMb => {
                if let Ok(v) = tail.parse::<u64>() {
                    self.settings.tail_mb = v.clamp(1, 512);
                }
            }
            SetField::UiFontSize => {
                if let Ok(v) = ui_size.parse::<f32>() {
                    self.settings.ui_font_size =
                        v.clamp(theme::FONT_SIZE_MIN, theme::FONT_SIZE_MAX);
                }
            }
            SetField::LogFontSize => {
                if let Ok(v) = log_size.parse::<f32>() {
                    self.settings.log_font_size =
                        v.clamp(theme::FONT_SIZE_MIN, theme::FONT_SIZE_MAX);
                }
            }
            SetField::FontFamily => {
                let changed = ui_fam != self.settings.ui_font_family
                    || log_fam != self.settings.log_font_family;
                self.settings.ui_font_family = ui_fam;
                self.settings.log_font_family = log_fam;
                if changed {
                    for tab in &mut self.tabs {
                        tab.heights.clear();
                        tab.heights_w = 0.0;
                        tab.view_gen = tab.view_gen.wrapping_add(1);
                    }
                }
            }
            SetField::RuleText => {}
            SetField::RecentsLimit => {
                if let Ok(v) = recents_n.parse::<usize>() {
                    self.settings.recents_limit = v.clamp(1, 500);
                    // 调小时立即生效:丢掉超出条数的旧记录(不清凭据)
                    let limit = self.settings.recents_limit;
                    self.settings.recents.truncate(limit);
                }
            }
        }
        self.settings_apply(false, cx);
    }

    /// 级别配色:合法 hex 立即写入(非法/清空保留原值),并同步色块
    fn commit_level_colors(
        &mut self,
        light_theme: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let inputs = if light_theme {
            &d.color_light
        } else {
            &d.color_dark
        };
        let vals: Vec<String> = inputs
            .iter()
            .map(|e| e.read(cx).value().to_string())
            .collect();
        {
            let target = if light_theme {
                &mut self.settings.colors_light
            } else {
                &mut self.settings.colors
            };
            for (i, v) in vals.iter().enumerate() {
                if is_hex_color(v) {
                    while target.len() <= i {
                        target.push(String::new());
                    }
                    target[i] = v.clone();
                }
            }
        }
        // 输入框 → 色块
        let picks = if light_theme {
            &d.pick_light
        } else {
            &d.pick_dark
        };
        for (i, v) in vals.iter().enumerate() {
            if is_hex_color(v) {
                if let Some(p) = picks.get(i) {
                    let c = theme::hex(v);
                    p.update(cx, |s, cx| s.set_value(c, window, cx));
                }
            }
        }
        self.settings_apply(false, cx);
    }

    /// 检索高亮底色:合法 hex 立即写入,并同步色块
    fn commit_highlight_colors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let hd = d.hl_dark.read(cx).value().to_string();
        let hl = d.hl_light.read(cx).value().to_string();
        if is_hex_color(&hd) {
            self.settings.highlight_dark = hd.clone();
            let c = theme::hex(&hd);
            d.pick_hl_dark
                .update(cx, |s, cx| s.set_value(c, window, cx));
        }
        if is_hex_color(&hl) {
            self.settings.highlight_light = hl.clone();
            let c = theme::hex(&hl);
            d.pick_hl_light
                .update(cx, |s, cx| s.set_value(c, window, cx));
        }
        self.settings_apply(false, cx);
    }

    /// 订阅取色器:选色后写回设置并同步 hex 输入框
    fn watch_picker(
        &self,
        p: &Entity<ColorPickerState>,
        target: PickTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            p,
            window,
            move |v, _p, ev: &ColorPickerEvent, window, cx| {
                if let ColorPickerEvent::Change(Some(color)) = ev {
                    v.apply_picked_color(target, color_hex6(*color), window, cx);
                }
            },
        )
        .detach();
    }

    /// 取色器选色 → 设置 + hex 输入框
    fn apply_picked_color(
        &mut self,
        target: PickTarget,
        hex: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let put = |v: &mut Vec<String>, i: usize, hex: &str| {
            while v.len() <= i {
                v.push(String::new());
            }
            v[i] = hex.to_string();
        };
        let input = match target {
            PickTarget::LevelDark(i) => {
                put(&mut self.settings.colors, i, &hex);
                d.color_dark.get(i).cloned()
            }
            PickTarget::LevelLight(i) => {
                put(&mut self.settings.colors_light, i, &hex);
                d.color_light.get(i).cloned()
            }
            PickTarget::HighlightDark => {
                self.settings.highlight_dark = hex.clone();
                Some(d.hl_dark.clone())
            }
            PickTarget::HighlightLight => {
                self.settings.highlight_light = hex.clone();
                Some(d.hl_light.clone())
            }
        };
        if let Some(e) = input {
            let v = hex.clone();
            e.update(cx, |s, cx| s.set_value(v, window, cx));
        }
        self.settings_apply(false, cx);
    }

    /// 订阅数值/字体输入:Change 防抖、失焦或回车立即提交
    fn watch_set_field(&self, e: &Entity<InputState>, field: SetField, cx: &mut Context<Self>) {
        cx.subscribe(e, move |v, _e, ev: &InputEvent, cx| match ev {
            InputEvent::Change => v.schedule_set_field(field, cx),
            InputEvent::Blur | InputEvent::PressEnter { .. } => v.commit_set_field(field, cx),
            _ => {}
        })
        .detach();
    }

    /// 订阅颜色输入:合法值随输入即时生效
    fn watch_color(
        &self,
        e: &Entity<InputState>,
        light_theme: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(e, window, move |v, _e, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::Blur) {
                v.commit_level_colors(light_theme, window, cx);
            }
        })
        .detach();
    }

    fn watch_highlight(&self, e: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe_in(e, window, |v, _e, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::Blur) {
                v.commit_highlight_colors(window, cx);
            }
        })
        .detach();
    }

    /// 选中配置当前是否只读(内置配置未解锁时只读)
    fn config_readonly(&self) -> bool {
        let Some(d) = self.settings_panel.as_ref() else {
            return false;
        };
        !d.cfg_unlock
            && self
                .settings
                .configs
                .get(d.sel_config)
                .is_some_and(|c| c.built_in)
    }

    /// 内置配置的只读 / 编辑切换:回到只读前先把输入框内容落回设置,避免丢改动
    fn toggle_config_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(unlocked) = self.settings_panel.as_ref().map(|d| d.cfg_unlock) else {
            return;
        };
        if unlocked {
            // 走正常提交路径:写回规则并触发重分析,然后按落盘内容刷新输入框
            self.commit_set_field(SetField::RuleText, cx);
            self.load_rule_inputs(window, cx);
        }
        if let Some(d) = self.settings_panel.as_mut() {
            d.cfg_unlock = !unlocked;
        }
        cx.notify();
    }

    fn select_config(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_rule_inputs(cx);
        if i < self.settings.configs.len() {
            if let Some(d) = self.settings_panel.as_mut() {
                d.sel_config = i;
                // 内置配置默认只读:每切换一次都重新上锁
                d.cfg_unlock = false;
            }
        }
        self.load_rule_inputs(window, cx);
    }

    /// 把选中配置的规则装进输入框：条数相同则原地换内容（不重建实体、不触发失焦提交），
    /// 条数变了才重建。反复切换配置页签因此只剩一次赋值 + 着色重算。
    fn load_rule_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.settings_panel.as_ref().map(|d| d.sel_config) else {
            return;
        };
        let rules = self
            .settings
            .configs
            .get(sel)
            .map(|c| c.rules.clone())
            .unwrap_or_default();
        let same_len = self
            .settings_panel
            .as_ref()
            .is_some_and(|d| d.rule_inputs.len() == rules.len());
        if !same_len {
            self.rebuild_rule_inputs(window, cx);
            return;
        }
        let light = !cx.theme().mode.is_dark();
        let inputs = self
            .settings_panel
            .as_ref()
            .map(|d| d.rule_inputs.clone())
            .unwrap_or_default();
        for (i, (re, fl)) in inputs.into_iter().enumerate() {
            let Some(rule) = rules.get(i) else { break };
            let regex = rule.regex.clone();
            let flags = rule.flags.clone();
            re.update(cx, |s, cx| {
                if s.value() != regex {
                    s.set_value(regex.clone(), window, cx);
                }
                s.set_decorations(crate::code_input::decorations(&regex, light), cx);
            });
            fl.update(cx, |s, cx| {
                if s.value() != flags {
                    s.set_value(flags.clone(), window, cx);
                }
            });
        }
        if let Some(d) = self.settings_panel.as_mut() {
            d.deco_light = Some(light);
        }
    }

    /// 从搜索结果跳转到「分析配置」页并选中该配置
    fn open_config_page(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.settings_panel.as_ref().map(|d| d.search.clone());
        if let Some(d) = self.settings_panel.as_mut() {
            d.tab = 2;
            if i < self.settings.configs.len() {
                d.sel_config = i;
                d.cfg_unlock = false;
            }
        }
        if let Some(s) = search {
            s.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        self.rebuild_rule_inputs(window, cx);
    }

    fn add_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_rule_inputs(cx);
        self.settings.configs.push(crate::level::LevelConfig {
            id: format!(
                "custom-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|t| t.as_millis())
                    .unwrap_or(0)
            ),
            name: i18n::t("configs.new_short").into(),
            enabled: true,
            built_in: false,
            rules: vec![crate::level::Rule {
                auto: true,
                level: crate::level::INFO,
                regex: String::new(),
                flags: "i".into(),
            }],
        });
        let idx = self.settings.configs.len() - 1;
        if let Some(d) = self.settings_panel.as_mut() {
            d.sel_config = idx;
        }
        self.rebuild_rule_inputs(window, cx);
        self.settings_apply(true, cx);
    }

    fn del_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_rule_inputs(cx);
        let idx = self.settings_panel.as_ref().map(|d| d.sel_config);
        let Some(idx) = idx else { return };
        let del_ok = self.settings.configs.get(idx).is_some_and(|c| !c.built_in);
        if del_ok {
            self.settings.configs.remove(idx);
            if let Some(d) = self.settings_panel.as_mut() {
                if d.sel_config >= self.settings.configs.len() {
                    d.sel_config = self.settings.configs.len().saturating_sub(1);
                }
            }
        }
        self.rebuild_rule_inputs(window, cx);
        if del_ok {
            self.settings_apply(true, cx);
        }
    }

    fn toggle_config_enabled(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(c) = self.settings.configs.get_mut(i) {
            c.enabled = !c.enabled;
        }
        self.settings_apply(true, cx);
    }

    /// 把某个内置配置恢复为出厂规则(规则被改坏时的一键修复)
    fn reset_builtin_config(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_rule_inputs(cx);
        let Some(id) = self.settings.configs.get(i).map(|c| c.id.clone()) else {
            return;
        };
        let Some(def) = crate::level::builtin_configs()
            .into_iter()
            .find(|c| c.id == id)
        else {
            return;
        };
        if let Some(c) = self.settings.configs.get_mut(i) {
            *c = def;
        }
        self.rebuild_rule_inputs(window, cx);
        self.settings_apply(true, cx);
        if let Some(d) = self.settings_panel.as_mut() {
            // 恢复出厂规则后重新上锁,回到只读浏览
            d.cfg_unlock = false;
            d.cfg_notice = Some(i18n::t("configs.restored_one").into());
        }
        cx.notify();
    }

    /// 把所有内置配置恢复为出厂规则(自定义配置不动)
    fn reset_all_builtins(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_rule_inputs(cx);
        let defaults = crate::level::builtin_configs();
        for c in self.settings.configs.iter_mut() {
            if let Some(def) = defaults.iter().find(|d| d.id == c.id) {
                *c = def.clone();
            }
        }
        self.rebuild_rule_inputs(window, cx);
        self.settings_apply(true, cx);
        if let Some(d) = self.settings_panel.as_mut() {
            d.cfg_notice = Some(i18n::t("configs.builtin_restored_all").into());
        }
        cx.notify();
    }

    fn add_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.config_readonly() {
            return;
        }
        self.sync_rule_inputs(cx);
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let idx = d.sel_config;
        if let Some(c) = self.settings.configs.get_mut(idx) {
            c.rules.push(crate::level::Rule {
                auto: true,
                level: crate::level::INFO,
                regex: String::new(),
                flags: "i".into(),
            });
        }
        self.rebuild_rule_inputs(window, cx);
        self.settings_apply(true, cx);
    }

    fn del_rule(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.config_readonly() {
            return;
        }
        self.sync_rule_inputs(cx);
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let idx = d.sel_config;
        if let Some(c) = self.settings.configs.get_mut(idx) {
            if c.rules.len() > 1 {
                c.rules.remove(i);
            }
        }
        self.rebuild_rule_inputs(window, cx);
        self.settings_apply(true, cx);
    }

    fn toggle_rule_auto(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.config_readonly() {
            return;
        }
        self.sync_rule_inputs(cx);
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let idx = d.sel_config;
        if let Some(r) = self
            .settings
            .configs
            .get_mut(idx)
            .and_then(|c| c.rules.get_mut(i))
        {
            r.auto = !r.auto;
        }
        self.settings_apply(true, cx);
    }

    fn cycle_rule_level(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.config_readonly() {
            return;
        }
        self.sync_rule_inputs(cx);
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let idx = d.sel_config;
        if let Some(r) = self
            .settings
            .configs
            .get_mut(idx)
            .and_then(|c| c.rules.get_mut(i))
        {
            r.auto = false;
            r.level = (r.level + 1) % LEVEL_COUNT as u8;
        }
        self.settings_apply(true, cx);
    }

    /// 最近打开:先尝试钥匙串回填;凭据缺失则弹表单预填,避免直接认证失败
    fn welcome_open_recent(
        &mut self,
        ti: usize,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 打开记录属于其他操作,顺手退出清除确认态
        self.recents_confirm = false;
        let Some(r) = self.settings.recents.get(idx).cloned() else {
            return;
        };
        let mut params = r.params;
        if let Some(msg) = self.fill_from_keyring(&mut params) {
            self.prompt_credentials(params, msg, window, cx);
            return;
        }
        if self.tabs.get(ti).is_some_and(|t| t.welcome) {
            self.open_in_tab(ti, params, false);
        } else {
            self.open_source(params, false);
        }
    }

    /// 凭据缺失/钥匙串不可用:打开对应表单并预填非敏感字段
    fn prompt_credentials(
        &mut self,
        params: SourceParams,
        msg: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match params {
            SourceParams::Ssh {
                host,
                port,
                user,
                path,
                ..
            } => {
                let mut form = SshForm::new(window, cx);
                form.host.update(cx, |s, cx| s.set_value(host, window, cx));
                form.port
                    .update(cx, |s, cx| s.set_value(port.to_string(), window, cx));
                form.user.update(cx, |s, cx| s.set_value(user, window, cx));
                form.path.update(cx, |s, cx| s.set_value(path, window, cx));
                form.remember = secrets::available();
                form.err = Some(msg);
                self.dialog = Dialog::Ssh(form);
            }
            SourceParams::Http {
                url,
                auth,
                user,
                header_name,
                ..
            } => {
                let mut form = UrlForm::new(window, cx);
                form.url.update(cx, |s, cx| s.set_value(url, window, cx));
                form.auth = match auth {
                    crate::source::HttpAuthKind::None => 0,
                    crate::source::HttpAuthKind::Basic => 1,
                    crate::source::HttpAuthKind::Bearer => 2,
                    crate::source::HttpAuthKind::ApiKey => 3,
                    crate::source::HttpAuthKind::Header => 4,
                };
                if let Some(u) = user {
                    form.user.update(cx, |s, cx| s.set_value(u, window, cx));
                }
                if let Some(n) = header_name {
                    form.header_name
                        .update(cx, |s, cx| s.set_value(n, window, cx));
                }
                form.remember = secrets::available();
                form.err = Some(msg);
                self.dialog = Dialog::Url(form);
            }
            SourceParams::Command {
                target: ExecTarget::Ssh {
                    host, port, user, ..
                },
                argv,
                preset,
                values,
                raw,
                ..
            } => {
                let mut form = SshForm::new(window, cx);
                form.host.update(cx, |s, cx| s.set_value(host, window, cx));
                form.port
                    .update(cx, |s, cx| s.set_value(port.to_string(), window, cx));
                form.user.update(cx, |s, cx| s.set_value(user, window, cx));
                form.mode = 1;
                fill_command_fields(
                    &self.settings.command_presets,
                    false,
                    &preset,
                    &values,
                    &argv,
                    raw,
                    &mut form.cmd,
                    window,
                    cx,
                );
                form.remember = secrets::available();
                form.err = Some(msg);
                self.dialog = Dialog::Ssh(form);
            }
            SourceParams::Command { .. } | SourceParams::Local { .. } => return,
        }
        self.cred_notice = None;
    }

    /// 打开 SSH 连接配置弹窗
    fn open_ssh_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = SshForm::new(window, cx);
        self.dialog = Dialog::Ssh(form);
    }

    /// 打开 URL 输入弹窗
    fn open_url_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = UrlForm::new(window, cx);
        self.dialog = Dialog::Url(form);
    }

    /// 打开本机命令弹窗
    fn open_cmd_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = Dialog::Cmd(CmdForm {
            fields: CmdFields::new(window, cx),
            err: None,
        });
    }

    fn cmd_set_preset(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let presets = self.settings.command_presets.clone();
        if let Dialog::Cmd(f) = &mut self.dialog {
            f.fields.preset = i;
            f.fields.inputs = build_preset_inputs(&presets, true, i, window, cx);
            f.err = None;
        }
    }

    fn cmd_toggle_restart(&mut self) {
        if let Dialog::Cmd(f) = &mut self.dialog {
            f.fields.restart = !f.fields.restart;
        }
    }

    /// 提交本机命令:与打开文件一致,优先占用当前空白标签页
    fn cmd_submit(&mut self, cx: &mut Context<Self>) {
        let res = {
            let Dialog::Cmd(f) = &self.dialog else {
                return;
            };
            command_params(
                &f.fields,
                &self.settings.command_presets,
                true,
                ExecTarget::Local,
                cx,
            )
        };
        let params = match res {
            Ok(p) => p,
            Err(e) => {
                if let Dialog::Cmd(f) = &mut self.dialog {
                    f.err = Some(e);
                }
                return;
            }
        };
        let target = self.active;
        if self.tabs.get(target).is_some_and(|t| t.welcome) {
            self.open_in_tab(target, params, false);
            self.dialog = Dialog::None;
        } else {
            self.open_source(params, false);
        }
    }

    fn ssh_set_mode(&mut self, i: usize) {
        if let Dialog::Ssh(f) = &mut self.dialog {
            f.mode = i;
            f.err = None;
        }
    }

    fn ssh_cmd_set_preset(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let presets = self.settings.command_presets.clone();
        if let Dialog::Ssh(f) = &mut self.dialog {
            f.cmd.preset = i;
            f.cmd.inputs = build_preset_inputs(&presets, false, i, window, cx);
            f.err = None;
        }
    }

    fn ssh_cmd_toggle_restart(&mut self) {
        if let Dialog::Ssh(f) = &mut self.dialog {
            f.cmd.restart = !f.cmd.restart;
        }
    }

    /// 认证失败:弹出对应的连接表单,预填目标与错误提示(凭据留空待重填),
    /// 重新提交后在原标签页打开(不再新开标签)
    fn open_retry_dialog(
        &mut self,
        tab_id: u64,
        params: &SourceParams,
        msg: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let err = i18n::tf("error.reenter_retry", &[("msg", msg)]);
        match params {
            SourceParams::Ssh {
                host,
                port,
                user,
                path,
                ..
            } => {
                let mut f = SshForm::new(window, cx);
                f.retry = Some(tab_id);
                f.host
                    .update(cx, |s, cx| s.set_value(host.clone(), window, cx));
                f.port
                    .update(cx, |s, cx| s.set_value(port.to_string(), window, cx));
                f.user
                    .update(cx, |s, cx| s.set_value(user.clone(), window, cx));
                f.path
                    .update(cx, |s, cx| s.set_value(path.clone(), window, cx));
                f.err = Some(err);
                self.dialog = Dialog::Ssh(f);
            }
            SourceParams::Http {
                url,
                auth,
                user,
                header_name,
                ..
            } => {
                let mut f = UrlForm::new(window, cx);
                f.retry = Some(tab_id);
                f.url
                    .update(cx, |s, cx| s.set_value(url.clone(), window, cx));
                f.auth = match auth {
                    crate::source::HttpAuthKind::None => 0,
                    crate::source::HttpAuthKind::Basic => 1,
                    crate::source::HttpAuthKind::Bearer => 2,
                    crate::source::HttpAuthKind::ApiKey => 3,
                    crate::source::HttpAuthKind::Header => 4,
                };
                if let Some(u) = user.as_deref().filter(|u| !u.is_empty()) {
                    f.user
                        .update(cx, |s, cx| s.set_value(u.to_string(), window, cx));
                }
                if let Some(h) = header_name.as_deref().filter(|h| !h.is_empty()) {
                    f.header_name
                        .update(cx, |s, cx| s.set_value(h.to_string(), window, cx));
                }
                f.err = Some(err);
                self.dialog = Dialog::Url(f);
            }
            SourceParams::Command {
                target: ExecTarget::Ssh {
                    host, port, user, ..
                },
                argv,
                preset,
                values,
                raw,
                ..
            } => {
                let mut f = SshForm::new(window, cx);
                f.retry = Some(tab_id);
                f.host
                    .update(cx, |s, cx| s.set_value(host.clone(), window, cx));
                f.port
                    .update(cx, |s, cx| s.set_value(port.to_string(), window, cx));
                f.user
                    .update(cx, |s, cx| s.set_value(user.clone(), window, cx));
                f.mode = 1;
                fill_command_fields(
                    &self.settings.command_presets,
                    false,
                    preset,
                    values,
                    argv,
                    *raw,
                    &mut f.cmd,
                    window,
                    cx,
                );
                f.err = Some(err);
                self.dialog = Dialog::Ssh(f);
            }
            SourceParams::Command { .. } | SourceParams::Local { .. } => {}
        }
    }

    /// 处理子线程上报的认证失败:无人正在填表单时自动弹出连接界面
    fn maybe_open_auth_retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((tab_id, params, msg)) = self.auth_retry.take() else {
            return;
        };
        if !matches!(self.dialog, Dialog::None) {
            return;
        }
        self.open_retry_dialog(tab_id, &params, msg, window, cx);
        cx.notify();
    }

    /// 已保存的 SSH 服务器(来自最近打开,按 用户@主机:端口 去重;只关心服务器本身,不带路径)
    /// 可选的 SSH 服务器:设置页保存的主机在前,最近打开里未保存的补在后面
    /// (按 主机+端口+用户名 去重;不包含远程路径)
    fn saved_ssh_servers(&self) -> Vec<(String, String, u16, String)> {
        let mut out: Vec<(String, String, u16, String)> = Vec::new();
        for h in &self.settings.ssh_hosts {
            if out
                .iter()
                .any(|(_, host, port, user)| host == &h.host && *port == h.port && user == &h.user)
            {
                continue;
            }
            let addr = format!("{}@{}:{}", h.user, h.host, h.port);
            let name = h.name.trim();
            let label = if name.is_empty() {
                addr
            } else {
                format!("{name} · {addr}")
            };
            out.push((label, h.host.clone(), h.port, h.user.clone()));
        }
        for r in &self.settings.recents {
            if let SourceParams::Ssh {
                host, port, user, ..
            } = &r.params
            {
                if out
                    .iter()
                    .any(|(_, h, p, u)| h == host && p == port && u == user)
                {
                    continue;
                }
                out.push((
                    format!("{user}@{host}:{port}"),
                    host.clone(),
                    *port,
                    user.clone(),
                ));
                if out.len() >= 12 {
                    break;
                }
            }
        }
        out
    }

    // ---- 设置页:已保存 SSH 主机管理 ----

    fn host_begin_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.settings_panel.as_mut() {
            d.host_edit = Some(HostEdit::New);
            d.host_err = None;
            d.host_notice = None;
            d.h_name
                .update(cx, |s, cx| s.set_value(String::new(), window, cx));
            d.h_host
                .update(cx, |s, cx| s.set_value(String::new(), window, cx));
            d.h_port.update(cx, |s, cx| s.set_value("22", window, cx));
            d.h_user
                .update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
    }

    fn host_begin_edit(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(h) = self.settings.ssh_hosts.get(i).cloned() else {
            return;
        };
        if let Some(d) = self.settings_panel.as_mut() {
            d.host_edit = Some(HostEdit::Existing(i));
            d.host_err = None;
            d.host_notice = None;
            d.h_name.update(cx, |s, cx| s.set_value(h.name, window, cx));
            d.h_host.update(cx, |s, cx| s.set_value(h.host, window, cx));
            d.h_port
                .update(cx, |s, cx| s.set_value(h.port.to_string(), window, cx));
            d.h_user.update(cx, |s, cx| s.set_value(h.user, window, cx));
        }
    }

    fn host_edit_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = self.settings_panel.as_mut() {
            d.host_edit = None;
            d.host_err = None;
        }
        cx.notify();
    }

    /// 保存当前编辑项:新增则追加,编辑则覆盖原位置;只保存连接目标,不含凭据
    fn host_save(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.settings_panel.as_ref() else {
            return;
        };
        let name = d.h_name.read(cx).value().trim().to_string();
        let host = d.h_host.read(cx).value().trim().to_string();
        let user = d.h_user.read(cx).value().trim().to_string();
        let port_txt = d.h_port.read(cx).value().trim().to_string();
        let edit = d.host_edit;
        let port = port_txt.parse::<u16>().ok().filter(|p| *p > 0);
        let err = if host.is_empty() {
            Some(i18n::t("validation.host_required").to_string())
        } else if user.is_empty() {
            Some(i18n::t("validation.username_required").to_string())
        } else if port.is_none() {
            Some(i18n::t("validation.port_range").to_string())
        } else {
            None
        };
        if let Some(e) = err {
            if let Some(d) = self.settings_panel.as_mut() {
                d.host_err = Some(e);
            }
            return;
        }
        let entry = crate::store::SavedHost {
            name,
            host,
            port: port.unwrap_or(22),
            user,
        };
        match edit {
            Some(HostEdit::Existing(i)) if i < self.settings.ssh_hosts.len() => {
                self.settings.ssh_hosts[i] = entry;
            }
            _ => self.settings.ssh_hosts.push(entry),
        }
        crate::store::save(&self.settings);
        if let Some(d) = self.settings_panel.as_mut() {
            d.host_edit = None;
            d.host_err = None;
        }
        cx.notify();
    }

    fn host_delete(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.settings.ssh_hosts.len() {
            return;
        }
        let gone = self.settings.ssh_hosts.remove(i);
        // 同时清理最近打开里的同一主机,否则下拉列表会因「最近打开」再次出现该服务器
        self.settings.recents.retain(|r| match &r.params {
            SourceParams::Ssh {
                host, port, user, ..
            } => !(host == &gone.host && *port == gone.port && user == &gone.user),
            _ => true,
        });
        crate::store::save(&self.settings);
        if let Some(d) = self.settings_panel.as_mut() {
            d.host_edit = match d.host_edit {
                Some(HostEdit::Existing(j)) if j == i => None,
                Some(HostEdit::Existing(j)) if j > i => Some(HostEdit::Existing(j - 1)),
                other => other,
            };
        }
        cx.notify();
    }

    /// 从 `~/.ssh/config` 导入具体主机(通配/Match 块跳过;与现有条目去重)
    fn host_import_ssh_config(&mut self, cx: &mut Context<Self>) {
        let notice = match crate::ssh_config::load() {
            Ok(list) => {
                let mut added = 0usize;
                let mut dup = 0usize;
                for h in list {
                    let exists = self.settings.ssh_hosts.iter().any(|e| {
                        e.host.eq_ignore_ascii_case(&h.host) && e.port == h.port && e.user == h.user
                    });
                    if exists {
                        dup += 1;
                        continue;
                    }
                    self.settings.ssh_hosts.push(h);
                    added += 1;
                }
                if added > 0 {
                    crate::store::save(&self.settings);
                }
                match (added, dup) {
                    (0, 0) => i18n::t("ssh.import.empty").to_string(),
                    (0, n) => i18n::tf("ssh.import.duplicates", &[("n", n.to_string())]),
                    (n, 0) => i18n::tf("ssh.import.done", &[("n", n.to_string())]),
                    (n, d) => i18n::tf(
                        "ssh.import.done_with_dups",
                        &[("n", n.to_string()), ("d", d.to_string())],
                    ),
                }
            }
            Err(e) => e,
        };
        if let Some(d) = self.settings_panel.as_mut() {
            d.host_notice = Some(notice);
            d.host_err = None;
        }
        cx.notify();
    }

    /// 把最近打开里的 SSH 服务器批量加入已保存列表(已存在的主机跳过)
    fn host_import_recents(&mut self, cx: &mut Context<Self>) {
        let mut added = 0usize;
        for r in self.settings.recents.clone() {
            if let SourceParams::Ssh {
                host, port, user, ..
            } = &r.params
            {
                if self
                    .settings
                    .ssh_hosts
                    .iter()
                    .any(|h| &h.host == host && h.port == *port && &h.user == user)
                {
                    continue;
                }
                self.settings.ssh_hosts.push(crate::store::SavedHost {
                    name: String::new(),
                    host: host.clone(),
                    port: *port,
                    user: user.clone(),
                });
                added += 1;
            }
        }
        if added > 0 {
            crate::store::save(&self.settings);
        }
        cx.notify();
    }

    fn ssh_set_auth(&mut self, password: bool) {
        if let Dialog::Ssh(f) = &mut self.dialog {
            f.auth_password = password;
        }
    }

    fn ssh_toggle_remember(&mut self) {
        if let Dialog::Ssh(f) = &mut self.dialog {
            f.remember = !f.remember;
        }
    }

    /// 从服务器联想列表选中一项:回填主机/端口/用户名(远程路径保持用户当前输入)
    fn ssh_pick_server(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, host, port, user)) = self.saved_ssh_servers().get(idx).cloned() else {
            return;
        };
        // 钥匙串中已有该服务器的密码时直接回填
        let saved = secrets::load(&secrets::ssh_key(&user, &host, port))
            .ok()
            .flatten();
        if let Dialog::Ssh(f) = &mut self.dialog {
            if let Some(sec) = saved {
                if let Some(pw) = sec.password {
                    f.remember = true;
                    f.password.update(cx, |s, cx| s.set_value(pw, window, cx));
                }
            }
            f.host.update(cx, |s, cx| s.set_value(host, window, cx));
            f.port
                .update(cx, |s, cx| s.set_value(port.to_string(), window, cx));
            f.user.update(cx, |s, cx| s.set_value(user, window, cx));
            // 远程路径不自动回填:焦点落到路径框,由用户自行填写(联想列表随主机框失焦收起)
            f.path.update(cx, |s, cx| s.focus(window, cx));
            f.err = None;
        }
    }

    /// 提交 SSH 表单:在当前空白页打开,否则新开标签页
    fn ssh_submit(&mut self, cx: &mut Context<Self>) {
        let (
            host,
            port_txt,
            user,
            password,
            path,
            auth_password,
            remember,
            retry,
            mode,
            cmd_fields,
        ) = {
            let Dialog::Ssh(f) = &self.dialog else {
                return;
            };
            let get = |e: &Entity<InputState>| e.read(cx).value().to_string();
            (
                get(&f.host),
                get(&f.port),
                get(&f.user),
                get(&f.password),
                get(&f.path),
                f.auth_password,
                f.remember,
                f.retry,
                f.mode,
                f.cmd.clone(),
            )
        };
        let err = if mode == 0 {
            validate_ssh(&host, &user, &path, &port_txt)
        } else {
            validate_ssh_conn(&host, &user, &port_txt)
        };
        if let Some(e) = err {
            if let Dialog::Ssh(f) = &mut self.dialog {
                f.err = Some(e);
            }
            return;
        }
        let port = port_txt.trim().parse().unwrap_or(22);
        let auth = if auth_password {
            SshAuth::Password(password)
        } else {
            SshAuth::Agent
        };
        let params = if mode == 0 {
            SourceParams::Ssh {
                host: host.trim().to_string(),
                port,
                user: user.trim().to_string(),
                auth,
                path,
            }
        } else {
            let target = ExecTarget::Ssh {
                host: host.trim().to_string(),
                port,
                user: user.trim().to_string(),
                auth,
            };
            match command_params(
                &cmd_fields,
                &self.settings.command_presets,
                false,
                target,
                cx,
            ) {
                Ok(p) => p,
                Err(e) => {
                    if let Dialog::Ssh(f) = &mut self.dialog {
                        f.err = Some(e);
                    }
                    return;
                }
            }
        };
        // 认证失败重试:在原标签页重新打开,不再新开标签
        if let Some(id) = retry {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                self.open_in_tab(i, params, remember);
                self.dialog = Dialog::None;
                return;
            }
        }
        let target = self.active;
        if self.tabs.get(target).is_some_and(|t| t.welcome) {
            self.open_in_tab(target, params, remember);
            self.dialog = Dialog::None;
        } else {
            self.open_source(params, remember);
        }
    }

    fn url_set_auth(&mut self, i: usize) {
        if let Dialog::Url(f) = &mut self.dialog {
            f.auth = i;
            f.err = None;
        }
    }

    fn url_set_remember(&mut self, on: bool) {
        if let Dialog::Url(f) = &mut self.dialog {
            f.remember = on;
            f.err = None;
        }
    }

    /// 删除一条已保存的 URL 凭据:先删钥匙串条目,再把对应最近打开标记为未保存
    fn url_cred_delete(&mut self, key: String, cx: &mut Context<Self>) {
        let notice = match secrets::delete(&key) {
            Ok(()) => i18n::t("http.creds.deleted").to_string(),
            Err(e) => i18n::tf("error.delete_failed", &[("e", e.to_string())]),
        };
        let mut changed = false;
        for r in &mut self.settings.recents {
            if r.remembered && secrets::key_of(&r.params).as_deref() == Some(key.as_str()) {
                r.remembered = false;
                changed = true;
            }
        }
        if changed {
            store::save(&self.settings);
        }
        if let Some(d) = self.settings_panel.as_mut() {
            d.url_cred_notice = Some(notice);
        }
        cx.notify();
    }

    /// 提交 URL 表单:网址 + 认证(无 / Basic / Bearer / API Key / 自定义头)
    fn url_submit(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Url(f)) = Some(&self.dialog) else {
            return;
        };
        let get = |e: &Entity<InputState>| e.read(cx).value().to_string();
        let url = get(&f.url);
        let auth = f.auth;
        let remember = f.remember;
        let retry = f.retry;
        let user = get(&f.user).trim().to_string();
        let password = get(&f.password);
        let token = get(&f.token).trim().to_string();
        let header_name = get(&f.header_name).trim().to_string();
        let header_value = get(&f.header_value);

        let u = url.trim();
        let err = match validate_url_addr(u) {
            Some(e) => Some(e),
            None => match auth {
                1 if user.is_empty() => {
                    Some(i18n::t("validation.basic_needs_username").to_string())
                }
                2 if token.is_empty() => Some(i18n::t("validation.bearer_needs_token").to_string()),
                3 | 4 => {
                    let name = if header_name.is_empty() && auth == 3 {
                        "X-API-Key"
                    } else {
                        header_name.as_str()
                    };
                    if name.is_empty() {
                        Some(i18n::t("validation.header_name_required").to_string())
                    } else if header_value.is_empty() {
                        Some(i18n::t("validation.header_value_required").to_string())
                    } else if matches!(
                        name.to_ascii_lowercase().as_str(),
                        "host" | "content-length" | "transfer-encoding" | "connection"
                    ) {
                        Some(i18n::tf(
                            "error.header_not_allowed",
                            &[("name", name.to_string())],
                        ))
                    } else if !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                    {
                        Some(i18n::t("validation.header_name_charset").to_string())
                    } else {
                        None
                    }
                }
                _ => None,
            },
        };
        if let Some(e) = err {
            if let Dialog::Url(f) = &mut self.dialog {
                f.err = Some(e);
            }
            return;
        }

        let (kind, user, password, token, header_name, header_value) = match auth {
            1 => (
                crate::source::HttpAuthKind::Basic,
                Some(user).filter(|s| !s.is_empty()),
                Some(password).filter(|s| !s.is_empty()),
                None,
                None,
                None,
            ),
            2 => (
                crate::source::HttpAuthKind::Bearer,
                None,
                None,
                Some(token),
                None,
                None,
            ),
            3 => (
                crate::source::HttpAuthKind::ApiKey,
                None,
                None,
                None,
                Some(if header_name.is_empty() {
                    "X-API-Key".to_string()
                } else {
                    header_name
                }),
                Some(header_value),
            ),
            4 => (
                crate::source::HttpAuthKind::Header,
                None,
                None,
                None,
                Some(header_name),
                Some(header_value),
            ),
            _ => (
                crate::source::HttpAuthKind::None,
                None,
                None,
                None,
                None,
                None,
            ),
        };
        let params = SourceParams::Http {
            url: u.to_string(),
            auth: kind,
            user,
            password,
            token,
            header_name,
            header_value,
        };
        // 认证失败重试:在原标签页重新打开,不再新开标签
        if let Some(id) = retry {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                self.open_in_tab(i, params, remember);
                self.dialog = Dialog::None;
                return;
            }
        }
        let target = self.active;
        if self.tabs.get(target).is_some_and(|t| t.welcome) {
            self.open_in_tab(target, params, remember);
            self.dialog = Dialog::None;
        } else {
            self.open_source(params, remember);
        }
    }

    /// 从最近打开里移除一条;URL 记录按设置同时清除钥匙串凭据
    fn welcome_remove_recent(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.settings.recents.len() {
            return;
        }
        let r = self.settings.recents.remove(idx);
        self.purge_recent_cred(&r);
        self.recents_confirm = false;
        store::save(&self.settings);
        cx.notify();
    }

    /// 退出「清除最近打开」的二次确认
    fn welcome_cancel_clear(&mut self, cx: &mut Context<Self>) {
        self.recents_confirm = false;
        cx.notify();
    }

    /// 清空最近打开:首次点击进入待确认,再次点击执行
    fn welcome_clear_recents(&mut self, cx: &mut Context<Self>) {
        if !self.recents_confirm {
            self.recents_confirm = true;
            cx.notify();
            return;
        }
        let all: Vec<Recent> = std::mem::take(&mut self.settings.recents);
        for r in &all {
            self.purge_recent_cred(r);
        }
        self.recents_confirm = false;
        store::save(&self.settings);
        cx.notify();
    }

    /// 删除某条 URL 最近打开时,按设置清理其钥匙串凭据(SSH 凭据不在此列)
    fn purge_recent_cred(&mut self, r: &Recent) {
        if !self.settings.recents_purge_creds || !r.remembered {
            return;
        }
        if !matches!(r.params, SourceParams::Http { .. }) {
            return;
        }
        if let Some(k) = secrets::key_of(&r.params) {
            if let Err(e) = secrets::delete(&k) {
                self.cred_notice = Some(i18n::tf(
                    "error.creds_delete_failed",
                    &[("e", e.to_string())],
                ));
            }
        }
    }

    /// 「删除 URL 记录时清除凭据」开关
    fn toggle_recents_purge(&mut self, cx: &mut Context<Self>) {
        self.settings.recents_purge_creds = !self.settings.recents_purge_creds;
        self.settings_apply(false, cx);
    }

    /// 导出分析配置为 JSON(与导入同一格式:配置数组)
    fn export_configs(&mut self, cx: &mut Context<Self>) {
        let json = serde_json::to_string_pretty(&self.settings.configs).unwrap_or_default();
        let dir = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&dir, Some("logview-configs.json"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let msg = match std::fs::write(&path, &json) {
                Ok(()) => i18n::tf("configs.exported", &[("path", path.display().to_string())]),
                Err(e) => i18n::tf("configs.export_failed", &[("e", e.to_string())]),
            };
            this.update(cx, |v, cx| {
                if let Some(d) = v.settings_panel.as_mut() {
                    d.cfg_notice = Some(msg);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 导入分析配置 JSON:整体替换配置列表,并立即重分析全部标签
    fn import_configs(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.first() else { return };
            let (cfg, msg) = match std::fs::read_to_string(p) {
                Ok(txt) => match serde_json::from_str::<Vec<LevelConfig>>(&txt) {
                    Ok(list) if !list.is_empty() => {
                        let n = list.len();
                        (
                            Some(list),
                            i18n::tf("configs.imported", &[("n", n.to_string())]),
                        )
                    }
                    _ => (None, i18n::t("configs.import_invalid").to_string()),
                },
                Err(e) => (
                    None,
                    i18n::tf("configs.import_failed", &[("e", e.to_string())]),
                ),
            };
            this.update(cx, |v, cx| {
                if let Some(cfgs) = cfg {
                    v.settings.configs = cfgs;
                    v.settings_apply(true, cx);
                }
                if let Some(d) = v.settings_panel.as_mut() {
                    d.cfg_notice = Some(msg);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 切换设置分类(清空搜索,回到分类浏览)
    fn set_settings_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.settings_panel.as_ref().map(|d| d.search.clone());
        if let Some(d) = self.settings_panel.as_mut() {
            d.tab = i;
        }
        if let Some(s) = search {
            s.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        cx.notify();
    }

    /// 显示行号:立即生效
    fn toggle_show_line_numbers(&mut self, cx: &mut Context<Self>) {
        self.settings.show_line_numbers = !self.settings.show_line_numbers;
        self.settings_apply(false, cx);
    }

    /// 显示时间戳:立即生效(列宽变化会重算自动换行宽度,settings_apply 已清行缓存)
    fn toggle_show_timestamps(&mut self, cx: &mut Context<Self>) {
        self.settings.show_timestamps = !self.settings.show_timestamps;
        self.settings_apply(false, cx);
    }

    /// 切换界面语言:同步 gpui-kit 组件文案、窗口标题与已存在输入框的占位符
    fn set_language(&mut self, lang: i18n::Lang, window: &mut Window, cx: &mut Context<Self>) {
        if i18n::lang() == lang {
            return;
        }
        self.settings.language = lang.code().to_string();
        i18n::set(lang);
        gpui_kit::component::set_locale(lang.locale());
        window.set_window_title(i18n::t("app.name"));
        self.refresh_placeholders(window, cx);
        self.settings_apply(false, cx);
        cx.notify();
    }

    /// 刷新仍存活的输入框占位符:这些文本在创建 InputState 时写入,之后不会自动跟随语言
    fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        set_input_placeholder(
            &self.search_input,
            i18n::t("search.placeholder"),
            window,
            cx,
        );
        if let Some(d) = self.settings_panel.as_ref() {
            set_input_placeholder(
                &d.search,
                i18n::t("settings.search.placeholder"),
                window,
                cx,
            );
        }
        match &self.dialog {
            Dialog::Ssh(f) => {
                set_input_placeholder(&f.host, i18n::t("ssh.host.placeholder"), window, cx);
                set_input_placeholder(&f.user, i18n::t("ssh.username"), window, cx);
                set_input_placeholder(
                    &f.password,
                    i18n::t("ssh.password.agent_placeholder"),
                    window,
                    cx,
                );
                refresh_cmd_placeholders(&f.cmd, window, cx);
            }
            Dialog::Cmd(f) => refresh_cmd_placeholders(&f.fields, window, cx),
            Dialog::Url(f) => {
                set_input_placeholder(&f.user, i18n::t("ssh.username"), window, cx);
                set_input_placeholder(&f.password, i18n::t("ssh.password"), window, cx);
                set_input_placeholder(&f.header_value, i18n::t("http.header.value"), window, cx);
            }
            Dialog::None | Dialog::HostKey(_) => {}
        }
    }

    // ---- 弹层渲染 ----

    fn render_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.dialog {
            Dialog::None => None,
            Dialog::Ssh(_) => Some(self.render_ssh_dialog(window, cx)),
            Dialog::Url(_) => Some(self.render_url_dialog(cx)),
            Dialog::Cmd(_) => Some(self.render_cmd_dialog(cx)),
            Dialog::HostKey(_) => Some(self.render_host_key_dialog(cx)),
        }
    }

    /// SSH 连接配置弹窗
    fn render_ssh_dialog(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        let ui_font = self.ui_font(cx);
        let Dialog::Ssh(f) = &self.dialog else {
            return div().into_any_element();
        };
        let auth_password = f.auth_password;
        let remember = f.remember;
        let mode = f.mode;
        let query = f.host.read(cx).value().trim().to_lowercase();
        // 服务器输入框获得焦点即展开已保存服务器列表,失焦(含选中后焦点移走)收起
        let host_focus = f.host.read(cx).presentation().focus_handle().clone();
        let show_servers = host_focus.is_focused(window);
        let err = f.err.clone().map(|e| i18n::td(&e));
        let (host_e, port_e, user_e, pass_e, path_e) = (
            f.host.clone(),
            f.port.clone(),
            f.user.clone(),
            f.password.clone(),
            f.path.clone(),
        );
        let servers: Vec<(usize, String)> = if show_servers {
            self.saved_ssh_servers()
                .into_iter()
                .enumerate()
                .filter(|(_, (label, ..))| {
                    query.is_empty() || label.to_lowercase().contains(&query)
                })
                .map(|(i, (label, ..))| (i, label))
                .take(10)
                .collect()
        } else {
            Vec::new()
        };

        fn label(text: &'static str, pal: &'static Palette) -> AnyElement {
            div()
                .w(px(88.0))
                .flex_shrink_0()
                .text_right()
                .text_size(px(theme::scaled(12.0)))
                .text_color(theme::hex(pal.text2))
                .child(text)
                .into_any_element()
        }

        let mut server_rows: Vec<AnyElement> = Vec::new();
        for (orig, name) in servers {
            server_rows.push(
                div()
                    .id(SharedString::from(format!("ssh-srv-{orig}")))
                    .cursor(CursorStyle::PointingHand)
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(5.0))
                    .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                    .child(
                        div()
                            .text_size(px(theme::scaled(12.0)))
                            .text_color(theme::hex(pal.text1))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(name),
                    )
                    .on_click(
                        cx.listener(move |v, _e, window, cx| v.ssh_pick_server(orig, window, cx)),
                    )
                    .into_any_element(),
            );
        }
        if show_servers && server_rows.is_empty() {
            server_rows.push(
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_size(px(theme::scaled(11.5)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("ssh.hosts.no_match"))
                    .into_any_element(),
            );
        }
        let server_list = div()
            .id("ssh-servers")
            .ml(px(96.0))
            .max_h(px(170.0))
            .overflow_y_scroll()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme::hex(pal.border))
            .bg(theme::hex(pal.bg_elevated))
            .children(server_rows);

        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("source.server"), pal))
                .child(div().flex_1().child(Input::new(&host_e).small()))
                .into_any_element(),
        );
        if show_servers {
            rows.push(server_list.into_any_element());
        }
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("ssh.port"), pal))
                .child(div().w(px(90.0)).child(Input::new(&port_e).small()))
                .child(div().w(px(10.0)))
                .child(
                    div()
                        .w(px(64.0))
                        .text_right()
                        .text_size(px(theme::scaled(12.0)))
                        .text_color(theme::hex(pal.text2))
                        .child(i18n::t("ssh.username")),
                )
                .child(div().flex_1().child(Input::new(&user_e).small()))
                .into_any_element(),
        );
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("ssh.password"), pal))
                .child(
                    div().flex_1().child(
                        Input::new(&pass_e)
                            .small()
                            .content_type(InputContentType::Password)
                            .mask_toggle(),
                    ),
                )
                .into_any_element(),
        );
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("source.method"), pal))
                .child(segmented(
                    cx,
                    "ssh-mode",
                    &[i18n::t("source.file"), i18n::t("source.command")],
                    mode,
                    pal,
                    |v, i| v.ssh_set_mode(i),
                ))
                .into_any_element(),
        );
        if mode == 0 {
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(label(i18n::t("ssh.remote_path"), pal))
                    .child(div().flex_1().child(Input::new(&path_e).small()))
                    .into_any_element(),
            );
        } else {
            rows.extend(command_rows(
                cx,
                &self.settings.command_presets,
                false,
                &f.cmd,
                pal,
                i18n::t("ssh.command.shell_hint"),
                |v, i, w, cx| v.ssh_cmd_set_preset(i, w, cx),
                |v| v.ssh_cmd_toggle_restart(),
            ));
        }
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("http.auth"), pal))
                .child(segmented(
                    cx,
                    "ssh-auth",
                    &[i18n::t("ssh.password"), "Agent"],
                    if auth_password { 0 } else { 1 },
                    pal,
                    |v, i| v.ssh_set_auth(i == 0),
                ))
                .into_any_element(),
        );
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().w(px(88.0)).flex_shrink_0())
                .child(
                    switch_el("ssh-remember", remember, pal)
                        .on_click(cx.listener(|v, _e, _w, _cx| v.ssh_toggle_remember())),
                )
                .child(
                    div()
                        .text_size(px(theme::scaled(12.0)))
                        .text_color(if secrets::available() {
                            theme::hex(pal.text2)
                        } else {
                            theme::hex(pal.warning)
                        })
                        .child(if secrets::available() {
                            i18n::t("ssh.remember.keychain")
                        } else {
                            i18n::t("ssh.remember.memory")
                        }),
                )
                .into_any_element(),
        );
        if let Some(e) = err {
            rows.push(
                div()
                    .pl(px(96.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.error))
                    .child(e)
                    .into_any_element(),
            );
        }

        let header = dlg_header(
            i18n::t("ssh.connection"),
            pal,
            cx.listener(|v, _e, _w, _cx| v.close_dialog()),
        );
        let body = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(16.0))
            .py(px(14.0))
            .children(rows);
        let footer = div()
            .flex()
            .justify_end()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(theme::hex(pal.split))
            .child(
                dlg_btn("ssh-cancel", i18n::t("common.cancel"), false, pal)
                    .on_click(cx.listener(|v, _e, _w, _cx| v.close_dialog())),
            )
            .child(
                dlg_btn(
                    "ssh-ok",
                    if mode == 0 {
                        i18n::t("common.connect")
                    } else {
                        i18n::t("common.run")
                    },
                    true,
                    pal,
                )
                .on_click(cx.listener(|v, _e, _w, cx| v.ssh_submit(cx))),
            );
        dlg_shell("dlg-ssh", header, body, footer, pal, ui_font)
    }

    /// 本机命令弹窗
    fn render_cmd_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        let ui_font = self.ui_font(cx);
        let Dialog::Cmd(f) = &self.dialog else {
            return div().into_any_element();
        };
        let err = f.err.clone().map(|e| i18n::td(&e));
        let mut rows = command_rows(
            cx,
            &self.settings.command_presets,
            true,
            &f.fields,
            pal,
            i18n::t("command.local_shell_hint"),
            |v, i, w, cx| v.cmd_set_preset(i, w, cx),
            |v| v.cmd_toggle_restart(),
        );
        if let Some(e) = err {
            rows.push(
                div()
                    .pl(px(96.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.error))
                    .child(e)
                    .into_any_element(),
            );
        }

        let header = dlg_header(
            i18n::t("command.run"),
            pal,
            cx.listener(|v, _e, _w, _cx| v.close_dialog()),
        );
        let body = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(16.0))
            .py(px(14.0))
            .children(rows);
        let footer = div()
            .flex()
            .justify_end()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(theme::hex(pal.split))
            .child(
                dlg_btn("cmd-cancel", i18n::t("common.cancel"), false, pal)
                    .on_click(cx.listener(|v, _e, _w, _cx| v.close_dialog())),
            )
            .child(
                dlg_btn("cmd-ok", i18n::t("common.run"), true, pal)
                    .on_click(cx.listener(|v, _e, _w, cx| v.cmd_submit(cx))),
            );
        dlg_shell("dlg-cmd", header, body, footer, pal, ui_font)
    }

    /// 首次连接未知主机:展示指纹,用户可选择仅本次信任或写入 known_hosts
    fn render_host_key_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        let ui_font = self.ui_font(cx);
        let Dialog::HostKey(f) = &self.dialog else {
            return div().into_any_element();
        };
        let host = format!("{}:{}", f.info.host, f.info.port);
        let algo = f.info.algo.clone();
        let fp = f.info.fingerprint.clone();
        let field = |k: &'static str, v: String, mono: bool| {
            let mut val = div()
                .text_size(px(theme::scaled(12.5)))
                .text_color(theme::hex(pal.text1));
            if mono {
                val = val.font_family(theme::mono_family());
            }
            div()
                .flex()
                .gap(px(10.0))
                .items_start()
                .child(
                    div()
                        .w(px(52.0))
                        .flex_none()
                        .text_size(px(theme::scaled(12.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(k),
                )
                .child(val.child(v))
        };
        let body = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(16.0))
            .py(px(14.0))
            .max_w(px(560.0))
            .child(field(i18n::t("ssh.host"), host, false))
            .child(field(i18n::t("ssh.host_key.algorithm"), algo, false))
            .child(field(i18n::t("ssh.host_key.fingerprint"), fp, true))
            .child(
                div()
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("ssh.host_key.warning")),
            )
            .child(
                div()
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("ssh.host_key.mismatch_hint")),
            );
        let header = dlg_header(
            i18n::t("ssh.host_key.confirm"),
            pal,
            cx.listener(|v, _e, _w, _cx| v.host_key_respond(false, false)),
        );
        let footer = div()
            .flex()
            .justify_end()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(theme::hex(pal.split))
            .child(
                dlg_btn("hk-cancel", i18n::t("common.cancel"), false, pal)
                    .on_click(cx.listener(|v, _e, _w, _cx| v.host_key_respond(false, false))),
            )
            .child(
                dlg_btn("hk-once", i18n::t("ssh.trust.once"), false, pal)
                    .on_click(cx.listener(|v, _e, _w, _cx| v.host_key_respond(true, false))),
            )
            .child(
                dlg_btn("hk-remember", i18n::t("ssh.trust.remember"), true, pal)
                    .on_click(cx.listener(|v, _e, _w, _cx| v.host_key_respond(true, true))),
            );
        dlg_shell("dlg-hostkey", header, body, footer, pal, ui_font)
    }

    /// 回传主机密钥决定并关闭弹窗
    fn host_key_respond(&mut self, accept: bool, remember: bool) {
        let Dialog::HostKey(f) = &self.dialog else {
            return;
        };
        let tab_id = f.tab;
        let host = f.info.host.clone();
        let port = f.info.port;
        if let Some(t) = self.tabs.iter().find(|t| t.id == tab_id) {
            let _ = t.cmd_tx.send(Cmd::HostKeyDecision { accept, remember });
        }
        self.cred_notice = Some(if accept {
            if remember {
                i18n::tf(
                    "ssh.trusted.saved",
                    &[("host", host.clone()), ("port", port.to_string())],
                )
            } else {
                i18n::tf(
                    "ssh.trusted.once",
                    &[("host", host.clone()), ("port", port.to_string())],
                )
            }
        } else {
            i18n::tf(
                "ssh.trust.cancelled",
                &[("host", host.clone()), ("port", port.to_string())],
            )
        });
        self.dialog = Dialog::None;
        self.show_next_host_key();
    }

    /// 弹出台阶队列中的下一个未知主机确认
    fn show_next_host_key(&mut self) {
        if !matches!(self.dialog, Dialog::None) {
            return;
        }
        while let Some((tab, info)) = self.host_key_queue.first().cloned() {
            self.host_key_queue.remove(0);
            if self.tabs.iter().any(|t| t.id == tab) {
                self.dialog = Dialog::HostKey(HostKeyForm { tab, info });
                return;
            }
        }
    }

    /// URL(HTTP)输入弹窗
    fn render_url_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        let ui_font = self.ui_font(cx);
        let Dialog::Url(f) = &self.dialog else {
            return div().into_any_element();
        };
        let err = f.err.clone().map(|e| i18n::td(&e));
        let auth = f.auth;
        let remember = f.remember;
        let (url_e, user_e, pass_e, token_e, hname_e, hvalue_e) = (
            f.url.clone(),
            f.user.clone(),
            f.password.clone(),
            f.token.clone(),
            f.header_name.clone(),
            f.header_value.clone(),
        );

        fn label(text: &'static str, pal: &'static Palette) -> AnyElement {
            div()
                .w(px(88.0))
                .flex_shrink_0()
                .text_right()
                .text_size(px(theme::scaled(12.0)))
                .text_color(theme::hex(pal.text2))
                .child(text)
                .into_any_element()
        }
        fn secret(e: &Entity<InputState>) -> AnyElement {
            div()
                .flex_1()
                .child(
                    Input::new(e)
                        .small()
                        .content_type(InputContentType::Password)
                        .mask_toggle(),
                )
                .into_any_element()
        }

        let mut rows: Vec<AnyElement> = vec![
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label("URL", pal))
                .child(div().flex_1().child(Input::new(&url_e).small()))
                .into_any_element(),
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("http.auth"), pal))
                .child(segmented(
                    cx,
                    "url-auth",
                    &[
                        i18n::t("common.none"),
                        "Basic",
                        "Bearer",
                        "API Key",
                        i18n::t("http.header.custom_short"),
                    ],
                    auth,
                    pal,
                    |v, i| v.url_set_auth(i),
                ))
                .into_any_element(),
        ];

        match auth {
            1 => {
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(label(i18n::t("ssh.username"), pal))
                        .child(div().flex_1().child(Input::new(&user_e).small()))
                        .into_any_element(),
                );
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(label(i18n::t("ssh.password"), pal))
                        .child(secret(&pass_e))
                        .into_any_element(),
                );
            }
            2 => {
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(label("Token", pal))
                        .child(secret(&token_e))
                        .into_any_element(),
                );
                rows.push(
                    div()
                        .pl(px(96.0))
                        .text_size(px(theme::scaled(11.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(i18n::t("http.auth.bearer_hint"))
                        .into_any_element(),
                );
            }
            3 | 4 => {
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(label(i18n::t("http.header.name"), pal))
                        .child(div().flex_1().child(Input::new(&hname_e).small()))
                        .into_any_element(),
                );
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(label(i18n::t("http.header.value_short"), pal))
                        .child(secret(&hvalue_e))
                        .into_any_element(),
                );
                rows.push(
                    div()
                        .pl(px(96.0))
                        .text_size(px(theme::scaled(11.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(if auth == 3 {
                            i18n::t("http.auth.api_key_hint")
                        } else {
                            i18n::t("http.header.hint")
                        })
                        .into_any_element(),
                );
            }
            _ => {
                rows.push(
                    div()
                        .pl(px(96.0))
                        .text_size(px(theme::scaled(11.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(i18n::t("http.auth.none_hint"))
                        .into_any_element(),
                );
            }
        }
        rows.push(
            div()
                .pl(px(96.0))
                .text_size(px(theme::scaled(11.0)))
                .text_color(theme::hex(pal.text3))
                .child(i18n::t("http.auth.security_note"))
                .into_any_element(),
        );
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("http.credentials"), pal))
                .child(segmented(
                    cx,
                    "url-remember",
                    &[
                        i18n::t("http.save_credentials"),
                        i18n::t("http.history_only"),
                    ],
                    if remember { 0 } else { 1 },
                    pal,
                    |v, i| v.url_set_remember(i == 0),
                ))
                .into_any_element(),
        );
        rows.push(
            div()
                .pl(px(96.0))
                .text_size(px(theme::scaled(11.0)))
                .text_color(theme::hex(pal.text3))
                .child(if !remember {
                    i18n::t("http.history_only.desc")
                } else if secrets::available() {
                    i18n::t("http.save_credentials.desc")
                } else {
                    i18n::t("http.keychain_unavailable")
                })
                .into_any_element(),
        );
        if let Some(e) = err {
            rows.push(
                div()
                    .pl(px(96.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.error))
                    .child(e)
                    .into_any_element(),
            );
        }

        let header = dlg_header(
            i18n::t("dialog.open_url"),
            pal,
            cx.listener(|v, _e, _w, _cx| v.close_dialog()),
        );
        let body = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(16.0))
            .py(px(14.0))
            .children(rows);
        let footer = div()
            .flex()
            .justify_end()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(theme::hex(pal.split))
            .child(
                dlg_btn("url-cancel", i18n::t("common.cancel"), false, pal)
                    .on_click(cx.listener(|v, _e, _w, _cx| v.close_dialog())),
            )
            .child(
                dlg_btn("url-ok", i18n::t("common.open"), true, pal)
                    .on_click(cx.listener(|v, _e, _w, cx| v.url_submit(cx))),
            );
        dlg_shell("dlg-url", header, body, footer, pal, ui_font)
    }

    /// 分析配置多选下拉框(当前标签页;设置里的是新标签页的默认值)
    fn render_config_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (x, y) = self.config_menu?;
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        let ti = self.active.min(self.tabs.len().saturating_sub(1));
        let on_ids = self
            .tabs
            .get(ti)
            .map(|t| t.enabled_ids.clone())
            .unwrap_or_default();
        let configs: Vec<(usize, String, String, bool, bool)> = self
            .settings
            .configs
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    i,
                    c.id.clone(),
                    i18n::td(&c.name),
                    c.built_in,
                    on_ids.iter().any(|id| id == &c.id),
                )
            })
            .collect();

        let mut list = div().flex().flex_col().gap(px(2.0));
        for (i, _id, name, built_in, on) in configs {
            let mut label = name;
            if built_in {
                label.push_str(i18n::t("configs.builtin_marker_alt"));
            }
            list = list.child(
                div()
                    .id(SharedString::from(format!("cfg-menu-item-{i}")))
                    .cursor(CursorStyle::PointingHand)
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(4.0))
                    .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                    .on_click(cx.listener(move |v, _e, _w, _cx| v.toggle_tab_config(i)))
                    .child(
                        div()
                            .w(px(14.0))
                            .h(px(14.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(if on {
                                theme::hex(pal.primary)
                            } else {
                                theme::hex(pal.border)
                            })
                            .bg(if on {
                                theme::hex(pal.primary)
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_size(px(10.0))
                            .text_color(theme::hex(pal.on_primary))
                            .child(if on { "✓" } else { "" }),
                    )
                    .child(
                        div()
                            .text_size(px(theme::scaled(12.0)))
                            .text_color(if on {
                                theme::hex(pal.text1)
                            } else {
                                theme::hex(pal.text3)
                            })
                            .child(label),
                    ),
            );
        }

        Some(
            div()
                .id("cfg-overlay")
                .absolute()
                .inset_0()
                .on_click(cx.listener(|v, _e, _w, _cx| v.close_config_menu()))
                .child(
                    div()
                        .id("cfg-menu")
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(290.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .p(px(8.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme::hex(pal.border))
                        .bg(theme::hex(pal.bg_elevated))
                        .shadow_lg()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .px(px(8.0))
                                .py(px(2.0))
                                .text_size(px(theme::scaled(11.0)))
                                .text_color(theme::hex(pal.text3))
                                .child(i18n::t("analysis.for_tab")),
                        )
                        .child(list)
                        .child(div().h(px(1.0)).my(px(2.0)).bg(theme::hex(pal.split)))
                        .child(
                            div()
                                .id("cfg-manage")
                                .cursor(CursorStyle::PointingHand)
                                .px(px(8.0))
                                .py(px(4.0))
                                .rounded(px(4.0))
                                .text_size(px(theme::scaled(12.0)))
                                .text_color(theme::hex(pal.primary))
                                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                                .child(i18n::t("settings.manage_link"))
                                .on_click(cx.listener(|v, _e, window, cx| {
                                    v.config_menu = None;
                                    v.open_settings_tab(None, window, cx);
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 空白标签页:VS Code 式欢迎页(内嵌「打开日志源」表单 + 最近打开)
    fn render_welcome(&self, ti: usize, cx: &mut Context<Self>) -> AnyElement {
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        let recents: Vec<(usize, String, String)> = self
            .settings
            .recents
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.label.clone(), r.params.summary()))
            .collect();
        let recents_confirm = self.recents_confirm;

        // 竖排链接入口
        fn link(
            id: &'static str,
            label: &str,
            pal: &'static Palette,
            on_click: impl Fn(&mut AppView, &ClickEvent, &mut Window, &mut Context<AppView>) + 'static,
            cx: &Context<AppView>,
        ) -> AnyElement {
            div()
                .id(id)
                .cursor(CursorStyle::PointingHand)
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .text_size(px(theme::scaled(14.0)))
                .text_color(theme::hex(pal.primary))
                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                .child(label.to_string())
                .on_click(cx.listener(on_click))
                .into_any_element()
        }

        let links = div()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(2.0))
            .child(link(
                "welcome-open",
                i18n::t("common.open"),
                pal,
                move |v, _e, _w, cx| v.welcome_browse(ti, cx),
                cx,
            ))
            .child(link(
                "welcome-ssh",
                "SSH",
                pal,
                move |v, _e, window, cx| v.open_ssh_dialog(window, cx),
                cx,
            ))
            .child(link(
                "welcome-url",
                "URL",
                pal,
                move |v, _e, window, cx| v.open_url_dialog(window, cx),
                cx,
            ))
            .child(link(
                "welcome-cmd",
                i18n::t("source.command"),
                pal,
                move |v, _e, window, cx| v.open_cmd_dialog(window, cx),
                cx,
            ))
            .child(link(
                "welcome-settings",
                i18n::t("settings.title"),
                pal,
                move |v, _e, window, cx| v.open_settings_tab(Some(ti), window, cx),
                cx,
            ));
        let links = links.child(
            div()
                .mt(px(6.0))
                .pl(px(10.0))
                .text_size(px(theme::scaled(11.5)))
                .text_color(theme::hex(pal.text3))
                .child(i18n::t("welcome.drag_hint")),
        );

        let mut recent_rows: Vec<AnyElement> = Vec::new();
        for (i, label, summary) in recents {
            recent_rows.push(
                div()
                    .id(SharedString::from(format!("welcome-recent-{i}")))
                    .cursor(CursorStyle::PointingHand)
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(4.0))
                    .hover(|s| s.bg(theme::hex(pal.fill)))
                    .child(
                        div()
                            .text_size(px(theme::scaled(12.0)))
                            .text_color(theme::hex(pal.text1))
                            .whitespace_nowrap()
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(theme::scaled(11.0)))
                            .text_color(theme::hex(pal.text3))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(summary),
                    )
                    .on_click(cx.listener(move |v, _e, w, cx| {
                        v.welcome_open_recent(ti, i, w, cx);
                    }))
                    .child(
                        div()
                            .id(SharedString::from(format!("welcome-recent-del-{i}")))
                            .cursor(CursorStyle::PointingHand)
                            .flex_shrink_0()
                            .size(px(18.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .text_size(px(theme::scaled(12.0)))
                            .text_color(theme::hex(pal.text3))
                            .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                            .child(
                                svg()
                                    .path("icons/close.svg")
                                    .w(px(12.0))
                                    .h(px(12.0))
                                    .text_color(theme::hex(pal.text3)),
                            )
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::element(|_, _| {
                                    div()
                                        .text_size(px(theme::scaled(11.0)))
                                        .child(i18n::t("welcome.recent_remove.tip"))
                                })
                                .build(window, cx)
                            })
                            .on_click(cx.listener(move |v, _e, _w, cx| {
                                // 阻止冒泡到整行,避免删记录的同时又把源重新打开
                                cx.stop_propagation();
                                v.welcome_remove_recent(i, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        let has_recents = !recent_rows.is_empty();
        let link_block = div().w_full().flex().flex_col().gap(px(4.0)).child(links);

        // 小号文字按钮:确认态下同时给出「取消」与「确认清除」,可随时退出确认
        let text_btn = |id: &'static str, label: &'static str, color: &'static str| {
            div()
                .id(id)
                .cursor(CursorStyle::PointingHand)
                .flex_shrink_0()
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .text_size(px(theme::scaled(11.0)))
                .text_color(theme::hex(color))
                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                .child(label)
        };
        let clear_ctl = if recents_confirm {
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .child(
                    text_btn(
                        "welcome-recents-cancel",
                        i18n::t("common.cancel"),
                        pal.text3,
                    )
                    .on_click(cx.listener(|v, _e, _w, cx| v.welcome_cancel_clear(cx))),
                )
                .child(
                    text_btn(
                        "welcome-recents-clear",
                        i18n::t("dialog.clear.title"),
                        pal.error,
                    )
                    .on_click(cx.listener(|v, _e, _w, cx| v.welcome_clear_recents(cx))),
                )
        } else {
            div().child(
                text_btn(
                    "welcome-recents-clear",
                    i18n::t("dialog.clear.action"),
                    pal.text3,
                )
                .on_click(cx.listener(|v, _e, _w, cx| v.welcome_clear_recents(cx))),
            )
        };

        let recent_head = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("menu.recent")),
            )
            .child(div().flex_1())
            .child(clear_ctl);

        // 超出可视高度后滚动,避免遮挡下方内容
        let recent_list = div()
            .id("welcome-recents")
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .max_h(px(320.0))
            .overflow_y_scroll()
            .children(recent_rows);

        let recent_block = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(recent_head)
            .child(recent_list);

        fn column() -> Div {
            div().w(px(720.0)).px(px(24.0))
        }

        let content = if has_recents {
            // 链接入口贴着页面中线向上收束,最近打开从中线向下展开
            div()
                .w_full()
                .min_h_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_1()
                        .min_h(px(0.0))
                        .w_full()
                        .flex()
                        .items_end()
                        .justify_center()
                        .pb(px(20.0))
                        .child(column().child(link_block)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h(px(0.0))
                        .w_full()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(20.0))
                        .child(column().child(recent_block)),
                )
        } else {
            // 无最近打开:入口在页面中垂直居中
            div()
                .w_full()
                .min_h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(column().child(link_block))
        };

        div()
            .id("welcome-scroll")
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .overflow_y_scroll()
            .child(content)
            .into_any_element()
    }

    /// 设置面板(VS Code 式双栏):左侧分类 + 搜索,右侧分组设置项;改动即时生效
    fn render_settings_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        // 正则着色装饰是颜色快照:主题切换后按新配色重算一次
        if self.settings_panel.as_ref().and_then(|d| d.deco_light) != Some(light) {
            let inputs: Vec<Entity<TextareaState>> = self
                .settings_panel
                .as_ref()
                .map(|d| d.rule_inputs.iter().map(|(re, _)| re.clone()).collect())
                .unwrap_or_default();
            for e in inputs {
                e.update(cx, |s, cx| {
                    let text = s.value().to_string();
                    let deco = crate::code_input::decorations(&text, light);
                    s.set_decorations(deco, cx);
                });
            }
            if let Some(d) = self.settings_panel.as_mut() {
                d.deco_light = Some(light);
            }
        }
        let (Some(d), settings) = (self.settings_panel.as_ref(), &self.settings) else {
            return div().flex_1().min_h(px(0.0)).into_any_element();
        };

        let tab_sel = d.tab.min(SETTINGS_PAGE_COUNT - 1);
        let sel = d.sel_config;
        // 正则代码框字体:优先用户设置的日志字体(本就用于取等宽),否则平台等宽字体
        let code_mono: SharedString = if settings.log_font_family.trim().is_empty() {
            theme::mono_family().into()
        } else {
            settings.log_font_family.clone().into()
        };
        let query = d.search.read(cx).value().trim().to_lowercase();
        let searching = !query.is_empty();
        let saved_recent = d
            .saved_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        let search_e = d.search.clone();
        let poll_e = d.poll_ms.clone();
        let max_e = d.max_lines.clone();
        let init_e = d.initial_mb.clone();
        let tail_e = d.tail_mb.clone();
        let ui_size_e = d.ui_font_size.clone();
        let log_size_e = d.log_font_size.clone();
        let (ui_fam_e, log_fam_e) = (d.ui_font_family.clone(), d.log_font_family.clone());
        let color_dark = d.color_dark.clone();
        let color_light = d.color_light.clone();
        let (hl_dark_e, hl_light_e) = (d.hl_dark.clone(), d.hl_light.clone());
        let pick_dark = d.pick_dark.clone();
        let pick_light = d.pick_light.clone();
        let (pick_hl_dark, pick_hl_light) = (d.pick_hl_dark.clone(), d.pick_hl_light.clone());
        let rule_inputs = d.rule_inputs.clone();

        let names = level_labels();
        let lvl_colors = settings.level_colors_hex(light);
        let show_ln = settings.show_line_numbers;
        let show_ts = settings.show_timestamps;
        let wrap_default = settings.wrap;
        let configs: Vec<(String, bool, bool, usize)> = settings
            .configs
            .iter()
            .map(|c| (i18n::td(&c.name), c.built_in, c.enabled, c.rules.len()))
            .collect();
        let rules: Vec<(bool, u8)> = settings
            .configs
            .get(sel)
            .map(|c| c.rules.iter().map(|r| (r.auto, r.level)).collect())
            .unwrap_or_default();

        // —— 连接:已保存的 SSH 主机(增删改;只存连接目标,不含凭据) ——
        let hosts: Vec<(String, String, u16, String)> = settings
            .ssh_hosts
            .iter()
            .map(|h| (h.name.clone(), h.host.clone(), h.port, h.user.clone()))
            .collect();
        let host_edit = d.host_edit;
        let host_err = d.host_err.clone();
        let host_notice = d.host_notice.clone();
        let (hn_e, hh_e, hp_e, hu_e) = (
            d.h_name.clone(),
            d.h_host.clone(),
            d.h_port.clone(),
            d.h_user.clone(),
        );
        let mut host_rows: Vec<AnyElement> = Vec::new();
        if hosts.is_empty() {
            host_rows.push(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("ssh.hosts.empty"))
                    .into_any_element(),
            );
        }
        for (i, (name, host, port, user)) in hosts.iter().enumerate() {
            let addr = format!("{user}@{host}:{port}");
            let title = if name.trim().is_empty() {
                addr
            } else {
                format!("{} · {addr}", name.trim())
            };
            let mut row = div()
                .id(SharedString::from(format!("set-host-{i}")))
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(16.0))
                .py(px(8.0));
            if host_edit == Some(HostEdit::Existing(i)) {
                row = row.bg(theme::hex(pal.fill));
            }
            host_rows.push(
                row.child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(theme::scaled(13.0)))
                                .text_color(theme::hex(pal.text1))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(theme::scaled(11.0)))
                                .text_color(theme::hex(pal.text3))
                                .child(i18n::tf(
                                    "ssh.host.user",
                                    &[
                                        ("host", host.to_string()),
                                        ("port", port.to_string()),
                                        ("user", user.to_string()),
                                    ],
                                )),
                        ),
                )
                .child(
                    panel_icon_btn(
                        SharedString::from(format!("set-host-edit-{i}")),
                        "icons/edit-outline.svg",
                        SharedString::from(i18n::t("ssh.hosts.edit.tip")),
                        pal,
                    )
                    .on_click(cx.listener(move |v, _e, w, cx| {
                        v.host_begin_edit(i, w, cx);
                        cx.notify();
                    })),
                )
                .child(
                    panel_icon_btn(
                        SharedString::from(format!("set-host-del-{i}")),
                        "icons/delete.svg",
                        SharedString::from(i18n::t("ssh.hosts.delete.tip")),
                        pal,
                    )
                    .on_click(cx.listener(move |v, _e, _w, cx| v.host_delete(i, cx))),
                )
                .into_any_element(),
            );
        }
        if !hosts.is_empty() {
            host_rows.push(
                div()
                    .px(px(16.0))
                    .pb(px(4.0))
                    .text_size(px(theme::scaled(11.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("ssh.hosts.delete_note"))
                    .into_any_element(),
            );
        }
        // 三个主机动作放在区块标题栏右上角:图标 + 悬停提示;编辑中暂时收起,避免与表单操作冲突
        let host_actions: Option<AnyElement> = if host_edit.is_none() {
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        panel_icon_btn(
                            "set-host-add",
                            "icons/host-add.svg",
                            SharedString::from(i18n::t("ssh.hosts.add.tip")),
                            pal,
                        )
                        .on_click(cx.listener(|v, _e, w, cx| {
                            v.host_begin_new(w, cx);
                            cx.notify();
                        })),
                    )
                    .child(
                        panel_icon_btn(
                            "set-host-import",
                            "icons/host-import-recent.svg",
                            SharedString::from(i18n::t("ssh.hosts.import_recent.tip")),
                            pal,
                        )
                        .on_click(cx.listener(|v, _e, _w, cx| v.host_import_recents(cx))),
                    )
                    .child(
                        panel_icon_btn(
                            "set-host-import-ssh",
                            "icons/host-import-ssh.svg",
                            SharedString::from(i18n::t("ssh.hosts.import_ssh_config.tip")),
                            pal,
                        )
                        .on_click(cx.listener(|v, _e, _w, cx| v.host_import_ssh_config(cx))),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };
        match host_edit {
            None => {}
            Some(ed) => {
                let mut ed_box = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .py(px(12.0))
                    .bg(theme::hex(pal.fill))
                    .border_t_1()
                    .border_color(theme::hex(pal.split))
                    .child(
                        div()
                            .text_size(px(theme::scaled(12.0)))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme::hex(pal.text2))
                            .child(if ed == HostEdit::New {
                                i18n::t("ssh.hosts.add_short")
                            } else {
                                i18n::t("ssh.hosts.edit")
                            }),
                    )
                    .child(settings_row(
                        pal,
                        i18n::t("ssh.hosts.note"),
                        Some(i18n::t("ssh.hosts.note_placeholder").into()),
                        text_input(&hn_e, 220.0),
                    ))
                    .child(settings_row(
                        pal,
                        i18n::t("ssh.host"),
                        None,
                        text_input(&hh_e, 220.0),
                    ))
                    .child(settings_row(
                        pal,
                        i18n::t("ssh.port"),
                        None,
                        text_input(&hp_e, 96.0),
                    ))
                    .child(settings_row(
                        pal,
                        i18n::t("ssh.username"),
                        None,
                        text_input(&hu_e, 220.0),
                    ));
                if let Some(e) = host_err {
                    ed_box = ed_box.child(
                        div()
                            .text_size(px(theme::scaled(11.5)))
                            .text_color(theme::hex(pal.error))
                            .child(e),
                    );
                }
                host_rows.push(
                    ed_box
                        .child(
                            div()
                                .flex()
                                .gap(px(8.0))
                                .child(
                                    host_btn(
                                        "set-host-save".into(),
                                        i18n::t("common.save"),
                                        true,
                                        pal,
                                    )
                                    .on_click(cx.listener(|v, _e, _w, cx| v.host_save(cx))),
                                )
                                .child(
                                    host_btn(
                                        "set-host-cancel".into(),
                                        i18n::t("common.cancel"),
                                        false,
                                        pal,
                                    )
                                    .on_click(cx.listener(|v, _e, _w, cx| v.host_edit_cancel(cx))),
                                ),
                        )
                        .into_any_element(),
                );
            }
        }
        let host_entries: Vec<(String, AnyElement)> = vec![(
            "已保存的 ssh 主机 服务器 连接 账号 删除 新增 导入 config".into(),
            div()
                .flex()
                .flex_col()
                .children(host_rows)
                .children(host_notice.map(|n| {
                    div()
                        .px(px(16.0))
                        .pb(px(10.0))
                        .text_size(px(theme::scaled(11.5)))
                        .text_color(theme::hex(pal.text2))
                        .child(n)
                        .into_any_element()
                }))
                .into_any_element(),
        )];

        // —— 连接:已保存 URL 凭据(系统钥匙串里 remembered 的 HTTP 条目) ——
        let url_cred_notice = d.url_cred_notice.clone();
        let url_creds: Vec<(String, String, String, String)> = {
            let mut out: Vec<(String, String, String, String)> = Vec::new();
            for r in &settings.recents {
                if !r.remembered {
                    continue;
                }
                let SourceParams::Http {
                    url,
                    auth,
                    user,
                    header_name,
                    ..
                } = &r.params
                else {
                    continue;
                };
                let Some(key) = secrets::key_of(&r.params) else {
                    continue;
                };
                if out.iter().any(|(k, ..)| k == &key) {
                    continue;
                }
                let principal = match auth {
                    crate::source::HttpAuthKind::Basic => user.clone().unwrap_or_default(),
                    crate::source::HttpAuthKind::ApiKey | crate::source::HttpAuthKind::Header => {
                        header_name.clone().unwrap_or_default()
                    }
                    _ => String::new(),
                };
                out.push((
                    key,
                    url.clone(),
                    http_auth_label(*auth).to_string(),
                    principal,
                ));
            }
            out
        };
        let mut url_cred_rows: Vec<AnyElement> = Vec::new();
        if url_creds.is_empty() {
            url_cred_rows.push(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("http.creds.empty"))
                    .into_any_element(),
            );
        }
        for (i, (key, url, kind, principal)) in url_creds.iter().enumerate() {
            let sub = if principal.is_empty() {
                kind.clone()
            } else {
                format!("{kind} · {principal}")
            };
            let del_key = key.clone();
            url_cred_rows.push(
                div()
                    .id(SharedString::from(format!("set-urlcred-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(16.0))
                    .py(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(theme::scaled(13.0)))
                                    .text_color(theme::hex(pal.text1))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(url.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::scaled(11.0)))
                                    .text_color(theme::hex(pal.text3))
                                    .child(sub),
                            ),
                    )
                    .child(
                        host_btn(
                            SharedString::from(format!("set-urlcred-del-{i}")),
                            i18n::t("common.delete"),
                            false,
                            pal,
                        )
                        .on_click(
                            cx.listener(move |v, _e, _w, cx| {
                                v.url_cred_delete(del_key.clone(), cx)
                            }),
                        ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(n) = url_cred_notice {
            url_cred_rows.push(
                div()
                    .px(px(16.0))
                    .pb(px(10.0))
                    .text_size(px(theme::scaled(11.5)))
                    .text_color(theme::hex(pal.text2))
                    .child(i18n::td(&n))
                    .into_any_element(),
            );
        }
        let url_cred_entries: Vec<(String, AnyElement)> = vec![(
            "url 凭据 http basic bearer token api key 钥匙串 密码 删除".into(),
            div()
                .flex()
                .flex_col()
                .children(url_cred_rows)
                .into_any_element(),
        )];

        // —— 连接:最近打开的行为 ——
        let rl_e = d.recents_limit.clone();
        let purge_creds = settings.recents_purge_creds;
        let recents_cfg: Vec<(String, AnyElement)> = vec![
            (
                "最近打开记录保存条数 条数 上限 history limit".into(),
                settings_row(
                    pal,
                    i18n::t("settings.recents_limit"),
                    Some(i18n::t("settings.recents_limit.desc").into()),
                    num_input(&rl_e, 96.0, i18n::t("unit.items"), pal),
                ),
            ),
            (
                "删除 url 最近打开记录时清除凭据 钥匙串 keychain 密码 token".into(),
                settings_row(
                    pal,
                    i18n::t("settings.recents_purge"),
                    Some(i18n::t("settings.recents_purge.desc").into()),
                    switch_el("set-recents-purge", purge_creds, pal)
                        .on_click(cx.listener(|v, _e, _w, cx| v.toggle_recents_purge(cx)))
                        .into_any_element(),
                ),
            ),
        ];

        // —— 外观与显示 ——
        let mut appearance: Vec<(String, AnyElement)> = vec![(
            "显示行号 行号 line numbers".into(),
            settings_row(
                pal,
                i18n::t("settings.line_numbers"),
                Some(i18n::t("settings.line_numbers.desc").into()),
                switch_el("set-ln", show_ln, pal)
                    .on_click(cx.listener(|v, _e, _w, cx| v.toggle_show_line_numbers(cx)))
                    .into_any_element(),
            ),
        )];
        appearance.push((
            "显示时间戳 时间戳 timestamp time 输出时间".into(),
            settings_row(
                pal,
                i18n::t("settings.timestamps"),
                Some(i18n::t("settings.timestamps.desc").into()),
                switch_el("set-ts", show_ts, pal)
                    .on_click(cx.listener(|v, _e, _w, cx| v.toggle_show_timestamps(cx)))
                    .into_any_element(),
            ),
        ));
        appearance.push((
            "默认自动换行 换行 wrap".into(),
            settings_row(
                pal,
                i18n::t("settings.wrap_default"),
                Some(i18n::t("settings.wrap_default.desc").into()),
                switch_el("set-wrap", wrap_default, pal)
                    .on_click(cx.listener(|v, _e, _w, cx| v.toggle_wrap_default(cx)))
                    .into_any_element(),
            ),
        ));
        // 界面语言:三态选择,切换后立即重绘(仅影响已迁移文案与组件库 locale)
        let mut lang_picker = div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded(px(6.0))
            .bg(theme::hex(pal.fill_hover));
        for lang in i18n::Lang::ALL {
            let on = i18n::lang() == lang;
            lang_picker = lang_picker.child(
                div()
                    .id(SharedString::from(format!("set-lang-{}", lang.code())))
                    .cursor(CursorStyle::PointingHand)
                    .px(px(9.0))
                    .py(px(3.0))
                    .rounded(px(5.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(if on { pal.text1 } else { pal.text2 }))
                    .bg(if on {
                        theme::hex(pal.bg_elevated)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .child(lang.label())
                    .on_click(
                        cx.listener(move |v, _e, window, cx| v.set_language(lang, window, cx)),
                    ),
            );
        }
        appearance.push((
            "界面语言 language language 文".into(),
            settings_row(
                pal,
                i18n::t("settings.language"),
                Some(i18n::t("settings.language.desc").into()),
                lang_picker.into_any_element(),
            ),
        ));
        let typography: Vec<(String, AnyElement)> = vec![
            (
                "界面字号 字号 font size".into(),
                settings_row(
                    pal,
                    i18n::t("settings.ui_font_size"),
                    Some(i18n::t("settings.ui_font_size.desc").into()),
                    num_input(&ui_size_e, 96.0, "pt", pal),
                ),
            ),
            (
                "日志字号 字号 font size".into(),
                settings_row(
                    pal,
                    i18n::t("settings.log_font_size"),
                    Some(i18n::t("settings.log_font_size.desc").into()),
                    num_input(&log_size_e, 96.0, "pt", pal),
                ),
            ),
            (
                "界面字体 字体 family".into(),
                settings_row(
                    pal,
                    i18n::t("settings.ui_font"),
                    Some(i18n::t("settings.ui_font.desc").into()),
                    text_input(&ui_fam_e, 220.0),
                ),
            ),
            (
                "日志字体 等宽 字体 family".into(),
                settings_row(
                    pal,
                    i18n::t("settings.log_font"),
                    Some(i18n::t("settings.log_font.desc").into()),
                    text_input(&log_fam_e, 220.0),
                ),
            ),
        ];

        // —— 颜色管理:两行表头(主题 / 日志级别 / 搜索高亮),每个主题一行 ——
        let cell_w = 108.0;
        let theme_w = 40.0;
        let col_gap = 5.0;
        let levels_w = cell_w * LEVEL_COUNT as f32 + col_gap * (LEVEL_COUNT as f32 - 1.0);
        let head1 = div()
            .flex()
            .items_center()
            .gap(px(col_gap))
            .child(
                div()
                    .w(px(theme_w))
                    .text_size(px(theme::scaled(10.5)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::t("settings.theme")),
            )
            .child(
                div()
                    .w(px(levels_w))
                    .flex()
                    .justify_center()
                    .text_size(px(theme::scaled(10.5)))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme::hex(pal.text2))
                    .child(i18n::t("settings.level_colors")),
            )
            .child(
                div()
                    .w(px(cell_w))
                    .flex()
                    .justify_center()
                    .text_size(px(theme::scaled(10.5)))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme::hex(pal.text2))
                    .child(i18n::t("settings.search_highlight")),
            );
        let mut head2 = div()
            .flex()
            .items_center()
            .gap(px(col_gap))
            .child(div().w(px(theme_w)));
        for i in 0..LEVEL_COUNT {
            head2 = head2.child(
                div()
                    .w(px(cell_w))
                    .flex()
                    .justify_center()
                    .text_size(px(theme::scaled(10.0)))
                    .text_color(theme::hex(&lvl_colors[i]))
                    .child(names[i].clone()),
            );
        }
        head2 = head2.child(
            div()
                .w(px(cell_w))
                .flex()
                .justify_center()
                .text_size(px(theme::scaled(10.0)))
                .text_color(theme::hex(pal.text3))
                .child(i18n::t("settings.background")),
        );
        let mut body = div().flex().flex_col().gap(px(4.0));
        for light_row in [false, true] {
            let (picks, inputs) = if light_row {
                (&pick_light, &color_light)
            } else {
                (&pick_dark, &color_dark)
            };
            let (hp, he) = if light_row {
                (&pick_hl_light, &hl_light_e)
            } else {
                (&pick_hl_dark, &hl_dark_e)
            };
            let mut row = div().flex().items_center().gap(px(col_gap)).child(
                div()
                    .w(px(theme_w))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text1))
                    .child(if light_row {
                        i18n::t("theme.light")
                    } else {
                        i18n::t("theme.dark")
                    }),
            );
            for i in 0..LEVEL_COUNT {
                row = row.child(color_cell(&picks[i], &inputs[i], cell_w));
            }
            row = row.child(color_cell(hp, he, cell_w));
            body = body.child(row);
        }
        let color_rows: Vec<(String, AnyElement)> = vec![(
            "颜色管理 颜色 日志级别 搜索高亮 高亮 暗色 亮色 hex".into(),
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(16.0))
                        .py(px(10.0))
                        .child(head1)
                        .child(head2)
                        .child(div().h(px(1.0)).my(px(2.0)).bg(theme::hex(pal.split)))
                        .child(body),
                )
                .child(
                    div()
                        .px(px(16.0))
                        .py(px(8.0))
                        .border_t_1()
                        .border_color(theme::hex(pal.split))
                        .text_size(px(theme::scaled(11.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(i18n::t("settings.level_colors.desc")),
                )
                .into_any_element(),
        )];

        // —— 日志与性能 ——
        let load: Vec<(String, AnyElement)> = vec![
            (
                "轮询间隔 poll ms".into(),
                settings_row(
                    pal,
                    i18n::t("settings.poll_interval"),
                    Some(i18n::t("settings.poll_interval.desc").into()),
                    num_input(&poll_e, 110.0, "ms", pal),
                ),
            ),
            (
                "初始加载 initial mb".into(),
                settings_row(
                    pal,
                    i18n::t("settings.initial_load"),
                    Some(i18n::t("settings.initial_load.desc").into()),
                    num_input(&init_e, 110.0, "MB", pal),
                ),
            ),
            (
                "尾部加载上限 tail mb".into(),
                settings_row(
                    pal,
                    i18n::t("settings.tail_limit"),
                    Some(i18n::t("settings.tail_limit.desc").into()),
                    num_input(&tail_e, 110.0, "MB", pal),
                ),
            ),
        ];
        let memory: Vec<(String, AnyElement)> = vec![(
            "行数上限 max lines 内存".into(),
            settings_row(
                pal,
                i18n::t("settings.max_lines"),
                Some(i18n::t("settings.max_lines.desc").into()),
                num_input(&max_e, 130.0, i18n::t("unit.lines"), pal),
            ),
        )];

        // —— 分析配置(搜索时的条目)——
        let mut cfg_entries: Vec<(String, AnyElement)> = Vec::new();
        for (i, (name, built_in, enabled, rules_n)) in configs.iter().enumerate() {
            let open = div()
                .id(SharedString::from(format!("cfg-open-{i}")))
                .cursor(CursorStyle::PointingHand)
                .px(px(12.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme::hex(pal.border))
                .text_size(px(theme::scaled(12.0)))
                .text_color(theme::hex(pal.text1))
                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                .child(i18n::t("common.edit"))
                .on_click(cx.listener(move |v, _e, window, cx| v.open_config_page(i, window, cx)))
                .into_any_element();
            let meta = i18n::tnf(
                *rules_n as u64,
                "configs.summary",
                &[
                    (
                        "status",
                        if *enabled {
                            i18n::t("common.enabled").to_string()
                        } else {
                            i18n::t("common.disabled").to_string()
                        },
                    ),
                    ("n", rules_n.to_string()),
                    (
                        "suffix",
                        if *built_in {
                            i18n::t("configs.builtin_marker_alt").to_string()
                        } else {
                            String::new()
                        },
                    ),
                ],
            );
            cfg_entries.push((
                format!("{name} 配置 分析规则 正则"),
                settings_row(pal, name.clone(), Some(meta.into()), open),
            ));
        }

        // 配置导入 / 导出(导入为整体替换;导出同格式的 JSON)
        let cfg_notice = d.cfg_notice.clone();
        let mut cfg_act = div().flex().items_center().gap(px(8.0));
        cfg_act = cfg_act
            .child(
                host_btn(
                    "cfg-import".into(),
                    i18n::t("configs.import_json"),
                    false,
                    pal,
                )
                .on_click(cx.listener(|v, _e, _w, cx| v.import_configs(cx))),
            )
            .child(
                host_btn(
                    "cfg-export".into(),
                    i18n::t("configs.export_json"),
                    false,
                    pal,
                )
                .on_click(cx.listener(|v, _e, _w, cx| v.export_configs(cx))),
            );
        if let Some(n) = cfg_notice {
            cfg_act = cfg_act.child(
                div()
                    .text_size(px(theme::scaled(11.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(n),
            );
        }
        cfg_entries.push((
            "配置 导入 导出 json 备份 恢复".into(),
            settings_row(
                pal,
                i18n::t("settings.config_io"),
                Some(i18n::t("settings.config_io.desc").into()),
                cfg_act.into_any_element(),
            ),
        ));

        // —— 分析配置页:左侧列表 + 右侧规则编辑 ——
        let config_page: AnyElement = {
            let mut list = div()
                .id("cfg-list")
                .w(px(216.0))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .px(px(8.0))
                .py(px(8.0))
                .border_r_1()
                .border_color(theme::hex(pal.split))
                .bg(theme::hex(pal.bg_bar))
                .overflow_y_scroll();
            for (i, (name, built_in, enabled, rules_n)) in configs.iter().enumerate() {
                let on = i == sel;
                let item = div()
                    .id(SharedString::from(format!("cfg-item-{i}")))
                    .cursor(CursorStyle::PointingHand)
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .bg(if on {
                        theme::hex(pal.fill)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                    .child(
                        div()
                            .text_size(px(theme::scaled(12.5)))
                            .text_color(theme::hex(if on { pal.text1 } else { pal.text2 }))
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(theme::hex(if *enabled { pal.success } else { pal.text3 })),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::scaled(10.5)))
                                    .text_color(theme::hex(pal.text3))
                                    .child(i18n::tnf(
                                        *rules_n as u64,
                                        "configs.rules_count",
                                        &[
                                            ("n", rules_n.to_string()),
                                            (
                                                "suffix",
                                                if *built_in {
                                                    i18n::t("configs.builtin_marker_alt")
                                                        .to_string()
                                                } else {
                                                    String::new()
                                                },
                                            ),
                                        ],
                                    )),
                            ),
                    )
                    .on_click(cx.listener(move |v, _e, window, cx| v.select_config(i, window, cx)));
                list = list.child(item);
            }
            if let Some(n) = d.cfg_notice.clone() {
                list = list.child(
                    div()
                        .pt(px(4.0))
                        .text_size(px(theme::scaled(10.5)))
                        .text_color(theme::hex(pal.text3))
                        .child(n),
                );
            }

            let mut detail = div()
                .id("cfg-detail")
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(14.0))
                .px(px(22.0))
                .py(px(18.0))
                .overflow_y_scroll();
            if let Some((name, built_in, enabled, _)) = configs.get(sel) {
                // 内置配置默认只读:规则输入框不可编辑,解锁后才允许改动
                let readonly = *built_in && !d.cfg_unlock;
                let mut head = div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(theme::scaled(16.0)))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme::hex(pal.text1))
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .bg(theme::hex(pal.fill))
                            .text_size(px(theme::scaled(10.5)))
                            .text_color(theme::hex(pal.text3))
                            .child(if *built_in {
                                i18n::t("configs.builtin")
                            } else {
                                i18n::t("configs.custom")
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(theme::scaled(12.0)))
                            .text_color(theme::hex(pal.text2))
                            .child(i18n::t("configs.enabled_default")),
                    )
                    .child(switch_el("cfg-enable", *enabled, pal).on_click(
                        cx.listener(move |v, _e, _w, cx| v.toggle_config_enabled(sel, cx)),
                    ));
                if !*built_in {
                    head = head.child(
                        dlg_btn("cfg-del", i18n::t("configs.delete"), false, pal)
                            .on_click(cx.listener(|v, _e, window, cx| v.del_config(window, cx))),
                    );
                } else if !readonly {
                    head = head.child(
                        dlg_btn(
                            "cfg-reset-one",
                            i18n::t("configs.restore_default"),
                            false,
                            pal,
                        )
                        .on_click(cx.listener(move |v, _e, window, cx| {
                            v.reset_builtin_config(sel, window, cx)
                        })),
                    );
                }
                detail = detail.child(head);

                let mut rule_rows: Vec<AnyElement> = Vec::new();
                rule_rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(6.0))
                        .border_b_1()
                        .border_color(theme::hex(pal.split))
                        .text_size(px(theme::scaled(10.5)))
                        .text_color(theme::hex(pal.text3))
                        .child(div().w(px(24.0)).child("#"))
                        .child(div().flex_1().child(i18n::t("configs.regex_auto")))
                        .child(div().w(px(56.0)).child(i18n::t("configs.flags")))
                        .child(div().w(px(44.0)).child(i18n::t("configs.auto")))
                        .child(div().w(px(72.0)).child(i18n::t("configs.level")))
                        .child(div().w(px(18.0)).child(""))
                        .into_any_element(),
                );
                for (i, (auto, level)) in rules.iter().enumerate() {
                    let Some((re, fl)) = rule_inputs.get(i) else {
                        break;
                    };
                    let mut r = div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(7.0))
                        .border_b_1()
                        .border_color(theme::hex(pal.split))
                        .child(
                            div()
                                .w(px(24.0))
                                .text_size(px(theme::scaled(11.0)))
                                .text_color(theme::hex(pal.text3))
                                .child(format!("{}", i + 1)),
                        )
                        .child(
                            div().flex_1().min_w(px(0.0)).child(
                                Textarea::new(re)
                                    .small()
                                    .w_full()
                                    .readonly(readonly)
                                    .font_family(code_mono.clone())
                                    .text_size(px(theme::scaled(12.0)))
                                    .text_color(theme::hex(pal.text1)),
                            ),
                        )
                        .child(
                            div()
                                .w(px(56.0))
                                .child(Input::new(fl).small().w_full().readonly(readonly)),
                        )
                        .child({
                            // 只读时开关淡出且不挂点击,避免误以为可以改
                            let sw =
                                switch_el(SharedString::from(format!("rule-auto-{i}")), *auto, pal);
                            let sw = if readonly {
                                sw.opacity(0.5)
                            } else {
                                sw.on_click(
                                    cx.listener(move |v, _e, _w, cx| v.toggle_rule_auto(i, cx)),
                                )
                            };
                            div().w(px(44.0)).child(sw)
                        });
                    let chip = if *auto {
                        div()
                            .w(px(72.0))
                            .text_size(px(theme::scaled(10.5)))
                            .text_color(theme::hex(pal.text3))
                            .child(i18n::t("configs.capture_group"))
                            .into_any_element()
                    } else {
                        let lname = names
                            .get(*level as usize)
                            .cloned()
                            .unwrap_or_else(|| "—".to_string());
                        let lc = lvl_colors
                            .get(*level as usize)
                            .map(|s| theme::hex(s))
                            .unwrap_or_else(|| theme::hex(pal.text2));
                        let chip = div()
                            .id(SharedString::from(format!("rule-lvl-{i}")))
                            .w(px(72.0))
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .bg(theme::hex(pal.fill))
                            .text_size(px(theme::scaled(10.5)))
                            .text_color(lc)
                            .child(format!("{lname} ▸"));
                        if readonly {
                            chip.opacity(0.6).into_any_element()
                        } else {
                            chip.cursor(CursorStyle::PointingHand)
                                .on_click(
                                    cx.listener(move |v, _e, _w, cx| v.cycle_rule_level(i, cx)),
                                )
                                .into_any_element()
                        }
                    };
                    r = r.child(chip);
                    if rules.len() > 1 && !readonly {
                        r = r.child(
                            div()
                                .id(SharedString::from(format!("rule-del-{i}")))
                                .cursor(CursorStyle::PointingHand)
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(18.0))
                                .h(px(18.0))
                                .rounded(px(4.0))
                                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                                .child(
                                    svg()
                                        .path("icons/close.svg")
                                        .w(px(12.0))
                                        .h(px(12.0))
                                        .text_color(theme::hex(pal.text3)),
                                )
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::element(|_, _| {
                                        div()
                                            .text_size(px(theme::scaled(11.0)))
                                            .child(i18n::t("configs.delete_rule.tip"))
                                    })
                                    .build(window, cx)
                                })
                                .on_click(
                                    cx.listener(move |v, _e, window, cx| v.del_rule(i, window, cx)),
                                ),
                        );
                    }
                    rule_rows.push(r.into_any_element());
                }
                // 规则块的右上角:新增规则(只读时隐藏)+ 内置配置的只读/编辑切换
                let mut rules_action = div().flex().items_center().gap(px(2.0));
                if readonly {
                    // 内置配置默认只读:给出状态标记,避免误以为输入框坏了
                    rules_action = rules_action.child(
                        div()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .bg(theme::hex(pal.fill))
                            .text_size(px(theme::scaled(10.5)))
                            .text_color(theme::hex(pal.text3))
                            .child(i18n::t("configs.readonly")),
                    );
                }
                if !readonly {
                    rules_action = rules_action.child(
                        panel_icon_btn(
                            "rule-add",
                            "icons/rule-add.svg",
                            SharedString::from(i18n::t("configs.add_rule.tip")),
                            pal,
                        )
                        .on_click(cx.listener(|v, _e, window, cx| v.add_rule(window, cx))),
                    );
                }
                if *built_in {
                    rules_action = rules_action.child(
                        panel_icon_btn(
                            "cfg-edit-toggle",
                            if readonly {
                                "icons/edit-outline.svg"
                            } else {
                                "icons/lock.svg"
                            },
                            SharedString::from(if readonly {
                                i18n::t("configs.edit.tip")
                            } else {
                                i18n::t("configs.readonly.tip")
                            }),
                            pal,
                        )
                        .on_click(
                            cx.listener(|v, _e, window, cx| v.toggle_config_edit(window, cx)),
                        ),
                    );
                }
                detail = detail.child(settings_group(
                    i18n::t("configs.rules"),
                    Some(rules_action.into_any_element()),
                    rule_rows,
                    pal,
                ));
                detail = detail.child(
                    div()
                        .text_size(px(theme::scaled(11.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(i18n::t("configs.auto.desc")),
                );
            } else {
                detail = detail.child(
                    div()
                        .text_size(px(theme::scaled(12.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(i18n::t("configs.empty")),
                );
            }

            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .child(list)
                .child(detail)
                .into_any_element()
        };

        // —— 键盘快捷键(可重绑定;Enter/Esc 为固定绑定) ——
        let mut shortcut_entries: Vec<(String, AnyElement)> = Vec::new();
        for (action, desc) in store::SHORTCUT_ACTIONS {
            let desc = i18n::td(desc);
            let spec = settings.shortcut(action);
            let capturing = d.capture.as_deref() == Some(action);
            let mut ctl = div().flex().items_center().gap(px(6.0));
            if capturing {
                ctl = ctl.child(
                    div()
                        .text_size(px(theme::scaled(11.0)))
                        .text_color(theme::hex(pal.primary))
                        .child(i18n::t("shortcut.press")),
                );
            } else {
                ctl = ctl.child(shortcut_caps(&spec, pal));
            }
            ctl = ctl.child(
                panel_icon_btn(
                    SharedString::from(format!("sc-edit-{action}")),
                    if capturing {
                        "icons/close.svg"
                    } else {
                        "icons/edit-outline.svg"
                    },
                    SharedString::from(if capturing {
                        i18n::t("shortcut.cancel.tip")
                    } else {
                        i18n::t("shortcut.change.tip")
                    }),
                    pal,
                )
                .on_click(cx.listener(move |v, _e, w, cx| {
                    if capturing {
                        v.cancel_shortcut_capture(cx);
                    } else {
                        v.begin_shortcut_capture(action, w, cx);
                    }
                })),
            );
            ctl = ctl.child(
                panel_icon_btn(
                    SharedString::from(format!("sc-reset-{action}")),
                    "icons/reset.svg",
                    SharedString::from(i18n::t("shortcut.reset.tip")),
                    pal,
                )
                .on_click(cx.listener(move |v, _e, _w, cx| v.reset_shortcut(action, cx))),
            );
            shortcut_entries.push((
                format!("{desc} {spec}"),
                settings_row(pal, &desc, None, ctl.into_any_element()),
            ));
        }
        for (keys, desc) in [
            ("⏎", i18n::t("shortcut.hint.next_match")),
            ("Esc", i18n::t("shortcut.hint.close_popups")),
        ] {
            let caps: AnyElement = div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .children(keys.split(' ').map(|k| keycap(k, pal)))
                .into_any_element();
            shortcut_entries.push((
                format!("{desc} {keys}"),
                settings_row(pal, desc, Some(i18n::t("shortcut.fixed").into()), caps),
            ));
        }
        if let Some(n) = d.sc_notice.clone() {
            shortcut_entries.push((
                i18n::t("shortcut.hints").to_string(),
                div()
                    .px(px(16.0))
                    .py(px(8.0))
                    .text_size(px(theme::scaled(11.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::td(&n))
                    .into_any_element(),
            ));
        }

        // —— 关于:版本与构建、许可证、项目链接、检查更新 ——
        let version = env!("CARGO_PKG_VERSION");
        let build = env!("GIT_HASH");
        let about_mono = theme::mono_family();
        let ver_control = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .font_family(about_mono)
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text2))
                    .child(format!("{version} · {build}")),
            )
            .child(
                host_btn(
                    SharedString::from("about-copy"),
                    i18n::t("about.copy"),
                    false,
                    pal,
                )
                .on_click(cx.listener(move |v, e: &ClickEvent, _w, cx| {
                    // 报障时直接粘这一行:版本 + 构建 + 平台
                    let text = format!(
                        "{} {} ({build}), {}",
                        i18n::t("app.name"),
                        env!("CARGO_PKG_VERSION"),
                        std::env::consts::OS
                    );
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    v.show_toast(i18n::t("about.copied"), e.position(), cx);
                })),
            )
            .into_any_element();

        let license_control = host_btn(
            SharedString::from("about-license"),
            i18n::t("about.license.open"),
            false,
            pal,
        )
        .on_click(cx.listener(|_v, _e, _w, _cx| {
            let _ = open::that(format!("{REPO_URL}/blob/main/LICENSE"));
        }))
        .into_any_element();

        // 外链统一走系统默认程序(与日志内 URL 链接一致)
        let link_btn = |id: &'static str, url: String| -> AnyElement {
            host_btn(SharedString::from(id), i18n::t("common.open"), false, pal)
                .on_click(cx.listener(move |_v, _e, _w, _cx| {
                    let _ = open::that(&url);
                }))
                .into_any_element()
        };

        let update_control: AnyElement = match self.update_state.clone() {
            UpdateState::Idle => host_btn(
                SharedString::from("about-check"),
                i18n::t("about.update.action"),
                true,
                pal,
            )
            .on_click(cx.listener(|v, _e, _w, cx| v.check_update(cx)))
            .into_any_element(),
            UpdateState::Checking => div()
                .text_size(px(theme::scaled(12.0)))
                .text_color(theme::hex(pal.text3))
                .child(i18n::t("about.update.checking"))
                .into_any_element(),
            UpdateState::Latest(tag) => div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    div()
                        .text_size(px(theme::scaled(12.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(i18n::tf("about.update.latest", &[("v", tag)])),
                )
                .child(
                    host_btn(
                        SharedString::from("about-recheck"),
                        i18n::t("about.update.retry"),
                        false,
                        pal,
                    )
                    .on_click(cx.listener(|v, _e, _w, cx| v.check_update(cx))),
                )
                .into_any_element(),
            UpdateState::Available(rel) => div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    div()
                        .text_size(px(theme::scaled(12.0)))
                        .text_color(theme::hex(pal.success))
                        .child(i18n::tf(
                            "about.update.available",
                            &[("v", rel.tag.clone())],
                        )),
                )
                .child(
                    host_btn(
                        SharedString::from("about-download"),
                        i18n::t("about.update.open"),
                        true,
                        pal,
                    )
                    .on_click(cx.listener(move |_v, _e, _w, _cx| {
                        let _ = open::that(&rel.url);
                    })),
                )
                .into_any_element(),
            UpdateState::Failed(err) => div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    div()
                        .max_w(px(360.0))
                        .text_size(px(theme::scaled(11.5)))
                        .text_color(theme::hex(pal.error))
                        .child(i18n::tf("about.update.failed", &[("e", err)])),
                )
                .child(
                    host_btn(
                        SharedString::from("about-retry"),
                        i18n::t("about.update.retry"),
                        false,
                        pal,
                    )
                    .on_click(cx.listener(|v, _e, _w, cx| v.check_update(cx))),
                )
                .into_any_element(),
        };

        // 行标题同时作为设置搜索的命中文本
        let row = |title: &'static str, desc: &'static str, control: AnyElement| {
            (
                title.to_string(),
                settings_row(pal, title, Some(desc.into()), control),
            )
        };
        let about_rows: Vec<(String, AnyElement)> = vec![
            row(
                i18n::t("about.version"),
                i18n::t("about.version.desc"),
                ver_control,
            ),
            row(
                i18n::t("about.license"),
                i18n::t("about.license.desc"),
                license_control,
            ),
        ];
        let link_rows: Vec<(String, AnyElement)> = vec![
            row(
                i18n::t("about.repo"),
                i18n::t("about.repo.desc"),
                link_btn("about-link-repo", REPO_URL.to_string()),
            ),
            row(
                i18n::t("about.issues"),
                i18n::t("about.issues.desc"),
                link_btn("about-link-issues", format!("{REPO_URL}/issues")),
            ),
            row(
                i18n::t("about.changelog"),
                i18n::t("about.changelog.desc"),
                link_btn(
                    "about-link-changelog",
                    format!("{REPO_URL}/blob/main/CHANGELOG.md"),
                ),
            ),
        ];

        // GitHub 图标入口:项目链接分组右上角(ionicons logo-github 单色化)
        let github_action = Some(
            panel_icon_btn(
                "about-github",
                "icons/github.svg",
                SharedString::from(i18n::t("about.repo.tip")),
                pal,
            )
            .on_click(cx.listener(|_v, _e, _w, _cx| {
                let _ = open::that(REPO_URL);
            }))
            .into_any_element(),
        );

        // 开源组件:只列直接依赖,版本号与传递依赖见 THIRD_PARTY_LICENSES.md
        const OSS: &[(&str, &str)] = &[
            ("gpui-kit / GPUI", "Apache-2.0"),
            ("ssh2 / libssh2", "MIT OR Apache-2.0 · BSD-3-Clause"),
            ("reqwest / rustls", "MIT OR Apache-2.0 · Apache-2.0 OR ISC"),
            ("keyring", "MIT OR Apache-2.0"),
            ("ropey", "MIT OR Apache-2.0"),
            ("regex", "MIT OR Apache-2.0"),
            ("serde / serde_json", "MIT OR Apache-2.0"),
            ("chrono", "MIT OR Apache-2.0"),
            ("sha2 / base64", "MIT OR Apache-2.0"),
            ("crossbeam-channel", "MIT OR Apache-2.0"),
            ("dirs / open / anyhow", "MIT OR Apache-2.0 · MIT"),
            ("toml", "MIT OR Apache-2.0"),
            ("Ionicons 5.0.0", "MIT"),
        ];
        let mut oss_rows: Vec<(String, AnyElement)> = OSS
            .iter()
            .map(|(name, lic)| {
                (
                    (*name).to_string(),
                    settings_row(pal, *name, Some((*lic).into()), div().into_any_element()),
                )
            })
            .collect();
        oss_rows.push(row(
            i18n::t("about.oss.full"),
            i18n::t("about.oss.full.desc"),
            host_btn(
                SharedString::from("about-oss"),
                i18n::t("common.open"),
                false,
                pal,
            )
            .on_click(cx.listener(|_v, _e, _w, _cx| {
                let _ = open::that(format!("{REPO_URL}/blob/main/THIRD_PARTY_LICENSES.md"));
            }))
            .into_any_element(),
        ));
        let update_rows: Vec<(String, AnyElement)> = vec![row(
            i18n::t("about.update.check"),
            i18n::t("about.update.check.desc"),
            update_control,
        )];

        // —— 分组数据:按分类/分组组织,搜索时跨分类过滤 ——
        type SettingsGroup = (
            usize,
            &'static str,
            Vec<(String, AnyElement)>,
            Option<AnyElement>,
        );
        let groups_all: Vec<SettingsGroup> = vec![
            (0, i18n::t("settings.group.interface"), appearance, None),
            (0, i18n::t("settings.group.typography"), typography, None),
            (0, i18n::t("settings.group.colors"), color_rows, None),
            (1, i18n::t("settings.group.loading"), load, None),
            (1, i18n::t("settings.group.memory"), memory, None),
            (2, i18n::t("settings.group.analysis"), cfg_entries, None),
            (
                3,
                i18n::t("settings.group.ssh_hosts"),
                host_entries,
                host_actions,
            ),
            (
                3,
                i18n::t("settings.group.url_creds"),
                url_cred_entries,
                None,
            ),
            (3, i18n::t("menu.recent"), recents_cfg, None),
            (
                4,
                i18n::t("settings.group.keyboard"),
                shortcut_entries,
                None,
            ),
            (5, i18n::t("about.group.app"), about_rows, None),
            (5, i18n::t("about.group.links"), link_rows, github_action),
            (5, i18n::t("about.group.oss"), oss_rows, None),
            (5, i18n::t("about.group.update"), update_rows, None),
        ];
        let page_titles = [
            i18n::t("settings.group.appearance"),
            i18n::t("settings.group.logging"),
            i18n::t("settings.group.analysis"),
            i18n::t("common.connect"),
            i18n::t("settings.group.shortcuts"),
            i18n::t("settings.group.about"),
        ];
        let page_descs = [
            i18n::t("settings.section.appearance.desc"),
            i18n::t("settings.section.logging.desc"),
            i18n::t("settings.section.analysis.desc"),
            i18n::t("settings.section.connections.desc"),
            i18n::t("settings.section.shortcuts.desc"),
            i18n::t("settings.section.about.desc"),
        ];

        let mut matched = 0usize;
        let mut groups: Vec<AnyElement> = Vec::new();
        for (page, group_title, entries, action) in groups_all {
            if !searching && page != tab_sel {
                continue;
            }
            let mut rows: Vec<AnyElement> = Vec::new();
            for (text, el) in entries {
                if setting_hit(&query, &[&text]) {
                    matched += 1;
                    rows.push(el);
                }
            }
            if rows.is_empty() {
                continue;
            }
            let title = if searching {
                format!("{} · {}", page_titles[page], group_title)
            } else {
                group_title.to_string()
            };
            groups.push(settings_group(title, action, rows, pal));
        }

        let body: AnyElement = if tab_sel == 2 && !searching {
            config_page
        } else {
            let mut scroll = div()
                .id("settings-scroll")
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .gap(px(16.0))
                .px(px(24.0))
                .py(px(16.0))
                .overflow_y_scroll();
            if groups.is_empty() {
                scroll = scroll.child(
                    div()
                        .py(px(24.0))
                        .text_size(px(theme::scaled(12.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(i18n::t("settings.search.no_match")),
                );
            } else {
                scroll = scroll.children(groups);
            }
            scroll.into_any_element()
        };

        // —— 页头 ——
        let head_title = if searching {
            i18n::t("settings.search.results").to_string()
        } else {
            page_titles[tab_sel].to_string()
        };
        let head_desc = if searching {
            if matched == 0 {
                i18n::tf("settings.search.empty", &[("query", query.to_string())])
            } else {
                i18n::tnf(
                    matched as u64,
                    "settings.search.matches",
                    &[("n", matched.to_string()), ("query", query.to_string())],
                )
            }
        } else {
            page_descs[tab_sel].to_string()
        };
        let head_text = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(theme::scaled(18.0)))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme::hex(pal.text1))
                    .child(head_title),
            )
            .child(
                div()
                    .text_size(px(theme::scaled(11.5)))
                    .text_color(theme::hex(pal.text2))
                    .child(head_desc),
            );
        // 分析配置页的 4 个动作收进页头右上角:图标 + 悬停提示,不复用旧版文字按钮
        let cfg_actions: Option<AnyElement> = if tab_sel == 2 && !searching {
            Some(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        panel_icon_btn(
                            "cfg-add",
                            "icons/config-new.svg",
                            SharedString::from(i18n::t("configs.new.tip")),
                            pal,
                        )
                        .on_click(cx.listener(|v, _e, window, cx| v.add_config(window, cx))),
                    )
                    .child(
                        panel_icon_btn(
                            "cfg-import",
                            "icons/config-import.svg",
                            SharedString::from(i18n::t("configs.import_json.tip")),
                            pal,
                        )
                        .on_click(cx.listener(|v, _e, _window, cx| v.import_configs(cx))),
                    )
                    .child(
                        panel_icon_btn(
                            "cfg-export",
                            "icons/config-export.svg",
                            SharedString::from(i18n::t("configs.export_json.tip")),
                            pal,
                        )
                        .on_click(cx.listener(|v, _e, _w, cx| v.export_configs(cx))),
                    )
                    .child(
                        panel_icon_btn(
                            "cfg-reset",
                            "icons/config-restore.svg",
                            SharedString::from(i18n::t("configs.restore_builtin.tip")),
                            pal,
                        )
                        .on_click(
                            cx.listener(|v, _e, window, cx| v.reset_all_builtins(window, cx)),
                        ),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };
        let page_head = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.0))
            .px(px(24.0))
            .pt(px(18.0))
            .pb(px(10.0))
            .child(head_text)
            .children(cfg_actions);

        // —— 左侧分类导航 ——
        let mut nav = div()
            .w(px(224.0))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .p(px(10.0))
            .bg(theme::hex(pal.bg_bar))
            .border_r_1()
            .border_color(theme::hex(pal.split))
            .child(
                div()
                    .w_full()
                    .pb(px(6.0))
                    .child(Input::new(&search_e).small().w_full()),
            );
        for (i, label) in [
            i18n::t("settings.group.appearance"),
            i18n::t("settings.group.logging"),
            i18n::t("settings.group.analysis"),
            i18n::t("common.connect"),
            i18n::t("settings.group.shortcuts"),
            i18n::t("settings.group.about"),
        ]
        .iter()
        .enumerate()
        {
            let on = tab_sel == i && !searching;
            nav = nav.child(
                div()
                    .id(SharedString::from(format!("set-nav-{i}")))
                    .cursor(CursorStyle::PointingHand)
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(5.0))
                    .bg(if on {
                        theme::hex(pal.fill)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .text_size(px(theme::scaled(12.5)))
                    .text_color(theme::hex(if on { pal.text1 } else { pal.text2 }))
                    .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                    .child(*label)
                    .on_click(
                        cx.listener(move |v, _e, window, cx| v.set_settings_tab(i, window, cx)),
                    ),
            );
        }
        nav = nav.child(div().flex_1()).child(
            div()
                .px(px(10.0))
                .pb(px(2.0))
                .text_size(px(theme::scaled(10.5)))
                .text_color(theme::hex(pal.text3))
                .child(if saved_recent {
                    i18n::t("settings.saved_auto")
                } else {
                    i18n::t("settings.changes_live")
                }),
        );

        // —— 顶部标题栏 ——
        let header = div()
            .flex()
            .items_center()
            .h(px(44.0))
            .px(px(16.0))
            .border_b_1()
            .border_color(theme::hex(pal.split))
            .bg(theme::hex(pal.bg_bar))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(theme::scaled(14.0)))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme::hex(pal.text1))
                            .child(i18n::t("settings.title")),
                    )
                    .child(
                        div()
                            .text_size(px(theme::scaled(11.0)))
                            .text_color(theme::hex(pal.text3))
                            .child(i18n::t("settings.changes_live.desc")),
                    ),
            );

        div()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .flex()
            .flex_col()
            .bg(theme::hex(pal.bg_layout))
            .child(header)
            .child(
                div().flex_1().min_h(px(0.0)).flex().child(nav).child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .min_h(px(0.0))
                        .flex()
                        .flex_col()
                        .child(page_head)
                        .child(body),
                ),
            )
            .into_any_element()
    }

    // ---- 检索联动 ----

    fn apply_search(&mut self, cx: &mut Context<Self>) {
        let text = self.search_input.read(cx).value().to_string();
        let ti = self.active;
        if let Some(t) = self.tabs.get_mut(ti) {
            if t.query != text {
                t.query = text;
                t.compile_search();
            }
        }
    }

    fn toggle_hidden(&mut self, idx: usize) {
        if let Some(t) = self.active_tab_mut() {
            if idx < LEVEL_COUNT {
                t.hidden[idx] = !t.hidden[idx];
                t.rebuild_view();
            }
        }
    }

    /// 配置多选下拉框:切换当前标签页的一项分析配置
    fn toggle_tab_config(&mut self, idx: usize) {
        let Some(cfg) = self.settings.configs.get(idx) else {
            return;
        };
        let id = cfg.id.clone();
        let configs = self.settings.configs.clone();
        if let Some(t) = self.active_tab_mut() {
            t.toggle_config(&configs, &id);
        }
        self.rows_cache.invalidate();
    }

    /// 自动换行开关:只影响当前标签页
    fn toggle_tab_wrap(&mut self) {
        if let Some(t) = self.active_tab_mut() {
            let on = !t.wrap;
            t.set_wrap(on);
        }
        self.rows_cache.invalidate();
    }

    fn open_config_menu(&mut self, x: f32, y: f32) {
        self.config_menu = Some((x, y));
    }

    fn close_config_menu(&mut self) {
        self.config_menu = None;
    }

    /// 默认自动换行(新标签页初始值):立即生效
    fn toggle_wrap_default(&mut self, cx: &mut Context<Self>) {
        self.settings.wrap = !self.settings.wrap;
        self.settings_apply(false, cx);
    }

    fn set_filter_mode(&mut self, on: bool) {
        if let Some(t) = self.active_tab_mut() {
            if t.filter_mode != on {
                t.filter_mode = on;
                t.compile_search();
            }
        }
    }

    fn toggle_regex(&mut self) {
        if let Some(t) = self.active_tab_mut() {
            t.regex = !t.regex;
            t.compile_search();
        }
    }

    fn toggle_case(&mut self) {
        if let Some(t) = self.active_tab_mut() {
            t.case_sensitive = !t.case_sensitive;
            t.compile_search();
        }
    }

    // ---- worker 消息轮询 ----

    /// 在鼠标位置上方弹出一条短提示(约 1.2s 后自动消失,由 poll 清理)
    fn show_toast(
        &mut self,
        text: impl Into<String>,
        at: gpui_kit::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.toast = Some(Toast {
            text: text.into(),
            at,
            until: Instant::now() + Duration::from_millis(1200),
        });
        cx.notify();
    }

    // ---- 更新检查 ----

    /// 启动一次版本检查:阻塞请求放到独立线程,不占用 UI 线程
    fn check_update(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update_state, UpdateState::Checking) {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.update_rx = Some(rx);
        self.update_state = UpdateState::Checking;
        std::thread::spawn(move || {
            // 接收端被丢弃时 send 会失败，忽略即可
            let _ = tx.send(fetch_latest_release());
        });
        cx.notify();
    }

    /// 取回在途检查的结果(由 poll 每帧调用)
    fn drain_update(&mut self) {
        let Some(rx) = self.update_rx.as_ref() else {
            return;
        };
        let Ok(res) = rx.try_recv() else { return };
        self.update_rx = None;
        let cur = env!("CARGO_PKG_VERSION");
        self.update_state = match res {
            // 404(尚无发布/仓库未公开)与空标签都当作「没有更新」
            Ok(None) => UpdateState::Latest(cur.to_string()),
            Ok(Some(rel)) => {
                if version_newer(&rel.tag, cur) {
                    UpdateState::Available(rel)
                } else {
                    UpdateState::Latest(rel.tag)
                }
            }
            Err(e) => UpdateState::Failed(e),
        };
    }

    fn poll(&mut self) {
        // 版本检查结果:由独立线程取回,这里只做无阻塞读取
        self.drain_update();
        // 短提示到期即清除(渲染侧用同一截止时间判断是否展示)
        if self
            .toast
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.until)
        {
            self.toast = None;
        }

        // 分帧重判定:每个标签最多 RELEVEL_CHUNK 行,期间状态栏提示
        for tab in self.tabs.iter_mut() {
            let Some(next) = tab.relevel else { continue };
            let end = tab
                .store
                .relevel_range(&tab.engine, next, next + RELEVEL_CHUNK);
            if end >= tab.store.len() {
                tab.relevel = None;
                tab.rebuild_matched();
                tab.rebuild_view();
            } else {
                tab.relevel = Some(end);
            }
        }

        let mut active_grew = 0usize;
        // 本帧发现的认证失败(标签页索引, 错误):轮询结束后再弹表单,避免嵌套借用
        let mut auth_fail: Option<(usize, String)> = None;
        // 本帧收到的未知主机确认请求(TOFU)
        let mut host_prompts: Vec<(u64, crate::source::HostKeyInfo)> = Vec::new();
        // 本帧活动标签的视图被重建(跳尾 Replace / 日志轮换),跟随中需要重新贴底
        let mut active_view_reset = false;
        // 本帧活动标签要求滚到顶部(加载全部)
        let mut active_scroll_top = false;
        for ti in 0..self.tabs.len() {
            let mut grew = 0usize;
            while let Ok(m) = self.tabs[ti].msg_rx.try_recv() {
                let tab = &mut self.tabs[ti];
                match m {
                    Msg::Append {
                        data,
                        offset,
                        size,
                        at,
                    } => {
                        let dropped_before = tab.store.dropped;
                        tab.exit_note = None;
                        let len_before = tab.store.len();
                        tab.store.push_bytes(&data, &tab.engine, at);
                        tab.store.bytes_loaded = offset;
                        tab.store.set_total(size);
                        tab.err = None;
                        let new_total = tab.store.len();
                        grew += new_total.saturating_sub(len_before);
                        if tab.store.dropped != dropped_before {
                            // 环形丢弃后索引整体前移,全量重建
                            tab.rebuild_matched();
                            tab.rebuild_view();
                        } else {
                            // 增量维护匹配集与可见行
                            let matcher = tab.matcher.clone();
                            for i in len_before..new_total {
                                let line = &tab.store.lines[i];
                                if let Some(m) = &matcher {
                                    if m.matches(&line.text) {
                                        tab.matched.push(i as u32);
                                    }
                                }
                                if tab.passes(line.level, &line.text) {
                                    tab.view.push(i as u32);
                                }
                            }
                        }
                    }
                    Msg::Replace { data, offset, size } => {
                        tab.store.reset_with(&data, offset, &tab.engine);
                        tab.store.set_total(size);
                        tab.err = None;
                        tab.pending_new = 0;
                        // 跳尾请求的响应:显示「已跳至末尾」banner;其他 Replace 清除旧 banner
                        if tab.pending_tail_banner {
                            tab.tail_banner = Some((offset, size));
                            tab.pending_tail_banner = false;
                            if ti == self.active {
                                active_view_reset = true;
                            }
                        } else {
                            tab.tail_banner = None;
                        }
                        if tab.pending_scroll_top {
                            tab.pending_scroll_top = false;
                            if ti == self.active {
                                active_scroll_top = true;
                            }
                        }
                        tab.rebuild_matched();
                        tab.rebuild_view();
                    }
                    Msg::Truncated { size } => {
                        tab.store.insert_separator(i18n::t("banner.rotation"));
                        tab.store.set_total(size);
                        tab.rebuild_matched();
                        tab.rebuild_view();
                        if ti == self.active {
                            active_view_reset = true;
                        }
                    }
                    Msg::Partial { loaded, total } => tab.loading = Some((loaded, total)),
                    Msg::Complete => tab.loading = None,
                    Msg::NoFollow => tab.no_follow = true,
                    Msg::Exited { code, retry_in } => {
                        let note = match (code, retry_in) {
                            (Some(c), Some(s)) => i18n::tf(
                                "command.retry.after_exit",
                                &[("code", c.to_string()), ("secs", s.to_string())],
                            ),
                            (Some(c), None) => {
                                i18n::tf("command.exited.code", &[("code", c.to_string())])
                            }
                            (None, Some(s)) => {
                                i18n::tf("command.retry.after_end", &[("secs", s.to_string())])
                            }
                            (None, None) => i18n::t("command.exited").to_string(),
                        };
                        let bad = code.is_some_and(|c| c != 0) || retry_in.is_some();
                        tab.exit_note = Some((note, bad));
                    }
                    Msg::Error { msg, fatal } => {
                        tab.err = Some(msg.clone());
                        if fatal {
                            tab.paused = true;
                            if is_auth_error(&msg) {
                                auth_fail = Some((ti, msg));
                            }
                        }
                    }
                    Msg::HostKeyPrompt(info) => host_prompts.push((tab.id, info)),
                }
            }
            let tab = &mut self.tabs[ti];
            if grew > 0 {
                if tab.follow {
                    if ti == self.active {
                        active_grew += grew;
                    }
                } else {
                    tab.pending_new = tab.pending_new.saturating_add(grew);
                }
            }
        }

        // 活动标签的认证失败:标记为待弹表单,由下一帧渲染前处理(需要 window/cx 建输入框)
        if !host_prompts.is_empty() {
            self.host_key_queue.extend(host_prompts);
            self.show_next_host_key();
        }
        if let Some((ti, msg)) = auth_fail {
            if ti == self.active && matches!(self.dialog, Dialog::None) {
                if let Some(t) = self.tabs.get(ti) {
                    if let Some(params) = t.params.clone() {
                        self.auth_retry = Some((t.id, params, msg));
                    }
                }
            }
        }

        // 活动标签:粘底跟随(对齐 egui 版语义)
        // - 跟随中且本轮有新行:立即贴底,并刷新 1s 宽限(吸收触板惯性产生的残余滚动)
        // - 跟随中但视图已离开底部超过 2 行,连续 2 次轮询才解除跟随(去抖)
        // - 不自动恢复:回到底部后由用户点「继续/跳到末尾」显式恢复(egui 同)
        if active_scroll_top {
            self.rows_cache.invalidate();
            self.list.scroll_to_item(0, ScrollStrategy::Top);
            self.suppress_follow_until = Some(Instant::now() + Duration::from_millis(1000));
        }
        let now = Instant::now();
        let following = self.tabs.get(self.active).is_some_and(|t| t.follow);
        if following && (active_grew > 0 || active_view_reset) {
            self.scroll_to_end();
            self.off_bottom_frames = 0;
            self.suppress_follow_until = Some(now + Duration::from_millis(1000));
        } else if following {
            let in_grace = self.suppress_follow_until.is_some_and(|u| now < u);
            if in_grace || self.at_bottom() {
                self.off_bottom_frames = 0;
            } else {
                self.off_bottom_frames = self.off_bottom_frames.saturating_add(1);
                if self.off_bottom_frames >= 2 {
                    self.off_bottom_frames = 0;
                    if let Some(t) = self.active_tab_mut() {
                        t.follow = false;
                    }
                }
            }
        } else {
            self.off_bottom_frames = 0;
        }
    }

    /// 贴底(跟随/跳尾用):按当前可见行数定位到最后一行。
    ///
    /// 不能用 `VirtualListScrollHandle::scroll_to_bottom()` —— 它读的是 handle
    /// 里上一次 prepaint 记录的 items_count,刚追加完新行时调用会退化成「滚到上一批的末行」,
    /// 于是视图与滚动条永远比真实末尾少一批行(表现为「日志在滚、滚动条停住不动」
    /// 以及跟随被 `at_bottom` 判定连续失败后自行解除)。
    fn scroll_to_end(&self) {
        let n = self.tabs.get(self.active).map_or(0, |t| t.view.len());
        if n == 0 {
            return;
        }
        self.list.scroll_to_item(n - 1, ScrollStrategy::Top);
    }

    /// 视图是否贴底:gpui 的 offset 自底部累加且符号约定随版本变化,
    /// 用与 max_offset 的绝对差值判断(内容不足一屏时两者同为 0)
    fn at_bottom(&self) -> bool {
        bottom_distance(self.list.offset().y, self.list.max_offset().y) <= px(theme::row_h() * 2.0)
    }

    // ---- 行渲染(虚拟列表回调,只渲染可见区间) ----

    fn row_sizes(&mut self, window: &mut Window, mono: &SharedString) -> Rc<Vec<Size<Pixels>>> {
        let ti = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(t0) = self.tabs.get(ti) else {
            return Rc::new(Vec::new());
        };
        let key_tab = t0.id;
        let n = t0.view.len();
        let gen = t0.view_gen;
        let wrap = t0.wrap;
        let show_ln = self.settings.show_line_numbers;
        // 时间戳列只对真实日志源开放(代码/JSON/欢迎页没有「输出时间」概念)
        let show_ts = self.settings.show_timestamps && t0.params.is_some();
        // 可用文本宽度:优先用滚动区实际宽度(首帧回退到窗口宽度)
        let viewport_w = {
            let w = self.list.bounds().size.width;
            if w > px(1.0) {
                w
            } else {
                window.viewport_size().width
            }
        };
        let gutter = px(24.0)
            + if show_ln { px(52.0) } else { px(0.0) }
            + if show_ts { px(TS_W) } else { px(0.0) };
        let wrap_w = f32::from((viewport_w - gutter).max(px(120.0)));
        let row_h = theme::row_h();
        let font_size = theme::log_scaled(13.0);

        // 自动换行:增量测量每行的真实高度(每帧限量,未测到的先用估算)
        {
            let tab = &mut self.tabs[ti];
            if wrap {
                if (tab.heights_w - wrap_w).abs() > 0.5 {
                    tab.heights.clear();
                    tab.heights_w = wrap_w;
                }
                let len = tab.store.len();
                if tab.heights.len() > len {
                    tab.heights.truncate(len);
                }
                let mut budget = MEASURE_CHUNK;
                let font = gpui_kit::font(mono.clone());
                while tab.heights.len() < len && budget > 0 {
                    let i = tab.heights.len();
                    let text = tab.store.lines[i].text.clone();
                    let h = measure_row_height(window, &text, &font, font_size, row_h, wrap_w);
                    tab.heights.push(h);
                    budget -= 1;
                }
            } else if !tab.heights.is_empty() {
                tab.heights.clear();
                tab.heights_w = 0.0;
            }
        }

        let tab = &self.tabs[ti];
        let key = (key_tab, n, gen, wrap, wrap_w, tab.heights.len());
        if self.rows_cache.key != key {
            let char_w = font_size * 0.6;
            let mut v = Vec::with_capacity(n);
            for &li in &tab.view {
                let height = if wrap {
                    tab.heights.get(li as usize).copied().unwrap_or_else(|| {
                        estimate_row_height(
                            &tab.store.lines[li as usize].text,
                            wrap_w,
                            char_w,
                            row_h,
                        )
                    })
                } else {
                    row_h
                };
                v.push(Size {
                    width: px(0.0),
                    height: px(height.max(row_h)),
                });
            }
            self.rows_cache = RowsCache {
                key,
                sizes: Rc::new(v),
            };
        }
        self.rows_cache.sizes.clone()
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let ti = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(t) = self.tabs.get(ti) else {
            return Vec::new();
        };
        let light = !cx.theme().mode.is_dark();
        let pal = theme::palette(light);
        let mono = self.log_font(cx);
        let hl_bg = theme::hex(&self.settings.highlight_hex(light));
        let hl_fg = theme::hex(self.settings.highlight_fg_hex(light));
        let link_color = theme::hex(pal.primary);
        let lv: Vec<Hsla> = self
            .settings
            .level_colors_hex(light)
            .iter()
            .map(|s| theme::hex(s))
            .collect();
        let show_ln = self.settings.show_line_numbers;
        // 时间戳列:只对真实日志源显示(与 row_sizes 的列宽计算保持一致)
        let show_ts = self.settings.show_timestamps && t.params.is_some();
        // 时区偏移每帧取一次,避免逐行查时区
        let ts_offset = if show_ts {
            crate::lines::local_offset_secs()
        } else {
            0
        };
        let row_h = theme::row_h();
        let wrap = t.wrap;
        let code_lang = t.code.as_ref().and_then(|d| d.lang);
        let sel = t.sel_range();
        let tab_id = t.id;
        let base = t.store.dropped;
        let sizes = self.rows_cache.sizes.clone();
        let entity = cx.entity();
        let mut spans_buf: Vec<(usize, usize)> = Vec::new();

        let mut out = Vec::with_capacity(range.end - range.start);
        for pos in range {
            let Some(&li) = t.view.get(pos) else { continue };
            let Some(line) = t.store.lines.get(li as usize) else {
                continue;
            };
            let h = sizes.get(pos).map(|s| s.height).unwrap_or(px(row_h));

            // 级别着色:全部级别使用设置中的级别色;WARN/ERROR/FATAL 另加整行底色
            let (bg, fg) = match line.level {
                crate::level::WARN => (theme::hex(pal.row_warn), lv[3]),
                crate::level::ERROR => (theme::hex(pal.row_error), lv[4]),
                crate::level::FATAL => (theme::hex(pal.row_fatal), lv[5]),
                SEP => (gpui_kit::transparent_black(), theme::hex(pal.text3)),
                l => (
                    gpui_kit::transparent_black(),
                    lv.get(l as usize)
                        .copied()
                        .unwrap_or_else(|| theme::hex(pal.text1)),
                ),
            };
            let selected = sel.is_some_and(|(a, b)| {
                let li = li as usize;
                li >= a && li <= b
            });
            let bg = if selected {
                theme::hex(pal.bg_selected)
            } else {
                bg
            };
            let bold = line.level == crate::level::FATAL;

            let mut row = div()
                .id(SharedString::from(format!("row-{}-{}", tab_id, pos)))
                .h(h)
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .px(px(12.0))
                .overflow_hidden()
                .bg(bg)
                .text_color(fg)
                .text_size(px(theme::log_scaled(13.0)))
                .line_height(px(row_h))
                .font_family(mono.clone());
            row = if wrap {
                row.whitespace_normal()
            } else {
                row.whitespace_nowrap()
            };
            if bold {
                row = row.font_weight(gpui_kit::FontWeight::BOLD);
            }
            // 行选中:单击选中一行,Shift/⌘ 点击扩选(选区供 ⌘C 与右键菜单使用)
            row = row.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |v, e: &MouseDownEvent, w, cx| {
                    v.select_row(li as usize, e.modifiers.shift, e.modifiers.platform, w, cx);
                }),
            );
            // 输出时间(本地):仅本次查看期间跟随读到的行有时间;历史加载的行留空占位
            if show_ts && line.level != SEP {
                let text = line
                    .at
                    .map(|ms| crate::lines::fmt_local_time(ms, ts_offset))
                    .unwrap_or_default();
                row = row.child(
                    div()
                        .w(px(TS_W))
                        .flex_shrink_0()
                        .pr(px(10.0))
                        .text_size(px(theme::log_scaled(11.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(text),
                );
            }

            if show_ln && line.level != SEP {
                row = row.child(
                    div()
                        .w(px(52.0))
                        .flex_shrink_0()
                        .pr(px(12.0))
                        .text_right()
                        .text_size(px(theme::log_scaled(11.0)))
                        .text_color(theme::hex(pal.text3))
                        .child(format!("{}", base + li as u64 + 1)),
                );
            }

            // 级别徽标:固定宽度着色胶囊,让级别在整行中一眼可辨
            if line.level != SEP && line.level != crate::level::NONE {
                let c = lv[(line.level as usize).min(6)];
                row = row.child(
                    div()
                        .flex_shrink_0()
                        .w(px(46.0))
                        .h(px(row_h))
                        .mr(px(6.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.0))
                        .bg(c.opacity(0.16))
                        .text_color(c)
                        .text_size(px(theme::log_scaled(10.0)))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(crate::level::level_name(line.level)),
                );
            }

            // 链接识别(URL / 文件路径):下划线 + 主题色;右键可「在新标签页打开」
            let line_links: Vec<links::Link> = if line.level == SEP || code_lang.is_some() {
                Vec::new()
            } else {
                links::detect(&line.text)
            };

            // 检索高亮 + 链接样式合并为互不重叠区间(SelectableText 要求升序无重叠)
            let mut searched: Vec<(usize, usize)> = Vec::new();
            if let Some(m) = &t.matcher {
                if !t.query.is_empty() {
                    m.spans(&line.text, &mut spans_buf);
                    searched.extend(spans_buf.iter().copied());
                }
            }
            let highlights = if let Some(lang) = code_lang {
                if searched.is_empty() {
                    code_row_highlights(lang, &line.text, pal)
                } else {
                    merge_row_highlights(&searched, &[], link_color, hl_bg, hl_fg)
                }
            } else {
                merge_row_highlights(&searched, &line_links, link_color, hl_bg, hl_fg)
            };

            // JSON 行:{ } 徐标 → 格式化后在新标签页打开
            if line.level != SEP && links::is_json_line(&line.text) {
                row = row.child(
                    div()
                        .id(SharedString::from(format!("json-{}-{}", tab_id, pos)))
                        .cursor(CursorStyle::PointingHand)
                        .flex_shrink_0()
                        .mr(px(6.0))
                        .px(px(4.0))
                        .rounded(px(3.0))
                        .bg(theme::hex(pal.fill))
                        .text_color(theme::hex(pal.text3))
                        .text_size(px(theme::log_scaled(10.0)))
                        .child("{}")
                        .on_click(cx.listener(move |v, _e, w, cx| {
                            v.open_json_tab(li as usize, w, cx);
                        })),
                );
            }

            // 可选中的日志文本(拖选 + Cmd/Ctrl+C 由窗口级选区层处理)
            // 文本容器自身记录窗口边界(prepaint 回填),供右键时定位鼠标下的链接
            let text_bounds = Rc::new(RefCell::new(None::<Bounds<Pixels>>));
            let text = SharedString::from(line.text.clone());
            let text_id = SharedString::from(format!("rowtext-{}-{}", tab_id, pos));
            let text_cell = div().flex_1().min_w(px(0.0)).overflow_hidden().child(
                SelectableText::new(text_id, text)
                    .document_order(pos as u64)
                    .highlights(highlights),
            );
            {
                let cell = text_bounds.clone();
                row = row.child(text_cell.on_prepaint(move |bounds, _window, _cx| {
                    *cell.borrow_mut() = Some(bounds);
                }));
            }

            // 右键菜单:链接动作 / 复制整行 / 复制选中文本 / 跳到末尾
            let row_text = line.text.clone();
            let menu_links = line_links;
            let menu_mono = mono.clone();
            let this = entity.clone();
            let row = row.context_menu(move |menu, window, _cx| {
                let mut menu = menu.min_w(px(230.0));

                // 指针在链接上时精确命中;否则回退到本行的链接列表(最多 3 个)
                let hit = text_bounds.borrow().as_ref().and_then(|bounds| {
                    link_at_cursor(window, &row_text, &menu_links, *bounds, wrap, &menu_mono)
                });
                let precise = hit.is_some();
                let targets: Vec<links::Link> = match hit {
                    Some(link) => vec![link],
                    None => menu_links.iter().take(3).cloned().collect(),
                };
                for link in &targets {
                    let target = row_text
                        .get(link.start..link.end)
                        .unwrap_or_default()
                        .to_string();
                    let kind = link.kind;
                    let this_open = this.clone();
                    let target_open = target.clone();
                    menu = menu.item(
                        PopupMenuItem::new(i18n::tf(
                            "links.open_in_tab",
                            &[("target", short_link(&target))],
                        ))
                        .on_click(move |_, _, cx| {
                            this_open
                                .update(cx, |v, _| v.open_from_link(kind, target_open.clone()));
                        }),
                    );
                    if !precise {
                        continue;
                    }
                    if kind == LinkKind::Url {
                        let target_browser = target.clone();
                        menu = menu.item(
                            PopupMenuItem::new(i18n::t("links.open_in_browser")).on_click(
                                move |_, _, _| {
                                    let _ = open::that(&target_browser);
                                },
                            ),
                        );
                    } else {
                        let target_local = target.clone();
                        menu = menu.item(
                            PopupMenuItem::new(i18n::t("links.open_local_file")).on_click(
                                move |_, _, _| {
                                    let _ = open::that(&target_local);
                                },
                            ),
                        );
                    }
                    let target_copy = target.clone();
                    menu = menu.item(PopupMenuItem::new(i18n::t("links.copy_link")).on_click(
                        move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(target_copy.clone()));
                        },
                    ));
                }
                if !targets.is_empty() {
                    menu = menu.separator();
                }

                let copy_row = row_text.clone();
                let this_end = this.clone();
                menu.item(PopupMenuItem::new(i18n::t("links.copy_line")).on_click(
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_row.clone()));
                    },
                ))
                .item(
                    PopupMenuItem::new(i18n::t("links.copy_selection")).on_click(
                        |_, window, cx| {
                            let sel = gpui_kit::base::TextSelection::selected_text(window, cx);
                            if !sel.is_empty() {
                                cx.write_to_clipboard(ClipboardItem::new_string(sel));
                            }
                        },
                    ),
                )
                .separator()
                .item(
                    PopupMenuItem::new(i18n::t("toolbar.jump_to_end")).on_click(move |_, _, cx| {
                        this_end.update(cx, |v, _| v.jump_to_end());
                    }),
                )
            });
            out.push(row.into_any_element());
        }
        out
    }

    // ---- 滚动行为 ----

    /// 把某行 JSON 格式化后开到新标签页(egui 版同)
    fn open_json_tab(&mut self, li: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self
            .tabs
            .get(self.active)
            .and_then(|t| t.store.lines.get(li))
        else {
            return;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.text.trim()) else {
            return;
        };
        let Ok(pretty) = serde_json::to_string_pretty(&v) else {
            return;
        };
        let mut tab = Tab::text(
            &self.settings,
            format!("JSON · {}", self.tabs[self.active].display_title()),
            &pretty,
        );
        tab.id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.rows_cache.invalidate();
        self.list.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// 跳到末尾:重取尾部并恢复跟随(暂停中也一并恢复,否则新日志不会追加)
    fn jump_to_end(&mut self) {
        {
            let Some(t) = self.active_tab_mut() else {
                return;
            };
            let _ = t.cmd_tx.send(Cmd::SeekEnd);
            t.follow = true;
            t.pending_new = 0;
            t.paused = false;
            t.pending_tail_banner = true;
            let _ = t.cmd_tx.send(Cmd::Pause(false));
        }
        self.suppress_follow_until = Some(Instant::now() + Duration::from_millis(1000));
        self.off_bottom_frames = 0;
        self.scroll_to_end();
    }

    /// 从头部全量加载:进入浏览状态(停止跟随但不暂停 worker),加载完停在顶部。
    /// 命令源从输出缓存(spool)回放,文件源照旧重读文件;两者语义一致。
    fn load_all(&mut self) {
        if let Some(t) = self.active_tab_mut() {
            let _ = t.cmd_tx.send(Cmd::SeekStart);
            t.follow = false;
            t.pending_new = 0;
            t.tail_banner = None;
            t.loading = None;
            t.pending_scroll_top = true;
        }
    }

    /// 关闭「已跳至末尾」banner
    fn dismiss_tail_banner(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.active_tab_mut() {
            t.tail_banner = None;
        }
        cx.notify();
    }

    /// 跟随开关(对齐 egui 版:暂停跟随时同时暂停 worker,恢复时继续并贴底)
    fn toggle_follow(&mut self) {
        let follow = {
            let Some(t) = self.active_tab_mut() else {
                return;
            };
            t.follow = !t.follow;
            t.pending_new = 0;
            t.paused = !t.follow; // 跟进中→暂停;暂停→恢复
            let _ = t.cmd_tx.send(Cmd::Pause(t.paused));
            t.follow
        };
        if follow {
            self.suppress_follow_until = Some(Instant::now() + Duration::from_millis(1000));
            self.off_bottom_frames = 0;
            self.scroll_to_end();
        }
    }

    /// 清屏:清空当前标签的行与派生视图,worker 继续追加。
    /// 命令源同时清掉输出缓存(临时文件),保证“从头加载”不会把已清掉的内容找回来。
    fn clear_screen(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.active_tab_mut() else {
            return;
        };
        t.store.clear();
        t.view.clear();
        t.matched.clear();
        t.match_cursor = 0;
        t.pending_new = 0;
        t.sel_anchor = None;
        t.sel_focus = None;
        t.tail_banner = None;
        t.pending_tail_banner = false;
        t.pending_scroll_top = false;
        if t.stream {
            let _ = t.cmd_tx.send(Cmd::Clear);
        }
        cx.notify();
    }

    /// 代码视图:只读 ⇄ 编辑(首次进入时用当前文本创建多行编辑器)
    fn toggle_code_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ni = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(t) = self.tabs.get_mut(ni) else {
            return;
        };
        let Some(d) = t.code.as_mut() else { return };
        if d.editor.is_none() {
            let lang = d.lang;
            let text = d.text.clone();
            let light = !cx.theme().mode.is_dark();
            let palette = crate::code::CodePalette::from_theme(theme::palette(light));
            let ed = cx.new(|cx| EditorState::new(window, cx));
            ed.update(cx, |s, cx| {
                if let Some(lang) = lang {
                    // 语言名只用于状态展示;真正的着色由下面的 highlighter 提供
                    s.set_highlighter(lang.label(), cx);
                    let factory: InputHighlighterFactory = std::rc::Rc::new(move |_name: &str| {
                        Some(Box::new(crate::code::CodeHighlighter::new(lang, palette))
                            as Box<dyn InputHighlighter>)
                    });
                    s.set_highlighter_factory(factory, cx);
                }
                s.set_value(text.clone(), window, cx);
            });
            d.editor = Some(ed);
        }
        d.editing = !d.editing;
        let (editing, ed) = (d.editing, d.editor.clone());
        if editing {
            if let Some(ed) = ed {
                let h = ed.read(cx).focus_handle(cx);
                window.focus(&h, cx);
            }
        } else {
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    /// 保存代码视图到磁盘,并刷新只读渲染的行缓存
    fn save_code(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let ni = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(t) = self.tabs.get_mut(ni) else {
            return;
        };
        let Some(ed) = t.code.as_ref().and_then(|d| d.editor.clone()) else {
            return;
        };
        let text = ed.read(cx).value().to_string();
        let Some(path) = t.code.as_ref().map(|d| d.path.clone()) else {
            return;
        };
        match std::fs::write(&path, &text) {
            Ok(()) => {
                if let Some(d) = t.code.as_mut() {
                    d.text = text.clone();
                }
                t.err = None;
                t.store.clear();
                t.store
                    .push_bytes(text.as_bytes(), &crate::code::no_level_engine(), None);
                t.rebuild_view();
                t.rebuild_matched();
                self.rows_cache.invalidate();
            }
            Err(e) => t.err = Some(i18n::tf("error.save_failed", &[("e", e.to_string())])),
        }
        cx.notify();
    }

    /// 检索结果跳转:在 matched 中循环,把命中行滚到视口中间,并解除跟随(egui 版同)
    fn jump_match(&mut self, next: bool, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get(self.active) else {
            return;
        };
        if t.matched.is_empty() {
            return;
        }
        let len = t.matched.len();
        let idx = if next {
            (t.match_cursor + 1) % len
        } else {
            (t.match_cursor + len - 1) % len
        };
        let real = t.matched[idx] as usize;
        // 过滤模式下命中行可能不可见,退化为不改位置
        let Some(row) = t.view.iter().position(|&r| r as usize == real) else {
            return;
        };
        if let Some(t) = self.active_tab_mut() {
            t.match_cursor = idx;
            t.follow = false;
        }
        self.suppress_follow_until = Some(Instant::now() + Duration::from_millis(300));
        self.list.scroll_to_item(row, ScrollStrategy::Center);
        cx.notify();
    }

    /// 行选中:普通点击重置为单行,Shift/⌘ 点击扩选(egui 版同)
    fn select_row(
        &mut self,
        li: usize,
        extend: bool,
        cmd: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.active_tab_mut() else {
            return;
        };
        if (extend || cmd) && t.sel_anchor.is_some() {
            t.sel_focus = Some(li);
        } else {
            t.sel_anchor = Some(li);
            t.sel_focus = Some(li);
        }
        let h = self.focus_handle.clone();
        window.focus(&h, cx);
        cx.notify();
    }

    /// 复制选中行的文本(⌘C);未选中时不动作
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.get(self.active) else {
            return;
        };
        let Some((a, b)) = t.sel_range() else { return };
        let text = t
            .store
            .lines
            .get(a..=b)
            .map(|rows| {
                rows.iter()
                    .map(|l| l.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// 开始录制某动作的新快捷键(焦点交还根节点,否则收不到按键)
    fn begin_shortcut_capture(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(d) = self.settings_panel.as_mut() {
            d.capture = Some(action.to_string());
            d.sc_notice = Some(i18n::t("shortcut.record_hint").into());
        }
        let h = self.focus_handle.clone();
        window.focus(&h, cx);
        cx.notify();
    }

    fn cancel_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = self.settings_panel.as_mut() {
            d.capture = None;
        }
        cx.notify();
    }

    /// 写入新快捷键;与其他动作撞键时互换,保证一键一用
    fn assign_shortcut(&mut self, action: &str, spec: String, cx: &mut Context<Self>) {
        let old = self.settings.shortcut(action);
        let conflict = store::SHORTCUT_ACTIONS
            .iter()
            .map(|(a, desc)| (*a, *desc))
            .find(|(a, _)| *a != action && self.settings.shortcut(a) == spec);
        if let Some((other, _)) = conflict {
            self.settings.shortcuts.insert(other.to_string(), old);
        }
        self.settings
            .shortcuts
            .insert(action.to_string(), spec.clone());
        crate::store::save(&self.settings);
        if let Some(d) = self.settings_panel.as_mut() {
            d.capture = None;
            d.sc_notice = Some(match conflict {
                Some((_, desc)) => i18n::tf("shortcut.swapped", &[("desc", i18n::td(desc))]),
                None => i18n::tf("shortcut.set", &[("shortcut", shortcut_label(&spec))]),
            });
        }
        cx.notify();
    }

    fn reset_shortcut(&mut self, action: &str, cx: &mut Context<Self>) {
        self.settings.shortcuts.remove(action);
        crate::store::save(&self.settings);
        if let Some(d) = self.settings_panel.as_mut() {
            d.capture = None;
            d.sc_notice = Some(i18n::t("shortcut.restored").into());
        }
        cx.notify();
    }

    /// 根节点快捷键(egui 版同):⌘T 打开源、⌘W 关标签、⌘F 聚焦检索、⌘K 清屏、
    /// ⌘, 设置、⌘C 复制选中行、Esc 关弹层
    fn handle_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &e.keystroke;
        if ks.key == "escape" {
            // 录制中优先取消录制
            if let Some(d) = self.settings_panel.as_mut() {
                if d.capture.take().is_some() {
                    d.sc_notice = Some(i18n::t("common.discard").into());
                    cx.notify();
                    return;
                }
            }
            if !matches!(self.dialog, Dialog::None) {
                self.close_dialog();
                cx.notify();
            } else if self.config_menu.is_some() {
                self.close_config_menu();
            } else if self.recents_confirm {
                self.welcome_cancel_clear(cx);
            }
            return;
        }
        // 快捷键录制:下一次带修饰键的组合写入设置
        if let Some(action) = self.settings_panel.as_ref().and_then(|d| d.capture.clone()) {
            let secondary = ks.modifiers.secondary(cx);
            match keystroke_spec(ks, secondary) {
                Some(spec) => self.assign_shortcut(&action, spec, cx),
                None => {
                    if let Some(d) = self.settings_panel.as_mut() {
                        d.sc_notice = Some(i18n::t("shortcut.modifier_required").into());
                    }
                    cx.notify();
                }
            }
            return;
        }
        // 普通分发:按设置里的绑定匹配(未改过时即默认 ⌘T/⌘W/…)
        let secondary = ks.modifiers.secondary(cx);
        let Some(spec) = keystroke_spec(ks, secondary) else {
            return;
        };
        let hit = store::SHORTCUT_ACTIONS
            .iter()
            .map(|(a, _)| *a)
            .find(|a| self.settings.shortcut(a) == spec);
        match hit {
            Some("new_tab") => self.add_blank_tab(window, cx),
            Some("close_tab") => self.close_tab(self.active, window, cx),
            Some("find") => {
                let h = self.search_input.read(cx).focus_handle(cx);
                window.focus(&h, cx);
                cx.notify();
            }
            Some("clear") => self.clear_screen(cx),
            Some("settings") => self.open_settings_tab(None, window, cx),
            Some("copy_row") => self.copy_selection(cx),
            Some("save_code") => self.save_code(window, cx),
            _ => {}
        }
    }

    // ---- 主渲染 ----

    fn view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.poll();
        self.maybe_open_auth_retry(window, cx);

        let light = !cx.theme().mode.is_dark();
        let pal: &Palette = theme::palette(light);
        let ui_font = self.ui_font(cx);
        let mono_font = self.log_font(cx);
        let lv: Vec<Hsla> = self
            .settings
            .level_colors_hex(light)
            .iter()
            .map(|s| theme::hex(s))
            .collect();
        let names = level_labels();

        // 标签条数据(先收集成小数据,避免与 listener 借用冲突)
        let tabs_meta: Vec<(u64, String, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id, t.display_title(), i == self.active))
            .collect();
        let ti = self.active.min(self.tabs.len().saturating_sub(1));
        let cur = self.tabs.get(ti);
        let counts = cur.map(|t| t.store.counts).unwrap_or([0u64; LEVEL_COUNT]);
        let hidden = cur.map(|t| t.hidden).unwrap_or([false; LEVEL_COUNT]);
        let follow = cur.map(|t| t.follow).unwrap_or(false);
        let paused = cur.map(|t| t.paused).unwrap_or(false);
        let pending_new = cur.map(|t| t.pending_new).unwrap_or(0);
        let filter_mode = cur.map(|t| t.filter_mode).unwrap_or(false);
        let regex_on = cur.map(|t| t.regex).unwrap_or(false);
        let case_on = cur.map(|t| t.case_sensitive).unwrap_or(false);
        let matched_n = cur.map(|t| t.matched.len()).unwrap_or(0);
        let loading = cur.and_then(|t| t.loading);
        let tail_banner = cur.and_then(|t| t.tail_banner);
        let no_follow = cur.map(|t| t.no_follow).unwrap_or(false);
        let err_text = cur.and_then(|t| t.err.clone()).map(|e| i18n::td(&e));
        let match_err = cur.and_then(|t| t.match_err.clone());
        let has_tabs = !self.tabs.is_empty();
        let is_welcome = cur.is_some_and(|t| t.welcome);
        let is_settings = cur.is_some_and(|t| t.settings);

        // —— 代码文件视图:语言标记、只读/编辑切换、保存 ——
        let code_doc = cur.and_then(|t| t.code.as_ref());
        let is_code = code_doc.is_some();
        // Markdown:只读态渲染预览,编辑态显示源码(不设单独切换按钮)
        let is_md = code_doc.is_some_and(|d| d.lang == Some(crate::code::Lang::Markdown));
        let code_bar: Option<AnyElement> = code_doc.map(|d| {
            let editing = d.editing;
            let lang = d
                .lang
                .map(|l| l.label())
                .unwrap_or(i18n::t("code.plain_text"));
            let mut bar = div().flex().items_center().gap(px(6.0)).child(
                div()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .bg(theme::hex(pal.fill))
                    .text_size(px(theme::scaled(10.5)))
                    .text_color(theme::hex(pal.text2))
                    .child(lang),
            );
            bar = bar.child(
                toolbar_icon_btn(
                    "code-edit",
                    "icons/edit.svg",
                    SharedString::from(if editing {
                        i18n::t("code.preview.readonly")
                    } else {
                        i18n::t("code.edit_source")
                    }),
                    editing,
                    pal,
                )
                .on_click(cx.listener(|v, _e, w, cx| v.toggle_code_edit(w, cx))),
            );
            if editing {
                bar = bar.child(
                    div()
                        .id("code-save")
                        .cursor(CursorStyle::PointingHand)
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(26.0))
                        .h(px(26.0))
                        .rounded(px(4.0))
                        .bg(theme::hex(pal.primary))
                        .hover(|s| s.bg(theme::hexa(pal.primary, 0.85)))
                        .child(
                            svg()
                                .path("icons/save.svg")
                                .w(px(14.0))
                                .h(px(14.0))
                                .text_color(theme::hex(pal.on_primary)),
                        )
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::element(|_, _| {
                                div()
                                    .text_size(px(theme::scaled(11.0)))
                                    .child(i18n::t("code.save"))
                            })
                            .build(window, cx)
                        })
                        .on_click(cx.listener(|v, _e, w, cx| v.save_code(w, cx))),
                );
            }
            if d.lang == Some(crate::code::Lang::Markdown)
                && !d.editing
                && d.text.len() as u64 > crate::code::MAX_PREVIEW_BYTES
            {
                bar = bar.child(
                    div()
                        .text_size(px(theme::scaled(10.5)))
                        .text_color(theme::hex(pal.text2))
                        .child(i18n::t("code.too_large")),
                );
            }
            bar.into_any_element()
        });
        let code_editor: Option<AnyElement> = code_doc.and_then(|d| {
            if !d.editing {
                return None;
            }
            let ed = d.editor.clone()?;
            // 多行输入元素按「父容器 100% 高度 + 绝对定位」布局:
            // 外层必须是确定高度的定位容器,否则只摊开一行
            Some(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .h_full()
                    .w_full()
                    .relative()
                    .overflow_hidden()
                    .child(
                        div()
                            .size_full()
                            .p(px(10.0))
                            .text_size(px(theme::log_scaled(13.0)))
                            .font_family(mono_font.clone())
                            .child(Editor::new(&ed)),
                    )
                    .into_any_element(),
            )
        });
        // Markdown 只读态:直接渲染预览(编辑态走上面的源码编辑器)
        let md_preview: Option<AnyElement> = code_doc.and_then(|d| {
            if d.editing
                || d.lang != Some(crate::code::Lang::Markdown)
                || d.text.len() as u64 > crate::code::MAX_PREVIEW_BYTES
            {
                return None;
            }
            // 图片按「文档所在目录」解析:markdown 里的相对路径本身不是 URI
            let base_dir = d
                .path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            // 代码块着色用的配色跟随当前主题
            crate::code::set_highlight_palette(crate::code::CodePalette::from_theme(pal));
            Some(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .h_full()
                    .w_full()
                    .relative()
                    .child(
                        div()
                            .size_full()
                            .px(px(22.0))
                            .py(px(14.0))
                            .text_size(px(theme::scaled(14.0)))
                            .font_family(ui_font.clone())
                            .child(
                                TextView::markdown(
                                    ElementId::Name(format!("md-preview-{ti}").into()),
                                    SharedString::from(crate::code::stub_missing_images(
                                        &d.text, &base_dir,
                                    )),
                                )
                                .scrollable(true)
                                .selectable(true)
                                .image_source(
                                    move |uri: &SharedUri| {
                                        let src = uri.to_string();
                                        // 远程与内嵌 data URL 交回默认处理
                                        if src.starts_with("http://")
                                            || src.starts_with("https://")
                                            || src.starts_with("data:")
                                        {
                                            return ImageSource::from(src);
                                        }
                                        let raw = src.strip_prefix("file://").unwrap_or(&src);
                                        let p = std::path::Path::new(raw);
                                        let full = if p.is_absolute() {
                                            p.to_path_buf()
                                        } else {
                                            base_dir.join(p)
                                        };
                                        ImageSource::from(full)
                                    },
                                ),
                            ),
                    )
                    .into_any_element(),
            )
        });
        let wrap_on = cur.is_some_and(|t| t.wrap);
        let cfg_on = cur.map_or(0, |t| t.enabled_count());
        let cfg_total = self.settings.configs.len();

        // —— 标签条 ——
        let ent_tab = cx.entity();
        let mut bar: Vec<AnyElement> = Vec::new();
        for (idx, (id, title, sel)) in tabs_meta.iter().enumerate() {
            bar.push(
                div()
                    .id(SharedString::from(format!("tab-{id}")))
                    .cursor(CursorStyle::PointingHand)
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .pl(px(12.0))
                    .pr(px(4.0))
                    .rounded_t(px(6.0))
                    .bg(if *sel {
                        theme::hex(pal.bg_layout)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .hover(|s| s.bg(theme::hex(pal.fill)))
                    .on_click(cx.listener(move |v, _e, window, cx| v.switch_tab(idx, window, cx)))
                    // 右键:关闭 / 关闭其他(egui 版同)
                    .context_menu({
                        let ent = ent_tab.clone();
                        move |menu, _window, _cx| {
                            menu.item(PopupMenuItem::new(i18n::t("common.close")).on_click({
                                let ent = ent.clone();
                                move |_, w, cx| {
                                    ent.update(cx, |v, cx| v.close_tab(idx, w, cx));
                                }
                            }))
                            .item(
                                PopupMenuItem::new(i18n::t("tab.close_others")).on_click({
                                    let ent = ent.clone();
                                    move |_, w, cx| {
                                        ent.update(cx, |v, cx| v.close_other_tabs(idx, w, cx));
                                    }
                                }),
                            )
                        }
                    })
                    .child(
                        div()
                            .max_w(px(160.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::scaled(12.5)))
                            .text_color(if *sel {
                                theme::hex(pal.text1)
                            } else {
                                theme::hex(pal.text2)
                            })
                            .child(title.clone()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("tabx-{id}")))
                            .cursor(CursorStyle::PointingHand)
                            .flex()
                            .items_center()
                            .justify_center()
                            .px(px(4.0))
                            .rounded(px(3.0))
                            .text_size(px(theme::scaled(11.0)))
                            .text_color(theme::hex(pal.text3))
                            .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                            .child(
                                svg()
                                    .path("icons/close.svg")
                                    .w(px(11.0))
                                    .h(px(11.0))
                                    .text_color(theme::hex(pal.text3)),
                            )
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::element(|_, _| {
                                    div()
                                        .text_size(px(theme::scaled(11.0)))
                                        .child(i18n::t("tab.close.tip"))
                                })
                                .build(window, cx)
                            })
                            .on_click(cx.listener(move |v, _e, window, cx| {
                                cx.stop_propagation();
                                v.close_tab(idx, window, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        bar.push(
            div()
                .id("btn-new")
                .cursor(CursorStyle::PointingHand)
                .px(px(10.0))
                .h_full()
                .flex()
                .items_center()
                .text_size(px(theme::scaled(15.0)))
                .text_color(theme::hex(pal.text2))
                .hover(|s| s.bg(theme::hex(pal.fill)))
                .child("+")
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::element(|_, _| {
                        div()
                            .text_size(px(theme::scaled(11.0)))
                            .child(i18n::t("shortcut.new_tab"))
                    })
                    .build(window, cx)
                })
                .on_click(cx.listener(|v, _e, window, cx| v.add_blank_tab(window, cx)))
                .into_any_element(),
        );
        bar.push(div().flex_1().into_any_element());
        bar.push(
            div()
                .id("btn-settings")
                .cursor(CursorStyle::PointingHand)
                .flex()
                .items_center()
                .justify_center()
                .w(px(26.0))
                .h(px(26.0))
                .mr(px(8.0))
                .rounded(px(4.0))
                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                .child(
                    svg()
                        .path("icons/settings.svg")
                        .w(px(15.0))
                        .h(px(15.0))
                        .text_color(theme::hex(pal.text2)),
                )
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::element(|_, _| {
                        div()
                            .text_size(px(theme::scaled(11.0)))
                            .child(i18n::t("shortcut.open_settings"))
                    })
                    .build(window, cx)
                })
                .on_click(cx.listener(|v, _e, window, cx| v.open_settings_tab(None, window, cx)))
                .into_any_element(),
        );

        // —— 工具栏:检索 + 级别 chips + 分析配置 + 自动换行 + 动作(图标化) ——
        // 级别芯片三档:完整(INFO · 12)→ 首字母 + 计数(I · 12)→ 仅首字母(I)。
        // 按各级真实字号实测宽度,窗口装不下就降一档,保证工具栏不溢出被裁切
        let chip_style = {
            let ts = window.text_system().clone();
            let font = gpui_kit::font(ui_font.clone());
            // 按指定字号量一段文字(字号必须与渲染处一致,否则宽度会算偏)
            let w_at = move |t: &str, size: f32| -> f32 {
                if t.is_empty() {
                    return 0.0;
                }
                let run = TextRun {
                    len: t.len(),
                    font: font.clone(),
                    color: gpui_kit::transparent_black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                f32::from(ts.layout_line(t, px(size), &[run], None).width)
            };
            let chip_size = theme::scaled(11.0);
            let btn_size = theme::scaled(12.0);
            // 七个芯片的总宽(文字 + 左右内边距 + 芯片间距):完整档 px(8)、其余档 px(5);
            // 仅首字母档是兜底,不需要再量宽度
            let (mut full_w, mut short_w) = (0.0f32, 0.0f32);
            for i in 0..LEVEL_COUNT {
                let head: String = names[i]
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                full_w += w_at(&format!("{} · {}", names[i], counts[i]), chip_size) + 16.0 + 4.0;
                short_w += w_at(&format!("{head} · {}", counts[i]), chip_size) + 10.0 + 4.0;
            }
            // 匹配翻页(▲▼)只在「高亮模式且命中非空」时出现
            let nav_w = if !filter_mode && matched_n > 0 {
                w_at("▲", chip_size) + w_at("▼", chip_size) + 2.0 * 12.0 + 2.0
            } else {
                0.0
            };
            // 其余控件:检索框 240 + 正则/大小写 + 高亮|过滤 + 匹配翻页 + 配置(图标/计数/箭头)
            // + 换行 + 四个动作图标 + 工具栏间距 + 左右内边距 + 安全余量
            let others = 240.0
                + w_at(".*", btn_size) + 16.0
                + w_at("Aa", btn_size) + 16.0
                + w_at(i18n::t("search.highlight"), btn_size) + 20.0
                + w_at(i18n::t("search.filter"), btn_size) + 20.0
                + nav_w
                + 5.0 + 14.0 + 3.0
                + w_at(&format!("{cfg_on}/{cfg_total}"), theme::scaled(10.5))
                + 3.0 + 9.0 + 5.0
                + w_at(i18n::t("wrap.label"), chip_size) + 16.0
                + 4.0 * 26.0 + 3.0 * 6.0
                + 7.0 * 6.0 + 20.0
                // 安全余量:估算与实际排版有几十像素出入(组件内边距/边框),留足避免裁字
                + 48.0;
            pick_chip_style(
                f32::from(window.viewport_size().width),
                others,
                full_w,
                short_w,
            )
        };
        let mut chips: Vec<AnyElement> = Vec::new();
        for idx in 0..LEVEL_COUNT {
            let color = lv[idx];
            let on = !hidden[idx];
            // 级别 tag:文字用级别配置色、底色用主题背景色;选中时底部画级别色指示线
            let lname = names[idx].clone();
            let head: String = lname
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default();
            let label = match chip_style {
                ChipStyle::Full => format!("{} · {}", lname, counts[idx]),
                ChipStyle::Short => format!("{head} · {}", counts[idx]),
                ChipStyle::Letter => head,
            };
            let tip = SharedString::from(i18n::tnf(
                counts[idx],
                "tab.title_rows",
                &[
                    ("name", lname.to_string()),
                    ("n", counts[idx].to_string()),
                    (
                        "action",
                        if on {
                            i18n::t("search.hide").to_string()
                        } else {
                            i18n::t("search.show").to_string()
                        },
                    ),
                ],
            ));
            let mut chip = div()
                .id(SharedString::from(format!("chip-{idx}")))
                .cursor(CursorStyle::PointingHand)
                .relative()
                .h(px(26.0))
                .flex()
                .items_center()
                .px(px(if chip_style == ChipStyle::Full {
                    8.0
                } else {
                    5.0
                }))
                .rounded(px(4.0))
                .bg(theme::hex(pal.bg_bar))
                .text_size(px(theme::scaled(11.0)))
                .text_color(color)
                .font_weight(if on {
                    gpui_kit::FontWeight::MEDIUM
                } else {
                    gpui_kit::FontWeight::NORMAL
                })
                .hover(|s| s.bg(theme::hex(pal.fill)))
                .child(label)
                .tooltip(move |window, cx| {
                    let tip = tip.clone();
                    gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                        div().text_size(px(theme::scaled(11.0))).child(tip.clone())
                    })
                    .build(window, cx)
                })
                .on_click(cx.listener(move |v, _e, _w, _cx| v.toggle_hidden(idx)));
            if on {
                chip = chip.child(
                    div()
                        .absolute()
                        .left(px(4.0))
                        .right(px(4.0))
                        .bottom(px(0.0))
                        .h(px(2.0))
                        .rounded_full()
                        .bg(color),
                );
            }
            chips.push(chip.into_any_element());
        }

        // 分析配置多选下拉框(当前标签页):图标 + 启用数/总数,完整信息走 tooltip
        let cfg_btn = div()
            .id("btn-configs")
            .cursor(CursorStyle::PointingHand)
            .h(px(26.0))
            .flex()
            .items_center()
            .gap(px(3.0))
            .px(px(5.0))
            .rounded(px(4.0))
            .bg(if self.config_menu.is_some() {
                theme::hexa(pal.primary, 0.12)
            } else {
                gpui_kit::transparent_black()
            })
            .text_size(px(theme::scaled(10.5)))
            .text_color(theme::hex(pal.primary))
            .hover(|s| s.bg(theme::hex(pal.fill_hover)))
            .child(
                svg()
                    .path("icons/configs.svg")
                    .w(px(14.0))
                    .h(px(14.0))
                    .text_color(theme::hex(pal.primary)),
            )
            .child(format!("{cfg_on}/{cfg_total}"))
            .child(
                svg()
                    .path("icons/chevron-down.svg")
                    .w(px(9.0))
                    .h(px(9.0))
                    .text_color(theme::hex(pal.text3)),
            )
            .tooltip(move |window, cx| {
                let tip = SharedString::from(i18n::tf(
                    "analysis.summary",
                    &[("on", cfg_on.to_string()), ("total", cfg_total.to_string())],
                ));
                gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                    div().text_size(px(theme::scaled(11.0))).child(tip.clone())
                })
                .build(window, cx)
            })
            .on_click(cx.listener(|v, e: &ClickEvent, _w, _cx| {
                let pos = e.position();
                v.open_config_menu(f32::from(pos.x), f32::from(pos.y) + 6.0);
            }));

        // 自动换行开关(当前标签页;设置里的默认值只影响新标签页):文字按钮,开启态用主题色
        let wrap_btn = div()
            .id("btn-wrap")
            .cursor(CursorStyle::PointingHand)
            .flex()
            .items_center()
            .h(px(26.0))
            .px(px(8.0))
            .rounded(px(4.0))
            .bg(if wrap_on {
                theme::hexa(pal.primary, 0.12)
            } else {
                gpui_kit::transparent_black()
            })
            .text_size(px(theme::scaled(11.0)))
            .text_color(if wrap_on {
                theme::hex(pal.primary)
            } else {
                theme::hex(pal.text2)
            })
            .hover(|s| s.bg(theme::hex(pal.fill_hover)))
            .child(i18n::t("wrap.label"))
            .tooltip(move |window, cx| {
                let tip = SharedString::from(if wrap_on {
                    i18n::t("wrap.on")
                } else {
                    i18n::t("wrap.off")
                });
                gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                    div().text_size(px(theme::scaled(11.0))).child(tip.clone())
                })
                .build(window, cx)
            })
            .on_click(cx.listener(|v, _e, _w, _cx| v.toggle_tab_wrap()));

        // 检索结果导航:仅高亮模式且存在命中时显示(过滤模式行已筛好,无需跳转)
        let match_nav: Option<AnyElement> = if !filter_mode && matched_n > 0 {
            let nav_btn = |id: &'static str, label: &'static str| {
                div()
                    .id(id)
                    .cursor(CursorStyle::PointingHand)
                    .flex_shrink_0()
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .text_size(px(theme::scaled(11.0)))
                    .text_color(theme::hex(pal.text2))
                    .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                    .child(label)
            };
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        nav_btn("btn-prev-match", "▲")
                            .on_click(cx.listener(|v, _e, _w, cx| v.jump_match(false, cx))),
                    )
                    .child(
                        nav_btn("btn-next-match", "▼")
                            .on_click(cx.listener(|v, _e, _w, cx| v.jump_match(true, cx))),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        // 加载/跳尾 banner(egui 版同):告知当前只加载了头部,或已跳到末尾
        let banner_btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .cursor(CursorStyle::PointingHand)
                .flex_shrink_0()
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .text_size(px(theme::scaled(11.0)))
                .text_color(theme::hex(pal.primary))
                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                .child(label)
        };
        let banner: Option<AnyElement> = if let Some((loaded, total)) = loading {
            Some(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(4.0))
                    .bg(theme::hex(pal.bg_bar))
                    .border_b_1()
                    .border_color(theme::hex(pal.split))
                    .text_size(px(theme::scaled(11.5)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::tf(
                        "status.head_loaded",
                        &[("loaded", fmt_mb(loaded)), ("total", fmt_mb(total))],
                    ))
                    .child(
                        banner_btn("ban-all", i18n::t("toolbar.load_all"))
                            .on_click(cx.listener(|v, _e, _w, _cx| v.load_all())),
                    )
                    .child(
                        banner_btn("ban-end", i18n::t("toolbar.jump_to_end"))
                            .on_click(cx.listener(|v, _e, _w, _cx| v.jump_to_end())),
                    )
                    .into_any_element(),
            )
        } else if let Some((off, total)) = tail_banner {
            Some(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(4.0))
                    .bg(theme::hex(pal.bg_bar))
                    .border_b_1()
                    .border_color(theme::hex(pal.split))
                    .text_size(px(theme::scaled(11.5)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::tf(
                        "status.jumped_end",
                        &[("off", off.to_string()), ("total", total.to_string())],
                    ))
                    .child(
                        banner_btn("ban-head", i18n::t("toolbar.load_from_start"))
                            .on_click(cx.listener(|v, _e, _w, _cx| v.load_all())),
                    )
                    .child(
                        banner_btn("ban-x", "✕")
                            .on_click(cx.listener(|v, _e, _w, cx| v.dismiss_tail_banner(cx))),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        let search_group = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                Input::new(&self.search_input)
                    .small()
                    .w(px(240.0))
                    .cleanable(true),
            )
            // 正则 / 大小写开关
            .child(
                div()
                    .id("btn-regex")
                    .cursor(CursorStyle::PointingHand)
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(if regex_on {
                        theme::hex(pal.primary)
                    } else {
                        theme::hex(pal.text2)
                    })
                    .bg(if regex_on {
                        theme::hexa(pal.primary, 0.12)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                    .child(".*")
                    .on_click(cx.listener(|v, _e, _w, _cx| v.toggle_regex())),
            )
            .child(
                div()
                    .id("btn-case")
                    .cursor(CursorStyle::PointingHand)
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(if case_on {
                        theme::hex(pal.primary)
                    } else {
                        theme::hex(pal.text2)
                    })
                    .bg(if case_on {
                        theme::hexa(pal.primary, 0.12)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                    .child("Aa")
                    .on_click(cx.listener(|v, _e, _w, _cx| v.toggle_case())),
            );

        let base = div()
            .w_full()
            .h(px(42.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .bg(theme::hex(pal.bg_bar))
            .border_b_1()
            .border_color(theme::hex(pal.split))
            .children(if is_md { None } else { Some(search_group) });

        // 代码标签页:仅保留「搜索 + 语言信息 + 编辑/只读 + 保存」
        let toolbar = if is_code {
            base.children(code_bar)
        } else {
            base
                // 模式:高亮 / 过滤
                .child(
                    div()
                        .flex()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(theme::hex(pal.border))
                        .overflow_hidden()
                        .child(
                            div()
                                .id("mode-highlight")
                                .cursor(CursorStyle::PointingHand)
                                .px(px(10.0))
                                .py(px(3.0))
                                .text_size(px(theme::scaled(12.0)))
                                .bg(if !filter_mode {
                                    theme::hex(pal.bg_selected)
                                } else {
                                    gpui_kit::transparent_black()
                                })
                                .text_color(if !filter_mode {
                                    theme::hex(pal.text1)
                                } else {
                                    theme::hex(pal.text2)
                                })
                                .child(i18n::t("search.highlight"))
                                .on_click(cx.listener(|v, _e, _w, _cx| v.set_filter_mode(false))),
                        )
                        .child(
                            div()
                                .id("mode-filter")
                                .cursor(CursorStyle::PointingHand)
                                .px(px(10.0))
                                .py(px(3.0))
                                .text_size(px(theme::scaled(12.0)))
                                .bg(if filter_mode {
                                    theme::hex(pal.bg_selected)
                                } else {
                                    gpui_kit::transparent_black()
                                })
                                .text_color(if filter_mode {
                                    theme::hex(pal.text1)
                                } else {
                                    theme::hex(pal.text2)
                                })
                                .child(i18n::t("search.filter"))
                                .on_click(cx.listener(|v, _e, _w, _cx| v.set_filter_mode(true))),
                        ),
                )
                .children(match_nav)
                .child(div().flex().gap(px(4.0)).overflow_hidden().children(chips))
                .child(cfg_btn)
                .child(wrap_btn)
                .children(code_bar)
                .child(div().flex_1())
                .child(
                    toolbar_icon_btn(
                        "btn-follow",
                        if follow {
                            "icons/tail-on.svg"
                        } else {
                            "icons/tail-off.svg"
                        },
                        SharedString::from(if follow {
                            i18n::t("status.following_hint")
                        } else {
                            i18n::t("status.paused_hint")
                        }),
                        follow,
                        pal,
                    )
                    .on_click(cx.listener(|v, _e, _w, _cx| v.toggle_follow())),
                )
                .child(
                    toolbar_icon_btn(
                        "btn-end",
                        "icons/jump-end.svg",
                        SharedString::from(i18n::t("toolbar.jump_to_end.hint")),
                        false,
                        pal,
                    )
                    .on_click(cx.listener(|v, _e, _w, _cx| v.jump_to_end())),
                )
                .child(
                    toolbar_icon_btn(
                        "btn-clear",
                        "icons/clear.svg",
                        SharedString::from(i18n::t("toolbar.clear.hint")),
                        false,
                        pal,
                    )
                    .on_click(cx.listener(|v, _e, _w, cx| v.clear_screen(cx))),
                )
                .child(
                    toolbar_icon_btn(
                        "btn-all",
                        "icons/load-all.svg",
                        SharedString::from(i18n::t("toolbar.load_all.confirm")),
                        false,
                        pal,
                    )
                    .on_click(cx.listener(|v, _e, _w, _cx| v.load_all())),
                )
        };

        // —— 状态栏 segments ——
        // 位置信息排在状态栏最后(本地 = 路径,SSH = 地址:路径,HTTP = URL;设置/欢迎页没有来源)
        let location: Option<String> = cur.and_then(|t| {
            t.params
                .as_ref()
                .map(|p| p.location())
                .or_else(|| t.code.as_ref().map(|d| d.path.display().to_string()))
        });
        let mut segs: Vec<(String, Hsla)> = Vec::new();
        if let Some(t) = cur {
            segs.push((
                i18n::tn(t.view.len() as u64, "status.rows"),
                theme::hex(pal.text2),
            ));
            if t.store.dropped > 0 {
                segs.push((
                    i18n::tf("status.dropped", &[("n", t.store.dropped.to_string())]),
                    theme::hex(pal.text3),
                ));
            }
            segs.push((
                if t.stream {
                    // 命令源的 size 就是输出缓存(spool)当前长度;跳尾 Replace 后
                    // bytes_loaded 只是尾部起点,不能代表“已接收多少”,所以用 total。
                    i18n::tf("status.received", &[("size", fmt_size(t.store.total_size))])
                } else {
                    format!(
                        "{}/{}",
                        fmt_size(t.store.bytes_loaded),
                        fmt_size(t.store.total_size)
                    )
                },
                theme::hex(pal.text3),
            ));
            if let Some((note, bad)) = &t.exit_note {
                segs.push((
                    note.clone(),
                    theme::hex(if *bad { pal.warning } else { pal.text3 }),
                ));
            }
            if let Some((loaded, total)) = loading {
                let pct = if total > 0 {
                    loaded as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                segs.push((
                    i18n::tf("status.loading", &[("pct", format!("{pct:.0}"))]),
                    theme::hex(pal.primary),
                ));
            }
            segs.push((
                i18n::tf(
                    "status.polling",
                    &[("ms", self.settings.poll_ms.to_string())],
                ),
                theme::hex(pal.text3),
            ));
            if cur.is_some_and(|t| t.relevel.is_some()) {
                segs.push((
                    i18n::t("status.reanalyzing").into(),
                    theme::hex(pal.warning),
                ));
            }
            if no_follow {
                segs.push((i18n::t("status.no_follow").into(), theme::hex(pal.warning)));
            }
            if paused {
                segs.push((i18n::t("status.paused").into(), theme::hex(pal.warning)));
            } else if follow {
                segs.push((i18n::t("status.following").into(), theme::hex(pal.primary)));
            } else if pending_new > 0 {
                segs.push((
                    i18n::tn(pending_new as u64, "status.new_lines_plus"),
                    theme::hex(pal.primary),
                ));
            }
            if !filter_mode && t.query_active() && match_err.is_none() {
                segs.push((
                    i18n::tn(matched_n as u64, "status.matched"),
                    theme::hex(pal.text2),
                ));
            }
            if let Some(e) = match_err {
                segs.push((i18n::tf("error.regex", &[("e", e)]), theme::hex(pal.error)));
            }
            if let Some(e) = err_text {
                segs.push((e, theme::hex(pal.error)));
            }
        } else {
            segs.push((i18n::t("status.ready").into(), theme::hex(pal.text3)));
        }
        if let Some(n) = &self.cred_notice {
            segs.push((i18n::td(n), theme::hex(pal.warning)));
        }

        // —— 位置信息:贴状态栏最右,占据剩余宽度;悬停显示完整路径,双击复制 ——
        let loc_size = px(theme::scaled(11.0));
        let ts = window.text_system().clone();
        let loc_font = gpui_kit::font(ui_font.clone());
        let mut measure = move |t: &str| -> Pixels {
            if t.is_empty() {
                return px(0.0);
            }
            let run = TextRun {
                len: t.len(),
                font: loc_font.clone(),
                color: gpui_kit::transparent_black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            ts.layout_line(t, loc_size, &[run], None).width
        };
        // 剩余宽度 = 视口 - 左右内边距 - 段间距 - 其他段宽 - 安全余量
        let seg_w = segs.iter().fold(px(0.0), |acc, (t, _)| acc + measure(t));
        let gaps = if location.is_some() {
            segs.len()
        } else {
            segs.len().saturating_sub(1)
        };
        let avail =
            (window.viewport_size().width - px(20.0) - px(12.0) * gaps as f32 - seg_w - px(8.0))
                .max(px(48.0));
        let loc_el: Option<AnyElement> = location.map(|loc| {
            let shown = elide_path(&loc, avail, &mut measure);
            let copy = loc.clone();
            let tip = loc.clone();
            // 外层占满剩余宽度并把文字推到最右;交互只挂在文字上,避免误触
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .justify_end()
                .child(
                    div()
                        .id("status-location")
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .cursor(CursorStyle::PointingHand)
                        .text_size(loc_size)
                        .text_color(theme::hex(pal.text2))
                        .child(shown)
                        .tooltip(move |window, cx| {
                            let full = tip.clone();
                            gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                                div()
                                    .max_w(px(520.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(full.clone())
                                    .child(
                                        div()
                                            .text_size(px(theme::scaled(10.5)))
                                            .text_color(theme::hex(pal.text3))
                                            .child(i18n::t("toolbar.copy_path.hint")),
                                    )
                            })
                            .build(window, cx)
                        })
                        .on_click(cx.listener(move |v, e: &ClickEvent, _w, cx| {
                            if e.click_count() >= 2 {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                v.show_toast(i18n::t("toast.path_copied"), e.position(), cx);
                            }
                        })),
                )
                .into_any_element()
        });
        let status_bar = div()
            .w_full()
            .h(px(24.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(10.0))
            .bg(theme::hex(pal.bg_bar))
            .border_t_1()
            .border_color(theme::hex(pal.split))
            .overflow_hidden()
            .children(segs.into_iter().map(|(txt, color)| {
                div()
                    .text_size(loc_size)
                    .text_color(color)
                    .whitespace_nowrap()
                    .child(txt)
            }))
            .children(loc_el);

        // —— 日志区(虚拟列表;跟随/解除跟随由 poll 中的贴底判定统一处理) ——
        let entity = cx.entity();
        let sizes = self.row_sizes(window, &mono_font);
        let log_list = v_virtual_list(entity, "log-list", sizes, |this, range, _window, cx| {
            this.render_rows(range, cx)
        })
        .track_scroll(&self.list);
        let log_area = div()
            .flex_1()
            .min_h(px(0.0))
            .relative()
            .overflow_hidden()
            .child(log_list)
            .child(Scrollbar::vertical(&self.list));
        // 未跟随时的"新行"提示浮标
        let log_area = if has_tabs && !follow && pending_new > 0 {
            log_area.child(
                div()
                    .absolute()
                    .bottom(px(10.0))
                    .right(px(14.0))
                    .id("pending-new")
                    .cursor(CursorStyle::PointingHand)
                    .px(px(10.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .bg(theme::hex(pal.primary))
                    .text_size(px(theme::scaled(11.5)))
                    .text_color(theme::hex(pal.on_primary))
                    .child(i18n::tn(pending_new as u64, "banner.new_lines"))
                    .on_click(cx.listener(|v, _e, _w, _cx| v.jump_to_end())),
            )
        } else {
            log_area
        };
        // 编辑态用多行编辑器替掉只读列表
        let content_area: AnyElement = match md_preview {
            Some(e) => e,
            None => match code_editor {
                Some(e) => e,
                None => log_area.into_any_element(),
            },
        };

        // —— 主体 ——
        let body = if has_tabs {
            if is_settings {
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    .child(self.render_settings_panel(cx))
            } else if is_welcome {
                // 空白标签页:整块内容为「打开日志源」欢迎页
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    .child(self.render_welcome(ti, cx))
            } else {
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    .child(toolbar)
                    .children(banner)
                    .child(content_area)
                    .child(status_bar)
            }
        } else {
            // 兜底:正常流程下关闭最后页会自动补空白页,这里不应出现可交互内容
            div().flex_1().min_h(px(0.0))
        };

        // —— 鼠标上方的短提示(已复制等):按文案实测宽度居中在鼠标上方 ——
        let toast_el: Option<AnyElement> = self.toast.as_ref().map(|t| {
            let size = px(theme::scaled(11.5));
            let text_w = {
                let run = TextRun {
                    len: t.text.len(),
                    font: gpui_kit::font(ui_font.clone()),
                    color: gpui_kit::transparent_black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                window
                    .text_system()
                    .layout_line(&t.text, size, &[run], None)
                    .width
            };
            let w = text_w + px(20.0);
            let h = px(24.0);
            let vp = window.viewport_size();
            // 居中对齐鼠标,超出窗口时收回边界
            let left = (t.at.x - w * 0.5).clamp(px(4.0), (vp.width - w - px(4.0)).max(px(4.0)));
            let top =
                (t.at.y - h - px(12.0)).clamp(px(4.0), (vp.height - h - px(4.0)).max(px(4.0)));
            // 成功色实底:暗色主题的 success 偏亮,配深色字;亮色主题反之配白字
            let (toast_bg, toast_fg) = if light {
                (theme::hex(pal.success), theme::hex(pal.on_primary))
            } else {
                (theme::hex(pal.success), theme::hex(pal.bg_layout))
            };
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(w)
                .h(h)
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .bg(toast_bg)
                .shadow_lg()
                .text_size(size)
                .text_color(toast_fg)
                .child(t.text.clone())
                .into_any_element()
        });

        div()
            .id("root")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::hex(pal.bg_layout))
            .text_color(theme::hex(pal.text1))
            .text_size(px(theme::scaled(13.0)))
            .font_family(ui_font)
            // 根节点持有焦点:接收 ⌘T/⌘W/⌘K 等快捷键
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, w, cx| v.handle_key(e, w, cx)))
            // 系统文件拖入:gpui 把外部拖拽包装为 ExternalPaths 的 drop 事件
            .on_drop(
                cx.listener(|v, paths: &ExternalPaths, _window, _cx| v.handle_file_drop(paths)),
            )
            .child(
                TitleBar::new().bg(theme::hex(pal.bg_bar)).child(
                    div()
                        .w_full()
                        .h_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .children(bar),
                ),
            )
            .child(body)
            .children(self.render_dialog(window, cx))
            .children(self.render_config_menu(cx))
            // 复制等操作的短提示:贴着鼠标位置上方出现
            .children(toast_el)
            // 选区层放在内容之后,才能在行底色之上画出选中高亮
            .child(gpui_kit::base::TextSelectionLayer)
    }
}

impl Tab {
    /// 选中行范围(闭区间,真实行号);无选中时为 None
    fn sel_range(&self) -> Option<(usize, usize)> {
        let (a, b) = (self.sel_anchor?, self.sel_focus?);
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// 检索是否处于激活状态(有查询词且编译成功)
    fn query_active(&self) -> bool {
        !self.query.is_empty() && self.matcher.is_some()
    }
}

/// 退出前把排队中的设置同步落盘，避免防抖窗口内的最后一次改动丢失
impl Drop for AppView {
    fn drop(&mut self) {
        if self.save_scheduled {
            store::save(&self.settings);
        }
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.view(window, cx)
    }
}

/// 修饰键键名(单独按下时不应生成组合)
const MODIFIER_KEYS: [&str; 9] = [
    "shift", "control", "ctrl", "alt", "option", "platform", "cmd", "meta", "fn",
];

/// 从按键事件生成规格字符串(如 "secondary+shift+k");无修饰键或只按修饰键返回 None
fn keystroke_spec(ks: &Keystroke, secondary: bool) -> Option<String> {
    let key = ks.key.to_ascii_lowercase();
    if key.is_empty() || MODIFIER_KEYS.contains(&key.as_str()) {
        return None;
    }
    let m = &ks.modifiers;
    let mut parts: Vec<&str> = Vec::new();
    if secondary {
        parts.push("secondary");
    }
    if m.control && !secondary {
        parts.push("ctrl");
    }
    if m.alt {
        parts.push("alt");
    }
    if m.shift {
        parts.push("shift");
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{}+{}", parts.join("+"), key))
}

/// 规格 → 展示文本(macOS 用符号,其他平台用 Ctrl/Alt/Shift)
fn shortcut_label(spec: &str) -> String {
    let mac = cfg!(target_os = "macos");
    spec.split('+')
        .filter(|t| !t.is_empty())
        .map(|t| match t {
            "secondary" => if mac { "⌘" } else { "Ctrl" }.to_string(),
            "ctrl" => if mac { "⌃" } else { "Ctrl" }.to_string(),
            "alt" => if mac { "⌥" } else { "Alt" }.to_string(),
            "shift" => if mac { "⇧" } else { "Shift" }.to_string(),
            k => k.to_ascii_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 规格 → 键帽序列(设置页展示)
fn shortcut_caps(spec: &str, pal: &'static Palette) -> AnyElement {
    let mac = cfg!(target_os = "macos");
    let mut row = div().flex().items_center().gap(px(4.0));
    for tok in spec.split('+').filter(|t| !t.is_empty()) {
        let label = match tok {
            "secondary" => if mac { "⌘" } else { "Ctrl" }.to_string(),
            "ctrl" => if mac { "⌃" } else { "Ctrl" }.to_string(),
            "alt" => if mac { "⌥" } else { "Alt" }.to_string(),
            "shift" => if mac { "⇧" } else { "Shift" }.to_string(),
            k => k.to_ascii_uppercase(),
        };
        row = row.child(keycap(&label, pal));
    }
    row.into_any_element()
}

/// 键帽(快捷键展示用,不可点击)
fn keycap(text: &str, pal: &'static Palette) -> AnyElement {
    div()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme::hex(pal.border))
        .bg(theme::hex(pal.bg_input))
        .text_size(px(theme::scaled(10.5)))
        .text_color(theme::hex(pal.text2))
        .child(text.to_string())
        .into_any_element()
}

/// 字节数转人类可读文本(日志加载 banner 用)
fn fmt_mb(b: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if b as f64 >= MB {
        format!("{:.1} MB", b as f64 / MB)
    } else {
        format!("{} KB", (b as f64 / 1024.0).ceil() as u64)
    }
}

// ---- 设置页小部件(VS Code 风格) ----

/// 设置页一行:左侧标题/说明,右侧控件
/// 查询 GitHub 最新发布(blocking,调用方必须放到独立线程)。
///
/// 404(尚无发布 / 仓库未公开)与缺 `tag_name` 都返回 `Ok(None)`,按「没有更新」处理;
/// 其余非 2xx 与网络错误返回可读错误文本。只发版本号作 User-Agent,不上报任何本地信息。
fn fetch_latest_release() -> Result<Option<Release>, String> {
    // reqwest 默认走系统/环境代理(HTTPS_PROXY 等),国内网络由用户自行配置
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("logview/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let body = resp.text().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let tag = v["tag_name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if tag.is_empty() {
        return Ok(None);
    }
    let url = v["html_url"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{REPO_URL}/releases"));
    Ok(Some(Release { tag, url }))
}

/// 拆语义化版本:`v1.2.3-rc.1` → ([1, 2, 3], Some("rc.1"))
fn split_version(s: &str) -> (Vec<u64>, Option<String>) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (s, None),
    };
    let nums = core
        .split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0))
        .collect();
    (nums, pre)
}

/// `a` 是否比 `b` 新:缺失段补 0;核心号相同时预发布低于正式版,预发布之间按字符串比。
fn version_newer(a: &str, b: &str) -> bool {
    let (mut an, ap) = split_version(a);
    let (mut bn, bp) = split_version(b);
    an.resize(3, 0);
    bn.resize(3, 0);
    match an.cmp(&bn) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match (ap, bp) {
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (Some(x), Some(y)) => x > y,
            (None, None) => false,
        },
    }
}

fn settings_row(
    pal: &'static Palette,
    title: impl Into<SharedString>,
    desc: Option<SharedString>,
    control: AnyElement,
) -> AnyElement {
    let title: SharedString = title.into();
    let mut left = div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .flex_1()
        .min_w(px(0.0))
        .child(
            div()
                .text_size(px(theme::scaled(13.0)))
                .text_color(theme::hex(pal.text1))
                .child(title),
        );
    if let Some(desc) = desc {
        left = left.child(
            div()
                .text_size(px(theme::scaled(11.5)))
                .text_color(theme::hex(pal.text2))
                .child(desc),
        );
    }
    div()
        .flex()
        .items_center()
        .gap(px(24.0))
        .px(px(16.0))
        .py(px(11.0))
        .border_b_1()
        .border_color(theme::hex(pal.split))
        .child(left)
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

/// 设置页分组卡片:标题栏(可带右上角动作)+ 若干设置行
fn settings_group(
    title: impl Into<SharedString>,
    action: Option<AnyElement>,
    rows: Vec<AnyElement>,
    pal: &'static Palette,
) -> AnyElement {
    let title: SharedString = title.into();
    let mut head = div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(theme::hex(pal.split))
        .bg(theme::hex(pal.fill))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(theme::scaled(11.5)))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(theme::hex(pal.text2))
                .child(title),
        );
    if let Some(action) = action {
        head = head.child(div().flex_shrink_0().child(action));
    }
    div()
        .flex()
        .flex_col()
        // 滚动容器是 flex 列,卡片必须禁止收缩,否则内容会被裁切
        .flex_shrink_0()
        .rounded(px(8.0))
        .border_1()
        .border_color(theme::hex(pal.border))
        .bg(theme::hex(pal.bg_bar))
        .overflow_hidden()
        .child(head)
        .children(rows)
        .into_any_element()
}

/// 设置项搜索匹配(标题/说明/关键词任一包含即命中,忽略大小写)
fn setting_hit(q: &str, parts: &[&str]) -> bool {
    q.is_empty() || parts.iter().any(|p| p.to_lowercase().contains(q))
}

/// 是否为认证类错误(SSH 认证失败 / HTTP 401·403):需要让用户重填凭据
fn is_auth_error(msg: &str) -> bool {
    msg.contains("认证失败") || msg.contains("HTTP 401") || msg.contains("HTTP 403")
}

/// URL 认证方式的中文标签(设置页凭据列表用)
fn http_auth_label(k: crate::source::HttpAuthKind) -> &'static str {
    use crate::source::HttpAuthKind as K;
    match k {
        K::None => i18n::t("http.auth.none"),
        K::Basic => "Basic",
        K::Bearer => "Bearer",
        K::ApiKey => "API Key",
        K::Header => i18n::t("http.header.custom"),
    }
}

/// SSH 表单校验:主机/用户名、远程路径、端口依次校验,返回第一条错误
fn validate_ssh(host: &str, user: &str, path: &str, port_txt: &str) -> Option<String> {
    if host.trim().is_empty() || user.trim().is_empty() {
        return Some(i18n::t("validation.host_user_required").to_string());
    }
    if path.trim().is_empty() {
        return Some(i18n::t("validation.remote_path_required").to_string());
    }
    validate_ssh_conn(host, user, port_txt)
}

/// SSH 命令模式:不需要远程路径
fn validate_ssh_conn(host: &str, user: &str, port_txt: &str) -> Option<String> {
    if host.trim().is_empty() || user.trim().is_empty() {
        return Some(i18n::t("validation.host_user_required").to_string());
    }
    match port_txt.trim().parse::<u16>() {
        Ok(p) if p > 0 => None,
        Ok(_) => Some(i18n::t("validation.port_range").to_string()),
        Err(e) => Some(i18n::tf("error.port_invalid", &[("e", e.to_string())])),
    }
}

/// 由参数生成命令执行计划(预设按当前字段值重新展开,避免沿用旧 PID)
fn command_plan(settings: &Settings, params: &SourceParams) -> Option<CommandPlan> {
    let SourceParams::Command {
        argv,
        preset,
        values,
        raw,
        ..
    } = params
    else {
        return None;
    };
    match preset
        .as_ref()
        .and_then(|id| command::find(&settings.command_presets, id))
    {
        Some(p) => command::plan(p, values).ok(),
        None => Some(CommandPlan {
            argv: argv.clone(),
            resolve: Vec::new(),
            raw: *raw,
        }),
    }
}

/// 参数对应的钥匙串 key:SSH 命令复用主机凭据(与文件源同一个条目)
fn key_of_params(p: &SourceParams) -> Option<String> {
    secrets::key_of(p).or_else(|| match p {
        SourceParams::Command {
            target: ExecTarget::Ssh {
                host, port, user, ..
            },
            ..
        } => Some(secrets::ssh_key(user, host, *port)),
        _ => None,
    })
}

/// 命令表单 → 源参数
fn command_params(
    fields: &CmdFields,
    presets: &[CommandPreset],
    local: bool,
    target: ExecTarget,
    cx: &Context<AppView>,
) -> Result<SourceParams, String> {
    let restart = fields.restart;
    if fields.preset == 0 {
        let text = fields.text.read(cx).value().trim().to_string();
        if text.is_empty() {
            return Err(i18n::t("validation.command_required").into());
        }
        let argv = if local {
            command::split_argv(&text)?
        } else {
            vec![text]
        };
        if argv.is_empty() {
            return Err(i18n::t("validation.command_required").into());
        }
        return Ok(SourceParams::Command {
            target,
            argv,
            preset: None,
            values: BTreeMap::new(),
            restart,
            raw: !local,
        });
    }
    let list = command::presets_for(presets, local);
    let p = list
        .get(fields.preset - 1)
        .ok_or(i18n::t("error.command.preset_missing"))?;
    let mut values = BTreeMap::new();
    for (f, e) in &fields.inputs {
        values.insert(f.key.clone(), e.read(cx).value().to_string());
    }
    let plan = command::plan(p, &values)?;
    Ok(SourceParams::Command {
        target,
        argv: plan.argv,
        preset: Some(p.id.clone()),
        values,
        restart,
        raw: false,
    })
}

/// 命令表单行(预设 chips + 字段/自定义命令 + 自动重试)
#[allow(clippy::too_many_arguments)]
fn command_rows(
    cx: &Context<AppView>,
    presets: &[CommandPreset],
    local: bool,
    fields: &CmdFields,
    pal: &'static Palette,
    hint: &'static str,
    on_preset: impl Fn(&mut AppView, usize, &mut Window, &mut Context<AppView>) + Clone + 'static,
    on_restart: impl Fn(&mut AppView) + 'static,
) -> Vec<AnyElement> {
    let list = command::presets_for(presets, local);
    let mut names: Vec<String> = vec![i18n::t("configs.custom_tag").into()];
    names.extend(list.iter().map(|p| i18n::td(&p.name)));
    let sel = fields.preset.min(names.len().saturating_sub(1));

    let mut chips = div().flex().flex_wrap().gap(px(4.0));
    for (i, name) in names.iter().enumerate() {
        let f = on_preset.clone();
        chips = chips.child(
            div()
                .id(SharedString::from(format!("cmd-preset-{local}-{i}")))
                .cursor(CursorStyle::PointingHand)
                .px(px(10.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .text_size(px(theme::scaled(12.0)))
                .bg(if i == sel {
                    theme::hex(pal.bg_selected)
                } else {
                    gpui_kit::transparent_black()
                })
                .text_color(if i == sel {
                    theme::hex(pal.text1)
                } else {
                    theme::hex(pal.text2)
                })
                .child(SharedString::from(name.clone()))
                .on_click(cx.listener(move |v, _e, w, cx| f(v, i, w, cx))),
        );
    }

    fn label(text: &str, pal: &'static Palette) -> AnyElement {
        div()
            .w(px(88.0))
            .flex_shrink_0()
            .text_right()
            .text_size(px(theme::scaled(12.0)))
            .text_color(theme::hex(pal.text2))
            .child(text.to_string())
            .into_any_element()
    }

    let mut rows: Vec<AnyElement> = vec![div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(div().w(px(88.0)).flex_shrink_0())
        .child(chips)
        .into_any_element()];

    if sel == 0 {
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(i18n::t("source.command"), pal))
                .child(div().flex_1().child(Input::new(&fields.text).small()))
                .into_any_element(),
        );
    } else if let Some(p) = list.get(sel - 1) {
        for (f, e) in &fields.inputs {
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(label(&i18n::td(&f.label), pal))
                    .child(div().flex_1().child(Input::new(e).small()))
                    .into_any_element(),
            );
        }
        let mut values = BTreeMap::new();
        for (f, e) in &fields.inputs {
            values.insert(f.key.clone(), e.read(cx).value().to_string());
        }
        if let Ok(plan) = command::plan(p, &values) {
            rows.push(
                div()
                    .pl(px(96.0))
                    .text_size(px(theme::scaled(11.0)))
                    .text_color(theme::hex(pal.text3))
                    .child(i18n::tf(
                        "command.will_run",
                        &[("cmd", command::display(&plan.argv))],
                    ))
                    .into_any_element(),
            );
        }
    }

    rows.push(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().w(px(88.0)).flex_shrink_0())
            .child(
                switch_el(
                    if local {
                        "cmd-restart"
                    } else {
                        "ssh-cmd-restart"
                    },
                    fields.restart,
                    pal,
                )
                .on_click(cx.listener(move |v, _e, _w, _cx| on_restart(v))),
            )
            .child(
                div()
                    .text_size(px(theme::scaled(12.0)))
                    .text_color(theme::hex(pal.text2))
                    .child(i18n::t("command.restart_hint")),
            )
            .into_any_element(),
    );
    rows.push(
        div()
            .pl(px(96.0))
            .text_size(px(theme::scaled(11.0)))
            .text_color(theme::hex(pal.text3))
            .child(hint)
            .into_any_element(),
    );
    rows
}

/// 日志地址校验:不能为空,且必须是 http/https
fn validate_url_addr(url: &str) -> Option<String> {
    let u = url.trim();
    if u.is_empty() {
        return Some(i18n::t("validation.url_required").to_string());
    }
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Some(i18n::t("validation.url_scheme").to_string());
    }
    None
}

/// 设置页数字输入(右侧单位)
///
/// 文本右对齐:数值个位/小数点靠近单位一侧,纵向扫视更容易比较大小。
fn num_input(
    e: &Entity<InputState>,
    w: f32,
    unit: &'static str,
    pal: &'static Palette,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .w(px(w))
                .child(Input::new(e).small().text_align(TextAlign::Right)),
        )
        .child(
            div()
                .text_size(px(theme::scaled(11.0)))
                .text_color(theme::hex(pal.text3))
                .child(unit),
        )
        .into_any_element()
}

/// 颜色单元:色块(点击打开取色器)+ hex 输入框
fn color_cell(pick: &Entity<ColorPickerState>, input: &Entity<InputState>, w: f32) -> AnyElement {
    div()
        .w(px(w))
        .flex()
        .items_center()
        .gap(px(4.0))
        .child(ColorPicker::new(pick).small())
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .child(Input::new(input).small().w_full()),
        )
        .into_any_element()
}

/// 设置页文本输入
fn text_input(e: &Entity<InputState>, w: f32) -> AnyElement {
    div()
        .w(px(w))
        .child(Input::new(e).small())
        .into_any_element()
}

// ---- 弹层通用小部件 ----

/// antd 风格分段选择(高亮/过滤、源类型等)
fn segmented<F>(
    cx: &Context<AppView>,
    id: &'static str,
    options: &[&str],
    sel: usize,
    pal: &'static Palette,
    on_change: F,
) -> AnyElement
where
    F: Fn(&mut AppView, usize) + Clone + 'static,
{
    let mut row = div()
        .id(SharedString::from(format!("{id}-seg")))
        .flex()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::hex(pal.border))
        .overflow_hidden();
    for (i, label) in options.iter().enumerate() {
        let f = on_change.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("{id}-seg-{i}")))
                .cursor(CursorStyle::PointingHand)
                .px(px(12.0))
                .py(px(3.0))
                .text_size(px(theme::scaled(12.0)))
                .bg(if i == sel {
                    theme::hex(pal.bg_selected)
                } else {
                    gpui_kit::transparent_black()
                })
                .text_color(if i == sel {
                    theme::hex(pal.text1)
                } else {
                    theme::hex(pal.text2)
                })
                .child(SharedString::from((*label).to_string()))
                .on_click(cx.listener(move |v, _e, _w, _cx| f(v, i))),
        );
    }
    row.into_any_element()
}

/// antd 开关(视觉 + 点击在调用处接)
fn switch_el(id: impl Into<ElementId>, on: bool, pal: &'static Palette) -> Stateful<Div> {
    div()
        .id(id)
        .cursor(CursorStyle::PointingHand)
        .relative()
        .w(px(34.0))
        .h(px(18.0))
        .rounded_full()
        .bg(if on {
            theme::hex(pal.primary)
        } else {
            theme::hex(pal.fill_hover)
        })
        .child(
            div()
                .absolute()
                .top(px(2.0))
                .left(if on { px(17.0) } else { px(2.0) })
                .size(px(14.0))
                .rounded_full()
                .bg(theme::hex(if on { pal.on_primary } else { pal.text3 })),
        )
}

/// 对话框按钮(primary=主题色实底)
fn dlg_btn(
    id: &'static str,
    label: &'static str,
    primary: bool,
    pal: &'static Palette,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .cursor(CursorStyle::PointingHand)
        .px(px(14.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .text_size(px(theme::scaled(12.5)))
        .child(label);
    if primary {
        base.bg(theme::hex(pal.primary))
            .text_color(theme::hex(pal.on_primary))
            .hover(|s| s.opacity(0.88))
    } else {
        base.border_1()
            .border_color(theme::hex(pal.border))
            .text_color(theme::hex(pal.text1))
            .hover(|s| s.bg(theme::hex(pal.fill_hover)))
    }
}

/// 设置页动态按钮(文案固定、id 可变)
fn host_btn(
    id: SharedString,
    label: &'static str,
    primary: bool,
    pal: &'static Palette,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .cursor(CursorStyle::PointingHand)
        .px(px(12.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .text_size(px(theme::scaled(12.0)))
        .child(label);
    if primary {
        base.bg(theme::hex(pal.primary))
            .text_color(theme::hex(pal.on_primary))
            .hover(|s| s.opacity(0.88))
    } else {
        base.border_1()
            .border_color(theme::hex(pal.border))
            .text_color(theme::hex(pal.text1))
            .hover(|s| s.bg(theme::hex(pal.fill_hover)))
    }
}

/// 对话框标题行(右侧 × 关闭)
fn dlg_header(
    title: &'static str,
    pal: &'static Palette,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(16.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(theme::hex(pal.split))
        .child(
            div()
                .text_size(px(theme::scaled(14.0)))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(theme::hex(pal.text1))
                .child(title),
        )
        .child(
            div()
                .id("dlg-close")
                .cursor(CursorStyle::PointingHand)
                .px(px(6.0))
                .rounded(px(4.0))
                .text_size(px(theme::scaled(14.0)))
                .text_color(theme::hex(pal.text3))
                .hover(|s| s.bg(theme::hex(pal.fill_hover)))
                .child(
                    svg()
                        .path("icons/close.svg")
                        .w(px(14.0))
                        .h(px(14.0))
                        .text_color(theme::hex(pal.text3)),
                )
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::element(|_, _| {
                        div()
                            .text_size(px(theme::scaled(11.0)))
                            .child(i18n::t("common.close"))
                    })
                    .build(window, cx)
                })
                .on_click(on_close),
        )
        .into_any_element()
}

/// 遮罩 + 居中面板
fn dlg_shell(
    id: &'static str,
    header: impl IntoElement,
    body: impl IntoElement,
    footer: impl IntoElement,
    pal: &'static Palette,
    font: SharedString,
) -> AnyElement {
    div()
        .id("dlg-overlay")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::hexa("#000000", 0.45))
        // 阻断遮罩后的命中测试,避免关闭弹窗的那次点击穿透到下方的最近打开列表
        .occlude()
        .font_family(font)
        .child(
            div()
                .id(id)
                .w(px(640.0))
                .flex()
                .flex_col()
                .bg(theme::hex(pal.bg_elevated))
                .border_1()
                .border_color(theme::hex(pal.border))
                .rounded(px(10.0))
                .shadow_lg()
                .overflow_hidden()
                .child(header)
                .child(body)
                .child(footer),
        )
        .into_any_element()
}

/// 状态栏位置文案的中间省略:保留开头目录与文件名,中间折叠为 …
///
/// 预算按像素计(`measure` 返回文本宽度):先尝试「头 h 段/…/尾 t 段」的各种组合,
/// 取放得下且信息量最大(同样多时头尾更均衡)的一个;若连最小分段形式都放不下,
/// 退化为字符级中间省略,仍优先保留末尾文件名。
fn elide_path(s: &str, avail: Pixels, measure: &mut dyn FnMut(&str) -> Pixels) -> String {
    if s.is_empty() {
        return String::new();
    }
    if avail <= px(0.0) {
        return "…".to_string();
    }
    if measure(s) <= avail {
        return s.to_string();
    }
    let abs = s.starts_with('/');
    let segs: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if segs.len() >= 2 {
        // (保留字符数, 头尾段数差, 文案)
        let mut cands: Vec<(usize, usize, String)> = Vec::new();
        for h in 1..segs.len() {
            for t in 1..=(segs.len() - h) {
                let kept: usize = segs[..h].iter().map(|p| p.chars().count()).sum::<usize>()
                    + segs[segs.len() - t..]
                        .iter()
                        .map(|p| p.chars().count())
                        .sum::<usize>();
                let head = segs[..h].join("/");
                let tail = segs[segs.len() - t..].join("/");
                let prefix = if abs { "/" } else { "" };
                cands.push((kept, h.abs_diff(t), format!("{prefix}{head}/…/{tail}")));
            }
        }
        cands.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for (_, _, text) in cands {
            if measure(&text) <= avail {
                return text;
            }
        }
    }
    // 字符级兜底:头部约 1/3、末尾(文件名)约 2/3,二分求放得下的最长形式
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut best = "…".to_string();
    let (mut lo, mut hi) = (1usize, n);
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let head = mid / 3;
        let tail = mid.saturating_sub(head + 1);
        let h: String = chars[..head].iter().collect();
        let t: String = chars[n - tail..].iter().collect();
        let text = format!("{h}…{t}");
        if measure(&text) <= avail {
            best = text;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    best
}

/// 视图离底部还有多远(屏幕像素)
///
/// gpui 的滚动 offset 始终 ≤ 0(向下累加为负),但 `max_offset()` 在不同版本里
/// 可能是正值也可能是负值;先把 max 归一到「底部对应的 offset」再取差值,两种约定都成立
fn bottom_distance(off: Pixels, max: Pixels) -> Pixels {
    let bottom = if max <= px(0.0) { max } else { -max };
    (off - bottom).abs()
}

/// 改写一个输入框的占位符(language 切换后调用)
fn set_input_placeholder(
    input: &Entity<InputState>,
    placeholder: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    input.update(cx, {
        let placeholder: SharedString = placeholder.into();
        move |state, cx| state.set_placeholder(placeholder, window, cx)
    });
}

/// 命令表单的占位符来自预设数据(中文源文案),切语言后按 `i18n::td` 重新翻译
fn refresh_cmd_placeholders(fields: &CmdFields, window: &mut Window, cx: &mut Context<AppView>) {
    set_input_placeholder(&fields.text, i18n::t("command.example"), window, cx);
    for (field, input) in &fields.inputs {
        if !field.placeholder.is_empty() {
            set_input_placeholder(input, i18n::td(&field.placeholder), window, cx);
        }
    }
}

/// 级别显示文案:级别缩写直接用原文,「无级别」等标签走语言包
fn level_labels() -> Vec<String> {
    level_names()
        .iter()
        .map(|id| i18n::t(id).to_string())
        .collect()
}

/// 按可用宽度挑选级别芯片的显示密度:先试完整名称,再退到首字母+计数,最后只留首字母
fn pick_chip_style(avail: f32, others: f32, full_w: f32, short_w: f32) -> ChipStyle {
    if avail >= others + full_w {
        ChipStyle::Full
    } else if avail >= others + short_w {
        ChipStyle::Short
    } else {
        ChipStyle::Letter
    }
}

/// 级别芯片的显示密度:窗口变窄时逐档退让,避免工具栏溢出
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChipStyle {
    /// `INFO · 153`
    Full,
    /// `I · 153`
    Short,
    /// `I`
    Letter,
}

/// 面板图标按钮(28×28,悬停底色,tooltip 说明)
///
/// 设置页等无文字标签的位置用图标按钮;图标本身不表意时,悬停提示就是唯一的说明。
fn panel_icon_btn(
    id: impl Into<ElementId>,
    icon: &'static str,
    tip: SharedString,
    pal: &'static Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .cursor(CursorStyle::PointingHand)
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.0))
        .h(px(28.0))
        .rounded(px(6.0))
        .bg(gpui_kit::transparent_black())
        .hover(|s| s.bg(theme::hex(pal.fill_hover)))
        .child(
            svg()
                .path(icon)
                .w(px(16.0))
                .h(px(16.0))
                .text_color(theme::hex(pal.text2)),
        )
        .tooltip(move |window, cx| {
            let tip = tip.clone();
            gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                div().text_size(px(theme::scaled(11.0))).child(tip.clone())
            })
            .build(window, cx)
        })
}

/// 工具栏图标按钮(26×26,悬停底色,tooltip 说明)
///
/// 工具栏按钮统一用图标而不是文字:窄窗口下文字按钮会把工具栏挤爆并裁掉右侧动作,
/// 图标恒定 26px,含义由 tooltip 说明。`active` 为真时用主题色描线 + 淡色底表示「已开启」。
fn toolbar_icon_btn(
    id: &'static str,
    icon: &'static str,
    tip: SharedString,
    active: bool,
    pal: &'static Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .cursor(CursorStyle::PointingHand)
        .flex()
        .items_center()
        .justify_center()
        .w(px(26.0))
        .h(px(26.0))
        .rounded(px(4.0))
        .bg(if active {
            theme::hexa(pal.primary, 0.12)
        } else {
            gpui_kit::transparent_black()
        })
        .hover(|s| s.bg(theme::hex(pal.fill_hover)))
        .child(
            svg()
                .path(icon)
                .w(px(15.0))
                .h(px(15.0))
                .text_color(if active {
                    theme::hex(pal.primary)
                } else {
                    theme::hex(pal.text2)
                }),
        )
        .tooltip(move |window, cx| {
            let tip = tip.clone();
            gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                div().text_size(px(theme::scaled(11.0))).child(tip.clone())
            })
            .build(window, cx)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        bottom_distance, elide_path, http_auth_label, is_auth_error, pick_chip_style, validate_ssh,
        validate_url_addr, version_newer, ChipStyle,
    };
    use crate::i18n;
    use gpui_kit::px;

    #[test]
    fn version_compare_orders_semver() {
        // 前导 v 忽略；0.10 比 0.9 新（不能按字符串比）
        assert!(version_newer("v0.4.0", "0.3.0"));
        assert!(version_newer("0.10.0", "0.9.9"));
        assert!(!version_newer("v0.3.0", "0.3.0"));
        assert!(!version_newer("0.3.0", "0.3.1"));
        // 缺失段补 0；预发布低于同号正式版
        assert!(!version_newer("0.3", "0.3.0"));
        assert!(version_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!version_newer("1.0.0-rc.1", "1.0.0"));
        assert!(version_newer("1.0.0-rc.2", "1.0.0-rc.1"));
    }

    #[test]
    fn fixed_tab_titles_use_semantic_ids() {
        let s = crate::store::Settings::default();
        let blank = super::Tab::blank(&s);
        assert_eq!(blank.title_id, Some("tab.new"));
        assert_eq!(blank.display_title(), i18n::t("tab.new"));

        let settings = super::Tab::settings(&s);
        assert_eq!(settings.title_id, Some("settings.title"));
        assert_eq!(settings.display_title(), i18n::t("settings.title"));

        let text = super::Tab::text(&s, "note.log".into(), "line\n");
        assert_eq!(text.title_id, None);
        assert_eq!(text.display_title(), "note.log");
    }

    #[test]
    fn chip_style_degrades_with_window_width() {
        // 其余控件 700、完整芯片 400、首字母+计数 250、仅首字母 120
        assert_eq!(
            pick_chip_style(1200.0, 700.0, 400.0, 250.0),
            ChipStyle::Full
        );
        // 差 1px 放不下完整名称 → 退到首字母+计数
        assert_eq!(
            pick_chip_style(1099.0, 700.0, 400.0, 250.0),
            ChipStyle::Short
        );
        assert_eq!(
            pick_chip_style(960.0, 700.0, 400.0, 250.0),
            ChipStyle::Short
        );
        // 连计数也放不下 → 只留首字母
        assert_eq!(
            pick_chip_style(840.0, 700.0, 400.0, 250.0),
            ChipStyle::Letter
        );
    }

    #[test]
    fn elide_path_keeps_head_and_tail_segments() {
        // 近似等宽:每字符 7px
        let mut m = |t: &str| px(t.chars().count() as f32 * 7.0);
        let p = "/tmp/aaaaaa/vvvv/bbbbb/xxxxxxxxxx/a.log";
        // 放得下 → 原样返回
        assert_eq!(elide_path(p, px(4000.0), &mut m), p);
        // 中等宽度 → 中间段折叠,首段与文件名保留
        let out = elide_path(p, px(210.0), &mut m);
        assert!(out.starts_with("/tmp"), "{out}");
        assert!(out.ends_with("a.log"), "{out}");
        assert!(out.contains("/…/"), "{out}");
        // 极窄 → 退化字符省略,仍以文件名结尾
        let out = elide_path(p, px(80.0), &mut m);
        assert!(out.ends_with("a.log"), "{out}");
        assert!(out.contains('…'), "{out}");
    }

    #[test]
    fn bottom_distance_covers_both_sign_conventions() {
        // max 为负:贴底时 offset == max
        assert_eq!(bottom_distance(px(-1000.0), px(-1000.0)), px(0.0));
        // max 为正:贴底时 offset == -max
        assert_eq!(bottom_distance(px(-1000.0), px(1000.0)), px(0.0));
        // 顶部离底部 = 整段可滚动量(与符号无关)
        assert_eq!(bottom_distance(px(0.0), px(1000.0)), px(1000.0));
        assert_eq!(bottom_distance(px(0.0), px(-1000.0)), px(1000.0));
        // 中间位置
        assert_eq!(bottom_distance(px(-500.0), px(1000.0)), px(500.0));
    }

    #[test]
    fn auth_kind_labels() {
        use crate::source::HttpAuthKind as K;
        assert_eq!(http_auth_label(K::None), i18n::t("http.auth.none"));
        assert_eq!(http_auth_label(K::Basic), "Basic");
        assert_eq!(http_auth_label(K::Bearer), "Bearer");
        assert_eq!(http_auth_label(K::ApiKey), "API Key");
        assert_eq!(http_auth_label(K::Header), i18n::t("http.header.custom"));
    }

    #[test]
    fn detects_auth_errors() {
        assert!(is_auth_error("SSH 认证失败(检查用户名/密码/密钥/agent)"));
        assert!(is_auth_error("HTTP 401 Unauthorized(认证失败)"));
        assert!(is_auth_error("HTTP 403 Forbidden(认证失败)"));
        assert!(!is_auth_error("SSH 连接失败 h:22: Connection refused"));
        assert!(!is_auth_error("HTTP 404 Not Found"));
        assert!(!is_auth_error("本地文件不存在: /nope"));
    }

    #[test]
    fn ssh_requires_remote_path() {
        let msg = validate_ssh("h", "u", "  ", "22").unwrap();
        assert!(msg.contains("远程日志路径"), "{msg}");
        assert!(validate_ssh("h", "u", "/var/log/app.log", "22").is_none());
        assert!(validate_ssh("", "u", "/a.log", "22")
            .unwrap()
            .contains(i18n::t("ssh.host")));
        assert!(validate_ssh("h", "", "/a.log", "22")
            .unwrap()
            .contains(i18n::t("ssh.username")));
        assert!(validate_ssh("h", "u", "/a.log", "0")
            .unwrap()
            .contains(i18n::t("ssh.port")));
        assert!(validate_ssh("h", "u", "/a.log", "abc")
            .unwrap()
            .contains(i18n::t("ssh.port")));
    }

    #[test]
    fn url_requires_address() {
        assert!(validate_url_addr("   ").unwrap().contains("日志地址"));
        assert!(validate_url_addr("example.com/a.log")
            .unwrap()
            .contains("http"));
        assert!(validate_url_addr("https://example.com/a.log").is_none());
    }
}
