// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 极简 i18n:以中文原文为 key,英文缺失时回退中文。
//!
//! 这样界面代码可以原地 `i18n::t("取消")` 渐进迁移,新增文案不写英文也能正常显示。
//! 切换语言只需 `set` + 重新渲染;持久化字段是 `Settings::language`。

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Zh,
    En,
}

static CURRENT: AtomicU8 = AtomicU8::new(Lang::Zh as u8);

impl Lang {
    pub fn from_code(code: &str) -> Self {
        if code.eq_ignore_ascii_case("en") {
            Lang::En
        } else {
            Lang::Zh
        }
    }

    /// gpui-kit 组件库使用的 locale 标识
    pub fn locale(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CN",
            Lang::En => "en",
        }
    }
}

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::En as u8 {
        Lang::En
    } else {
        Lang::Zh
    }
}

/// 取当前语言的文案;英文表未收录时回退中文原文
pub fn t(key: &'static str) -> &'static str {
    if lang() == Lang::Zh {
        return key;
    }
    lookup(key).unwrap_or(key)
}

/// 动态文案(错误/提示)翻译:按整串精确查表,未收录时原样返回
pub fn td(text: &str) -> String {
    if lang() == Lang::Zh {
        return text.to_string();
    }
    lookup(text)
        .map(str::to_string)
        .unwrap_or_else(|| text.to_string())
}

fn lookup(key: &str) -> Option<&'static str> {
    EN.iter().find(|(zh, _)| *zh == key).map(|(_, en)| *en)
}

/// 带 `{name}` 占位符的模板翻译:先查表,再替换占位符
pub fn tf(template: &'static str, args: &[(&str, String)]) -> String {
    let mut out = t(template).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// 中文原文 → 英文;逐步补齐,未收录的条目自动回退中文
const EN: &[(&str, &str)] = &[
    ("无级别", "None"),
    ("取消", "Cancel"),
    ("连接", "Connect"),
    ("运行", "Run"),
    ("打开", "Open"),
    ("仅本次信任", "Trust once"),
    ("信任并记住", "Trust and remember"),
    ("SSH 连接", "SSH connection"),
    ("运行命令", "Run command"),
    ("确认主机密钥", "Confirm host key"),
    ("打开 URL", "Open URL"),
    ("＋ 新建配置", "＋ New config"),
    ("导入 JSON", "Import JSON"),
    ("导出 JSON", "Export JSON"),
    ("恢复内置默认", "Restore built-in defaults"),
    ("删除配置", "Delete config"),
    ("恢复默认", "Restore default"),
    ("＋ 添加规则", "＋ Add rule"),
    ("外观与显示", "Appearance"),
    ("日志与性能", "Logging & performance"),
    ("分析配置", "Analysis rules"),
    ("快捷键", "Shortcuts"),
    ("界面语言", "Language"),
    (
        "切换中文 / English(部分文案仍在迁移)",
        "Switch 中文 / English (some text is still being migrated)",
    ),
    ("设置", "Settings"),
    ("搜索设置", "Search settings"),
    ("纯文本", "Plain text"),
    ("只读预览(点击退出编辑)", "Read-only preview (click to exit editing)"),
    ("编辑源码(⌘S 保存)", "Edit source (⌘S to save)"),
    ("保存修改(⌘S)", "Save changes (⌘S)"),
    ("文档较大,已显示源码", "Document too large; showing source"),
    ("关闭其他", "Close others"),
    ("高亮", "Highlight"),
    ("过滤", "Filter"),
    ("隐藏", "Hide"),
    ("显示", "Show"),
    ("自动换行:已开启(点击关闭)", "Word wrap: on (click to turn off)"),
    ("自动换行:已关闭(点击开启)", "Word wrap: off (click to turn on)"),
    ("加载全部", "Load all"),
    ("跳到末尾", "Jump to end"),
    ("保存条数", "History limit"),
    ("最多保留多少条最近打开记录(1–500)", "How many recent entries to keep (1–500)"),
    (
        "移除或清空 URL 最近打开记录时,同时删除系统钥匙串中的对应凭据",
        "Also delete matching system keychain credentials when removing or clearing recent URLs",
    ),
    ("显示行号", "Show line numbers"),
    ("在日志行左侧显示行号", "Show line numbers at the left of each log line"),
    ("显示时间戳", "Show timestamps"),
    (
        "在日志行左侧显示该行的本地时间(本次查看期间新到达的行;已存在的历史行留空)",
        "Show each line's local arrival time (only for lines received while viewing; older lines stay blank)",
    ),
    ("默认自动换行", "Word wrap by default"),
    ("新标签页的初始值;标签页内可单独切换", "Initial value for new tabs; can be toggled per tab"),
    ("界面字号", "UI font size"),
    ("设置、工具栏等界面文字大小(9–24 pt)", "Size of UI text in settings and toolbars (9–24 pt)"),
    ("日志字号", "Log font size"),
    ("日志文本大小,行高随之等比变化(9–24 pt)", "Log text size; line height scales with it (9–24 pt)"),
    ("界面字体", "UI font"),
    ("留空跟随系统 UI 字体(如 PingFang SC)", "Leave empty to follow the system UI font (e.g. PingFang SC)"),
    ("日志字体", "Log font"),
    (
        "留空跟随系统等宽字体(如 Menlo、JetBrains Mono)",
        "Leave empty to follow the system monospace font (e.g. Menlo, JetBrains Mono)",
    ),
    ("主题", "Theme"),
    ("日志级别", "Log levels"),
    ("搜索高亮", "Search highlight"),
    ("底色", "Background"),
    ("亮色", "Light"),
    ("暗色", "Dark"),
    ("轮询间隔", "Poll interval"),
    ("检查新增内容的间隔(≥200 ms)", "How often to check for new content (≥200 ms)"),
    ("初始加载", "Initial load"),
    ("打开日志时从开头读取的大小(1–512 MB)", "Bytes read from the start when opening a log (1–512 MB)"),
    ("尾部加载上限", "Tail load limit"),
    ("「跳到末尾」一次读取的大小(1–512 MB)", "Bytes read by \"Jump to end\" (1–512 MB)"),
    ("行数上限", "Line limit"),
    ("超出后环形丢弃最旧行(≥1000)", "Oldest lines are dropped in a ring buffer beyond this (≥1000)"),
    ("配置导入 / 导出", "Config import / export"),
    (
        "导出全部分析配置为 JSON;导入会整体替换现有配置",
        "Export all analysis configs as JSON; importing replaces the current set",
    ),
    ("内置配置", "Built-in config"),
    ("自定义配置", "Custom config"),
    ("启用(新标签页默认)", "Enabled (default for new tabs)"),
    ("正则(捕获组 1 取级别文本)", "Regex (capture group 1 provides the level text)"),
    ("标志", "Flags"),
    ("自动", "Auto"),
    ("级别", "Level"),
    ("取捕获组", "Capture group"),
    ("匹配规则", "Matching rules"),
    (
        "暂无配置,点击左侧「新建配置」创建",
        "No configs yet. Click \"New config\" on the left to create one.",
    ),
    ("按下组合键…", "Press a shortcut…"),
    ("修改", "Change"),
    ("界面行号换行、字号字体与颜色管理", "Line numbers, wrapping, fonts and colors"),
    ("自动追加、加载大小与内存上限", "Auto-follow, load sizes and memory limits"),
    (
        "级别分析配置:正则规则、级别映射与启用状态",
        "Level analysis: regex rules, level mapping and enablement",
    ),
    (
        "已保存的主机、URL 凭据与最近打开记录的管理",
        "Saved hosts, URL credentials and recent entries",
    ),
    ("全局快捷键一览(固定绑定)", "Global shortcuts (fixed bindings)"),
    ("· 内置", "Built-in"),
    (" · 内置", " · built-in"),
    ("用户名", "Username"),
    ("密码", "Password"),
    ("端口", "Port"),
    ("主机", "Host"),
    ("认证", "Authentication"),
    ("无认证", "No authentication"),
    ("自定义请求头", "Custom header"),
    ("请求头值", "Header value"),
    (
        "主机名或 IP,输入时可搜索已保存的服务器",
        "Hostname or IP; type to search saved servers",
    ),
    ("密码(Agent 认证可留空)", "Password (optional for agent auth)"),
    ("例如 adb logcat -v threadtime", "e.g. adb logcat -v threadtime"),
    (
        "检索:高亮 / 过滤(支持正则)",
        "Search: highlight / filter (regex supported)",
    ),
    ("新标签页", "New tab"),
    ("最近打开", "Recent"),
    ("编辑", "Edit"),
    ("删除", "Delete"),
    ("命令", "Command"),
    ("新配置", "New config"),
    ("服务器", "Server"),
    ("方式", "Method"),
    ("文件", "File"),
    ("远程路径", "Remote path"),
    ("无", "None"),
    ("自定义头", "Custom header"),
    ("头名称", "Header name"),
    ("头值", "Header value"),
    ("分析配置(当前标签页)", "Analysis configs (current tab)"),
    ("在设置中管理…", "Manage in settings…"),
    ("也可以把日志文件直接拖入窗口", "You can also drag a log file into the window"),
    ("确认清除", "Confirm clear"),
    ("清除", "Clear"),
    ("＋ 新增主机", "＋ Add host"),
    ("从最近打开导入", "Import from recent"),
    ("新增主机", "Add host"),
    ("编辑主机", "Edit host"),
    ("备注", "Note"),
    ("保存", "Save"),
    ("请填写主机名或 IP", "Enter a hostname or IP"),
    ("请填写用户名", "Enter a username"),
    ("端口需为 1–65535", "Port must be 1–65535"),
    ("请填写请求头名称", "Enter a header name"),
    ("请填写请求头值", "Enter a header value"),
    ("已恢复该内置配置的默认规则", "Restored defaults for this built-in config"),
    ("已恢复全部内置配置的默认规则", "Restored all built-in config defaults"),
    ("没有匹配的已保存服务器", "No matching saved servers"),
    ("算法", "Algorithm"),
    ("指纹", "Fingerprint"),
    (
        "首次连接该主机,无法自动校验身份。请与服务器管理员提供的指纹核对;",
        "First connection to this host; its identity cannot be verified automatically. Compare the fingerprint with the one provided by the server administrator.",
    ),
    (
        "不一致时可能存在中间人攻击。选择「记住」后,密钥变化将直接拒绝连接。",
        "A mismatch may indicate a man-in-the-middle attack. If you choose Remember, future key changes will be rejected.",
    ),
    (
        "记住连接(凭据保存到系统钥匙串)",
        "Remember connection (credentials stored in the system keychain)",
    ),
    (
        "记住连接(系统钥匙串不可用,仅本次会话有效)",
        "Remember connection (system keychain unavailable; valid for this session only)",
    ),
    ("命令退出(code={code}),{secs}s 后重试", "Command exited (code={code}); retrying in {secs}s"),
    ("命令已退出(code={code})", "Command exited (code={code})"),
    ("命令结束,{secs}s 后重试", "Command ended; retrying in {secs}s"),
    ("命令已退出", "Command exited"),
    (
        "已信任并记住 {host}:{port} 的主机密钥",
        "Trusted and saved the host key for {host}:{port}",
    ),
    (
        "已信任 {host}:{port} 的主机密钥(仅本次)",
        "Trusted the host key for {host}:{port} (this session only)",
    ),
    ("已取消连接 {host}:{port}", "Cancelled connection to {host}:{port}"),
    ("已导出到 {path}", "Exported to {path}"),
    ("导出失败:{e}", "Export failed: {e}"),
    ("已导入 {n} 套配置", "Imported {n} config sets"),
    (
        "导入失败:文件不是有效的配置 JSON",
        "Import failed: the file is not valid config JSON",
    ),
    ("导入失败:{e}", "Import failed: {e}"),
    ("{n} 行", "{n} lines"),
    ("已丢弃 {n}", "Dropped {n}"),
    ("已接收 {size}", "Received {size}"),
    ("加载中 {pct}%", "Loading {pct}%"),
    ("轮询 {ms}ms", "Polling every {ms} ms"),
    ("重新分析级别中…", "Re-analyzing levels…"),
    ("该源不支持自动跟随", "This source does not support auto-follow"),
    (
        "从头加载全部(大文件可能耗时较久)",
        "Load all from the start (may take a while for large files)",
    ),
    (
        "以 Authorization: Bearer <token> 发送",
        "Sent as Authorization: Bearer <token>",
    ),
    (
        "API Key 以请求头发送,名称默认 X-API-Key,可修改",
        "API key is sent as a header; default name X-API-Key, editable",
    ),
    (
        "自定义请求头,例如 X-Api-Key、X-Token 等",
        "Custom header, e.g. X-Api-Key or X-Token",
    ),
    ("不携带认证信息", "No authentication information is sent"),
    (
        "认证头只发送到初始主机,重定向到其他域名时不携带;密码/token/头值默认不写入最近打开。",
        "Auth headers are only sent to the original host and dropped on cross-domain redirects; passwords, tokens and header values are not saved to recent entries by default.",
    ),
    ("凭据", "Credentials"),
    ("保存凭据", "Save credentials"),
    ("仅记录历史", "History only"),
    (
        "只记录地址与认证方式到最近打开,不保存密码/token;已保存的旧凭据不会自动删除,可在设置→连接里管理",
        "Only the address and auth method are saved to recent entries; passwords and tokens are not. Existing saved credentials are not deleted automatically; manage them in Settings → Connections.",
    ),
    (
        "写入系统钥匙串,下次连接自动回填",
        "Stored in the system keychain and filled in automatically next time",
    ),
    (
        "系统钥匙串不可用,凭据仅本次会话有效",
        "System keychain unavailable; credentials are valid for this session only",
    ),
    ("已暂停", "Paused"),
    ("跟进中", "Following"),
    ("+{n} 新行", "+{n} new lines"),
    ("匹配 {n} 行", "{n} matching lines"),
    ("正则错误:{e}", "Regex error: {e}"),
    (
        "已加载头部 {loaded} / {total}(文件更大)",
        "Loaded first {loaded} of {total} (file is larger)",
    ),
    (
        "已跳至末尾,从字节 {off} / {total} 开始 — 新日志将自动追加",
        "Jumped to end, starting at byte {off} of {total}; new logs will follow automatically",
    ),
    (
        "没有与「{query}」匹配的设置项",
        "No settings match \"{query}\"",
    ),
    (
        "{n} 项设置匹配「{query}」",
        "{n} settings match \"{query}\"",
    ),
    ("{name}:{n} 行(点击{action})", "{name}: {n} lines (click to {action})"),
    ("已与「{desc}」互换快捷键", "Swapped shortcut with \"{desc}\""),
    ("已设为 {shortcut}", "Set to {shortcut}"),
    (
        "分析配置:{on}/{total} 已启用,点击选择",
        "Analysis: {on}/{total} enabled; click to choose",
    ),
    ("将执行:{cmd}", "Will run: {cmd}"),
    ("端口无效: {e}", "Invalid port: {e}"),
    ("保存失败:{e}", "Save failed: {e}"),
    ("{host}:{port} · 用户 {user}", "{host}:{port} · user {user}"),
    ("{status} · {n} 条规则{suffix}", "{status} · {n} rules{suffix}"),
    ("已启用", "Enabled"),
    ("未启用", "Disabled"),
    ("{n} 条规则{suffix}", "{n} rules{suffix}"),
    (
        "~/.ssh/config 里没有可导入的具体主机;通配与 Match 块已跳过",
        "~/.ssh/config has no concrete hosts to import; wildcard and Match blocks were skipped",
    ),
    (
        "~/.ssh/config 中的主机均已存在,跳过 {n} 个重复项",
        "All hosts in ~/.ssh/config already exist; skipped {n} duplicates",
    ),
    ("已从 ~/.ssh/config 导入 {n} 个主机", "Imported {n} hosts from ~/.ssh/config"),
    (
        "已从 ~/.ssh/config 导入 {n} 个主机,跳过 {d} 个重复项",
        "Imported {n} hosts from ~/.ssh/config; skipped {d} duplicates",
    ),
    ("已删除该 URL 凭据", "Deleted the URL credentials"),
    ("删除失败:{e}", "Delete failed: {e}"),
    ("不允许设置请求头: {name}", "Header not allowed: {name}"),
    ("凭据删除失败:{e}", "Failed to delete credentials: {e}"),
    ("在新标签页打开 {target}", "Open {target} in a new tab"),
    ("从头加载", "Load from start"),
    ("已复制路径", "Path copied"),
    ("请填写主机与用户名", "Enter a host and username"),
    ("命令流不支持大小/随机读取", "Command streams do not support size or random access"),
    ("命令流不支持随机读取", "Command streams do not support random access"),
    (
        "服务器忽略 Range 请求,无法增量读取",
        "The server ignored the Range request; incremental reading is unavailable",
    ),
    (
        "SSH 认证失败(检查用户名/密码/密钥/agent)",
        "SSH authentication failed (check username, password, key or agent)",
    ),
    ("URL 缺少主机名", "The URL is missing a hostname"),
    ("重定向缺少 Location", "Redirect is missing a Location header"),
    ("重定向 Location 无效", "Redirect Location is invalid"),
    ("重定向次数过多", "Too many redirects"),
    ("不是 SSH 目标", "Not an SSH target"),
    ("不是命令源", "Not a command source"),
    (
        "命令源需通过 open_command 打开",
        "Command sources must be opened via open_command",
    ),
    ("命令为空", "Command is empty"),
    ("无法获取服务器主机密钥", "Unable to obtain the server host key"),
    ("未知算法", "Unknown algorithm"),
    ("命令源缺少执行计划", "Command source is missing an execution plan"),
    ("命令源返回了非流式句柄", "Command source returned a non-streaming handle"),
    (
        "文件不存在或不是普通文件: {path}",
        "File does not exist or is not a regular file: {path}",
    ),
    ("HTTP {status}(认证失败)", "HTTP {status} (authentication failed)"),
    ("stat 失败: {e}", "stat failed: {e}"),
    ("SFTP 通道失败: {e}", "SFTP channel failed: {e}"),
    ("SFTP stat 失败: {e}", "SFTP stat failed: {e}"),
    ("SFTP open 失败: {e}", "SFTP open failed: {e}"),
    ("SFTP seek 失败: {e}", "SFTP seek failed: {e}"),
    ("SFTP 读取失败: {e}", "SFTP read failed: {e}"),
    ("打开失败: {e}", "Open failed: {e}"),
    ("seek 失败: {e}", "Seek failed: {e}"),
    ("读取失败: {e}", "Read failed: {e}"),
    ("HTTP 读取失败: {e}", "HTTP read failed: {e}"),
    (
        "SSH 连接失败 {host}:{port}: {e}",
        "SSH connection failed {host}:{port}: {e}",
    ),
    ("SSH 握手失败: {e}", "SSH handshake failed: {e}"),
    (
        "已取消连接:{host}:{port} 的主机密钥未确认",
        "Connection cancelled: the host key for {host}:{port} was not confirmed",
    ),
    ("HTTP client 构建失败: {e}", "Failed to build the HTTP client: {e}"),
    ("Bearer token 无效: {e}", "Invalid bearer token: {e}"),
    ("请求头名称无效: {e}", "Invalid header name: {e}"),
    ("请求头值无效: {e}", "Invalid header value: {e}"),
    ("HTTP 请求失败: {e}", "HTTP request failed: {e}"),
    ("取值命令失败(code={code})", "Value command failed (code={code})"),
    ("取值命令失败:{err}", "Value command failed: {err}"),
    (
        "取值失败:{key} 没有输出(目标是否在运行?参数是否正确?)",
        "Failed to resolve {key}: no output (is the target running? are the arguments correct?)",
    ),
    ("命令中有未解析的占位符:{a}", "Unresolved placeholder in command: {a}"),
    ("本地 {path}", "Local {path}"),
    ("命令(本机) {cmd}", "Command (local) {cmd}"),
    ("命令 {user}@{host}:{port} {cmd}", "Command {user}@{host}:{port} {cmd}"),
    (
        "未找到命令 {prog}(请确认已安装,并在 PATH 中)",
        "Command not found: {prog} (is it installed and on PATH?)",
    ),
    ("没有执行权限:{prog}", "Permission denied: {prog}"),
    ("无法启动命令 {prog}:{e}", "Failed to start command {prog}: {e}"),
    ("SSH 通道创建失败:{e}", "SSH channel failed: {e}"),
    ("SSH stderr 合并失败:{e}", "SSH stderr merge failed: {e}"),
    ("远端取值命令执行失败:{e}", "Remote value command failed: {e}"),
    (
        "远端取值命令读取失败:{e}",
        "Failed to read remote value command output: {e}",
    ),
    ("远端命令执行失败:{e}", "Remote command execution failed: {e}"),
    ("取值命令为空", "Value command is empty"),
    ("{e}(重试 {err_count}/5)", "{e} (retry {err_count}/5)"),
    ("日志查看器", "LogView"),
    (
        "未找到已保存的凭据,连接时需重新输入",
        "Saved credentials not found; enter them again when connecting",
    ),
    (
        "{e};连接时需重新输入凭据",
        "{e}; credentials must be re-entered when connecting",
    ),
    (
        "{e};凭据仅在本次会话内有效,下次连接需重新输入",
        "{e}; credentials are valid for this session only",
    ),
    ("已恢复全部内置默认规则", "All built-in default rules restored"),
    ("{msg};请重新输入后重试", "{msg}; please re-enter and try again"),
    ("Basic 认证需要填写用户名", "Basic authentication requires a username"),
    ("Bearer 认证需要填写 token", "Bearer authentication requires a token"),
    ("请求头名称包含非法字符", "Header name contains invalid characters"),
    (
        "远端命令由登录 shell 执行,可用管道与重定向。",
        "Remote commands run in a login shell; pipes and redirection are available.",
    ),
    (
        "本机命令不经过 shell:管道与重定向不生效,参数按引号拆分。",
        "Local commands bypass the shell: pipes and redirection do not apply; arguments are split by quotes.",
    ),
    (
        "还没有保存的主机;可点下方「新增主机」,或从最近打开导入",
        "No saved hosts yet. Add one below, or import from recents.",
    ),
    (
        "删除会同时清理「最近打开」中的同名服务器;系统钥匙串里的凭据不受影响",
        "Deleting also removes the same server from recents; credentials in the system keychain are kept.",
    ),
    ("从 ~/.ssh/config 导入", "Import from ~/.ssh/config"),
    ("用于区分同一主机的不同用途,可留空", "Optional note to distinguish uses of the same host"),
    (
        "没有保存的 URL 凭据;在「打开 URL」里选「保存凭据」后会出现在这里",
        "No saved URL credentials yet; choose “Save credentials” in “Open URL” to add one.",
    ),
    ("检索框内跳到下一处命中", "Jump to the next match in the search box"),
    ("关闭弹层 / 取消快捷键录制", "Close popups / cancel shortcut recording"),
    ("固定绑定", "Fixed binding"),
    ("快捷键提示", "Shortcut hints"),
    ("界面", "Interface"),
    ("字号与字体", "Size and fonts"),
    ("颜色管理", "Colors"),
    ("加载与自动追加", "Loading and follow"),
    ("内存", "Memory"),
    ("已保存的 SSH 主机", "Saved SSH hosts"),
    ("已保存的 URL 凭据", "Saved URL credentials"),
    ("键盘快捷键", "Keyboard shortcuts"),
    ("没有匹配的设置项", "No matching settings"),
    ("搜索结果", "Search results"),
    ("已自动保存", "Saved automatically"),
    ("更改即时生效", "Changes apply immediately"),
    ("所有更改立即生效并自动保存", "All changes apply immediately and are saved automatically"),
    ("在浏览器打开", "Open in browser"),
    ("作为本地文件打开", "Open as local file"),
    ("复制链接", "Copy link"),
    ("复制整行", "Copy whole line"),
    ("复制选中文本", "Copy selected text"),
    ("请按下组合键;Esc 取消", "Press a key combination; Esc to cancel"),
    ("已恢复默认快捷键", "Default shortcut restored"),
    ("已取消修改", "Changes cancelled"),
    ("── 检测到日志轮换 ──", "── log rotation detected ──"),
    ("跟进中(点击暂停跟随)", "Following (click to pause)"),
    ("已暂停(点击恢复跟随并跳到末尾)", "Paused (click to resume and jump to end)"),
    ("跳到末尾(只读文件尾部并继续跟随)", "Jump to end (read the tail and keep following)"),
    ("清屏(保留读取位置,新日志继续追加)", "Clear screen (keep reading position; new logs still append)"),
    ("↓ {n} 条新行", "↓ {n} new lines"),
    ("双击复制完整路径", "Double-click to copy the full path"),
    ("就绪", "Ready"),
    (
        "自动 = 取正则捕获组 1 的文本映射级别(err→ERROR、info→INFO 等);关闭后用所选固定级别。改动立即重分析全部标签。",
        "Auto = map capture group 1 text to a level (err→ERROR, info→INFO, ...); when off, use the fixed level. Changes re-analyze all tabs immediately.",
    ),
    (
        "需要含修饰键的组合(如 ⌘/Ctrl + 字母)",
        "A modifier key is required (e.g. ⌘/Ctrl + a letter)",
    ),
    (
        "请填写远程日志路径(例如 /var/log/app.log)",
        "Enter the remote log path (e.g. /var/log/app.log)",
    ),
    ("预设不存在", "Preset not found"),
    ("自定义", "Custom"),
    (
        "退出后自动重试(断线、设备重连)",
        "Retry automatically after exit (disconnects, device reconnects)",
    ),
    (
        "请填写日志地址(例如 https://example.com/app.log)",
        "Enter the log URL (e.g. https://example.com/app.log)",
    ),
    (
        "URL 需以 http:// 或 https:// 开头",
        "URL must start with http:// or https://",
    ),
    ("请填写命令", "Enter a command"),
    ("条", "items"),
    ("行", "lines"),
    (
        "找不到用户目录,无法写入 known_hosts",
        "Cannot locate the home directory; cannot write known_hosts",
    ),
    ("创建 ~/.ssh 失败:{e}", "Failed to create ~/.ssh: {e}"),
    ("known_hosts 初始化失败:{e}", "Failed to initialize known_hosts: {e}"),
    ("读取 known_hosts 失败:{e}", "Failed to read known_hosts: {e}"),
    ("写入 known_hosts 失败:{e}", "Failed to write known_hosts: {e}"),
    ("保存 known_hosts 失败:{e}", "Failed to save known_hosts: {e}"),
    (
        "SSH 主机密钥校验失败({host}:{port}):known_hosts 无法读取",
        "SSH host key verification failed ({host}:{port}): cannot read known_hosts",
    ),
    (
        "SSH 主机密钥与 known_hosts 记录不一致({host}:{port}),可能存在中间人攻击;确认服务器换过密钥后请先更新 known_hosts",
        "SSH host key does not match known_hosts ({host}:{port}); a man-in-the-middle attack is possible. If the server key changed, update known_hosts first.",
    ),
    ("URL 无效: {e}", "Invalid URL: {e}"),
    (
        "仅允许 http/https 协议(收到: {s})",
        "Only http/https is allowed (got: {s})",
    ),
    (
        "不支持携带用户名/密码的 URL",
        "URLs with embedded username/password are not supported",
    ),
    ("URL 缺少端口", "URL is missing a port"),
    (
        "拒绝 localhost/.local 地址(SSRF 防护)",
        "localhost/.local addresses are blocked (SSRF protection)",
    ),
    ("DNS 解析失败: {e}", "DNS resolution failed: {e}"),
    ("DNS 解析无结果", "DNS returned no results"),
    (
        "拒绝内网/环回/保留地址 {ip}(SSRF 防护,仅允许公网地址)",
        "Blocked internal/loopback/reserved address {ip} (SSRF protection; public addresses only)",
    ),
    ("⚠ 图片缺失:{decoded}", "⚠ Image missing: {decoded}"),
    (
        "⚠ 图片缺失:{decoded}({alt})",
        "⚠ Image missing: {decoded} ({alt})",
    ),
    (
        "无法定位用户主目录,未导入",
        "Cannot locate the home directory; nothing imported",
    ),
    ("读取 {path} 失败: {e}", "Failed to read {path}: {e}"),
    ("初始化系统钥匙串失败: {e}", "Failed to initialize the system keychain: {e}"),
    ("凭据序列化失败: {e}", "Failed to serialize credentials: {e}"),
    ("写入系统钥匙串失败: {e}", "Failed to write to the system keychain: {e}"),
    (
        "已保存的凭据无法解析: {e}",
        "Saved credentials could not be parsed: {e}",
    ),
    ("读取系统钥匙串失败: {e}", "Failed to read from the system keychain: {e}"),
    (
        "删除系统钥匙串条目失败: {e}",
        "Failed to delete the system keychain entry: {e}",
    ),
    (
        "存在以私钥正文保存的连接,无法迁移到钥匙串;建议改用私钥文件",
        "A connection stores the private key text; it cannot be migrated to the keychain. Use a key file instead.",
    ),
    (
        "{e};原明文凭据已移除,连接时需重新输入",
        "{e}; the plaintext credentials were removed; enter them again when connecting",
    ),
    ("请填写{label}", "{label} is required"),
    ("展开后的命令为空", "The expanded command is empty"),
    ("引号未闭合: {q}", "Unclosed quote: {q}"),
    ("logcat 全部", "logcat (all)"),
    ("logcat 包名", "logcat (package)"),
    ("设备", "Device"),
    ("包名", "Package"),
    ("路径", "Path"),
    ("服务", "Service"),
    ("容器", "Container"),
    (
        "留空 = 默认设备(多设备时需填写)",
        "Empty = default device (required when multiple devices are connected)",
    ),
    ("例如 com.example.app", "e.g. com.example.app"),
    ("留空 = 默认设备", "Empty = default device"),
    ("例如 ActivityManager", "e.g. ActivityManager"),
    ("例如 nginx.service", "e.g. nginx.service"),
    ("容器名或 ID", "Container name or ID"),
    ("pod 名称,可带 -c 容器", "Pod name, optionally with -c container"),
    ("通用单词", "Common words"),
    ("方括号 [LEVEL]", "Bracketed [LEVEL]"),
    ("单字母级别 (T/D/I/W/E/V/F)", "Single-letter levels (T/D/I/W/E/V/F)"),
    ("方括号单字母 [D]/[I]/[W]/[E]", "Bracketed single letters [D]/[I]/[W]/[E]"),
    ("新建标签页", "New tab"),
    ("关闭当前标签页", "Close current tab"),
    ("聚焦检索框", "Focus search box"),
    ("清屏", "Clear screen"),
    ("打开设置", "Open settings"),
    ("复制选中行", "Copy selected row"),
    ("保存代码视图修改", "Save code view edits"),
    ("HTTP {status}", "HTTP {status}"),
    ("── 轮换 ──", "── rotation ──"),
    ("关闭", "Close"),
    (
        "删除 URL 记录时清除凭据",
        "Clear credentials when deleting URL history",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_and_falls_back() {
        set(Lang::En);
        assert_eq!(t("取消"), "Cancel");
        assert_eq!(t("跳到末尾"), "Jump to end");
        // 未收录的文案在英文模式下回退中文,不会显示空白
        assert_eq!(t("尚未收录的文案"), "尚未收录的文案");
        assert_eq!(td("尚未收录的文案"), "尚未收录的文案");
        assert_eq!(td("取消"), "Cancel");
        assert_eq!(
            tf(
                "命令退出(code={code}),{secs}s 后重试",
                &[("code", "1".into()), ("secs", "3".into())]
            ),
            "Command exited (code=1); retrying in 3s"
        );
        set(Lang::Zh);
        assert_eq!(t("取消"), "取消");
    }
}
