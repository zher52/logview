# 日志查看器 设计文档

> 日志查看器 · Rust + egui 原生桌面应用(无 WebView)· 参考 Chrome DevTools 控制台样式
> 状态:**待审核**(审核通过 → figcraft 原型 → 确认 → 开发)

## 1. 需求映射

| # | 需求 | 方案 |
|---|------|------|
| 1 | 打开本地/SSH 远端/HTTP 链接日志文件 | 三种 Source:本地 fs 增量读 / SSH SFTP 分块读 / HTTP Range 增量拉取(SSRF 防护) |
| 2 | 正则(默认+自定义多配置)分析级别、着色、筛选 | 内置 3 套规则 + 用户多套命名配置,可启停/排序;6 级别着色;级别 chips 筛选(带计数) |
| 3 | 高亮检索、过滤检索、关键字检索 | 单搜索框:模式切换(高亮/过滤),开关(正则/大小写);高亮模式支持 Enter/▲▼ 跳转匹配 |
| 4 | 自动追加 | 每标签独立 worker 线程轮询增量读取;粘底跟随;上滚自动暂停;浮动按钮回到底部;截断/轮换检测 |
| 5 | 识别文本/文件路径/URL,新标签页打开 | 行内 URL/绝对路径(本地或继承 SSH 上下文)/相对日志路径 识别点击;JSON 行可 pretty 打开;右键菜单 |

## 2. 技术栈(已确认)

| 项 | 选择 | 说明 |
|----|------|------|
| UI | egui 0.29 + eframe(glow) | 自绘,无 WebView;`show_rows` 虚拟滚动,万行只画可见行 |
| SSH | ssh2 crate(libssh2) | 密码 / 私钥(文本或文件)/ 系统 agent;SFTP stat + seek 读 |
| HTTP | reqwest blocking + rustls | Range 请求;`resolve()` 固定已校验 IP,防 DNS rebinding |
| 文件选择 | rfd | 原生文件对话框(三平台) |
| 持久化 | serde_json | `~/<config_dir>/logview/settings.json` |
| 并发 | std::thread + crossbeam-channel | 每标签一个 worker 线程 |

## 3. 总体架构

```
┌──────────────────────────── UI 主线程(egui) ────────────────────────────┐
│ 标签栏(多日志源标签)                                                    │
│ 工具栏:级别 chips(计数/开关) │ 搜索框(Aa/.* | 高亮⇄过滤) │ 跟进/清屏/设置 │
│ 日志区:虚拟滚动 + 行号 gutter + 级别圆点 + 分段着色 + 链接下划线          │
│ 状态栏:行数 · 字节进度 · 轮询间隔 · 来源摘要                              │
│ 浮动按钮:↓ N 新行(未粘底时)                                             │
└──────┬────────── channel: Cmd(Pause/SeekStart/SeekEnd/Stop) ────────────┘
       │                    ↑ channel: Msg(Append/Replace/Truncated/Error/Partial)
┌──────┴──────── Tail Worker 线程 × N(每标签一个) ─────────────────────────┐
│ poll 周期(默认 1s):stat size → 增量读 → 分帧发 Append                    │
│ size < offset → Truncated(轮换):UI 插分隔线,worker 从 0 继续读           │
│ 初始加载:头部 4MB;超出 → Partial 通知,UI 出 banner[加载全部][跳到末尾]   │
│ 跳到末尾:offset=size,回退至行边界,读尾部 2MB → Replace                   │
└──────┬──────────────────────────────────────────────────────────────────┘
       │
┌──────┴──────── Source 层 ────────────────────────────────────────────────┐
│ LocalSource: fs.stat + seek 读                                           │
│ SshSource : ssh2 Session + SFTP(stat/open/seek/readdir)                  │
│ HttpSource: Range 探测 → 206 增量 / 200 全量(不支持跟随)                │
│ CommandSrc: 本地 spawn / ssh2 exec(channel 读)→ 按行入 store            │
│ ssrf.rs   : 仅 http/https;DNS 全地址逐个校验,拒绝 localhost/环回/私有/  │
│             保留网段(IPv4+IPv6);固定 IP 连接;重定向逐跳复检;大小上限  │
└──────────────────────────────────────────────────────────────────────────┘
```

## 4. 模块划分

```
src/
├── main.rs        eframe 启动、窗口选项
├── theme.rs       DevTools 深色配色(面板/文字/级别色/highlight 色)
├── level.rs       Level 枚举(Trace/Debug/Info/Warn/Error/Fatal/None)、
│                  级别名映射(err→Error, warning→Warn…)、LevelConfig 多套配置、
│                  正则编译引擎(规则序 = 优先序,首中即停;capture 组自动映射级别)
├── lines.rs       LineStore:字节流→行(UTF-8 断字 carry)、ANSI 剥离、级别标注、
│                  各级别计数、行数上限(环形丢弃+提示行)、过滤索引(增量维护)
├── ansi.rs        行内 ANSI SGR 解析:剥离控制序列,产出纯文本 + 样式区间
├── search.rs      Matcher:文本 indexOf(大小写)/regex;高亮段计算;过滤判定
├── links.rs       URL / POSIX 路径 / Windows 路径 / 相对日志路径 识别;
│                  与搜索高亮段合并(优先级:高亮 > 链接 > 正文);JSON 行探测
├── source.rs      SourceParams(打开参数)→ OpenedSource(open/size/read_at/browse)
├── ssrf.rs        URL 安全校验(网段表见 §8)
├── tail.rs        worker 协议、分块读、轮换、SeekStart/SeekEnd
├── app.rs         App:标签管理、全局状态、快捷键、配置加载/保存、开新标签入口
│                  工具栏含「级别配置选择器」:下拉多选启用哪几套配置(与设置联动、即时生效并持久化);
│                  级别筛选 chips 的颜色实时取自当前级别颜色配置(与设置中色块一致)
├── store.rs       settings.json 读写(级别配置/颜色/间隔/最近记录)
└── ui/
    ├── widgets.rs chips、图标开关、segmented、搜索框、浮动按钮
    ├── tabs.rs    标签栏(右键:关闭其他/关闭右侧)
    ├── viewer.rs  日志区渲染(行分段、hover、点击、右键菜单、stick)
    ├── open.rs    打开源对话框(本地/SSH/HTTP 三页 + 最近记录)
    └── settings.rs 设置对话框(多配置管理/颜色/间隔/导入导出)
```

## 5. 级别配置数据模型

```rust
enum Level { Trace, Debug, Info, Warn, Error, Fatal, None }

struct LevelRule {
    level: RuleLevel,      // 固定级别 或 Auto(取 capture 组 1 → 别名映射)
    regex: String,
    flags: String,         // 如 "i"
}
struct LevelConfig { name, enabled, built_in, rules: Vec<LevelRule> }
```

内置配置(默认全启用,按序判定,首中即停):
1. **通用单词**:`\b(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|ERR|FATAL|CRITICAL|PANIC|SEVERE)\b`(i,Auto)
2. **logfmt**:`(?:^|[\s"'[](?:level|lvl|severity)[=:](\w+)`(i,Auto)
3. **方括号**:`\[(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|ERR|FATAL|CRITICAL|PANIC)\]`(i,Auto)

别名映射:err→Error,severe→Error,critical/fatal/panic/emerg/alert→Fatal,
warning→Warn,notice→Info,verbose→Debug,dbg→Debug…

级别配色(可自定义):
Trace `#7d8085` 斜体 · Debug `#9aa0a6` · Info `#e3e3e3` · Warn `#faa64b` ·
Error `#f28b82`(行底色 10% + 红左条)· Fatal `#f28b82` 加粗(底色 18%)

## 6. 检索模型

```
SearchState { query, mode: 高亮|过滤, regex: bool, case_sensitive: bool }
```
- **过滤模式**:LineStore 维护匹配索引(追加时增量判定);级别筛选 + 搜索取交集
- **高亮模式**:全部行可见,匹配段加背景色,匹配计数,Enter/▲▼ 循环跳转
- 正则无效输入:输入框红边 + 错误提示,不影响现有视图

## 7. 线程协议

```
Cmd(UI→worker): Stop | Pause(bool) | SeekStart | SeekEnd | SeekOffset(u64)
Msg(worker→UI):
  Replace { bytes, offset, size }   // 跳到末尾后整体替换
  Append  { bytes, offset, size }   // 增量追加(UI 自行处理 UTF-8 断字)
  Partial { loaded, total }         // 初始加载未完,出 banner
  Truncated { size }                // 轮换/截断
  Error   { msg, fatal }
  NoFollow{ size }                  // HTTP 不支持 Range,无法跟随
  Exited  { code, retry_in }        // 命令退出:显示退出码;retry_in = 若可重试,下次重开倒计时
```

命令源复用同一 worker,但走「流式」分支:常驻读取线程把 stdout/stderr 合并后送入 channel,worker 先追加到本机 spool(临时文件),再按 4MB/轮的预算补发给 UI。`Pause` 只停止补发、继续落盘;`SeekStart` 从 spool 开头回放,`SeekEnd` 取 spool 尾部,`Clear` 截断 spool。spool 上限 128MB,超出丢最早一半;关闭标签/退出删除,崩溃残留在下次启动清理。

SSH 源与 SSH 命令都不直接建连,而是交给 `session.rs` 的会话层:复用键是「用户 + 主机 + 端口 + 凭据摘要」,同键的多标签共享同一条 `ssh2::Session`(因此日志里点开远端路径不会再握手一次,文件源与命令源也共用)。会话登记在进程级注册表里,句柄是 `Arc`;最后一个句柄释放后保留 30 秒,由清扫线程断开空闲连接。共享会话在认证后切到**非阻塞**模式:libssh2 的阻塞调用会锁住整个会话,一个标签的读就会卡住其它标签;SFTP 调用在 10 秒预算内重试 `EAGAIN`,命令通道由读端自己退避。连接失效时下一次操作按需重连(沿用首次的主机密钥决定,密钥变化仍被拒绝),并把旧代次通道的迟到错误挡在新连接之外。

## 8. SSRF 安全策略(按安全约束实现)

- 仅 `http://` / `https://`;请求前解析 DNS,**所有**解析地址逐个校验
- 拒绝:localhost、环回、私有、保留网段 —— IPv4(0/8,10/8,100.64/10,127/8,
  169.254/16,172.16/12,192.0.0/24,192.0.2/24,192.88.99/24,192.168/16,198.18/15,
  198.51.100/24,203.0.113/24,224/4,240/4)与 IPv6(::,::1,fc00::/7,fe80::/10,
  ::ffff: 映射段回查 v4,2001:db8::/32)
- 连接固定到已校验 IP + Host 头(防 rebinding);重定向每一跳重新走完整校验
- 单次响应大小上限;被拒 URL 在 UI 明确报错
- ⚠️ 因此**内网 HTTP 日志地址无法访问**(约束要求);可用 SSH/本地路径替代

## 9. 交互细节

- **跟随**:新标签默认进入"跳到末尾 + 跟随";`stick_to_bottom`;用户上滚 → 自动停跟随,浮动 `↓ N 新行`
- **链接点击**:
  - URL → 应用内新日志标签(HTTP 源);右键可"在浏览器打开"
  - 路径 → 新标签;当前标签是 SSH 源 → 按同主机同凭据的远端路径打开(密码仅内存保存,不落盘);同一主机的多标签共享一条 SSH 会话,不重新握手认证
  - JSON 行 → 行尾 `{}` 徽标 → 新 JSON 标签(pretty)
- **快捷键**:Cmd/Ctrl+T 打开源 · Cmd/Ctrl+W 关标签 · Cmd/Ctrl+F 焦点搜索 · Esc 关对话框 · Cmd/Ctrl+K 清屏
- **清屏**:保留 offset,只清已显示行(继续追加新内容)
- 行号 gutter 默认开(暗灰右对齐);级别圆点:仅 Warn+ 显示,降低视觉噪音

## 10. 打包

- macOS:`cargo build --release --target x86_64-apple-darwin` + `.app` bundle 脚本 + dmg
- Windows / Linux:同一代码,后续在对应平台构建(msi/AppImage),代码不设平台障碍
- 安装包预期 ~15MB

## 11. 里程碑

| 阶段 | 内容 | 验收 |
|------|------|------|
| M1 | 骨架 + 本地文件源 + 虚拟滚动 + 自动追加 | 打开大文件流畅,追新增量 <1s |
| M2 | 级别配置系统 + 着色 + chips 筛选 + 检索三模式 | 内置配置命中常见格式 |
| M3 | SSH / HTTP 源 + SSRF 校验 | 密码/密钥可连,内网 URL 被拒且提示明确 |
| M4 | 链接识别 + 新标签打开 + JSON pretty + 右键菜单 | URL/路径/JSON 全可点 |
| M5 | 设置持久化 + 快捷键 + 打包 | 配置可导入导出,dmg 可分发 |
| M6 | 命令源(本地 / SSH)+ 预设数据化 | `adb logcat`、`journalctl -f` 可看可跟;退出码与重试可见 |

## 12. 命令源(0.3.0)

**定位**:日志往往来自「持续输出的命令」而不只是文件。命令源把任意长驻命令的输出变成标签页,与文件源共用级别分析、检索、跟随与行数上限。

- **入口两处、一套表单**:欢迎页「命令」= 本地;SSH 对话框「命令」分页 = 远端。二者共用 `CmdFields`(预设选择 + 字段值 + 自定义 argv + 自动重试)。
- **预设是数据**:`Settings.command_presets` 持久化整套预设(argv 模板、字段、`resolve` 预解析步骤、`level_config_id`);`builtin_presets()` 只是首次启动与按 id 补齐的默认值,用户修改不被覆盖。
- **展开规则**:`${field}` 替换;值为空的可选参数连同紧邻 `-` 开头的开关一起丢弃(`["-s","${serial}"]` → 无);`resolve` 在执行前跑一条短命令取结果(如 `adb shell pidof -s pkg` → `${pid}`),失败即报错。
- **执行边界**:本地按 argv `exec`,不经 shell(可执行文件由 `PATH` 解析);远端拼成一条命令行交给登录 shell(`sh -c`),每个参数单引号转义 —— 因此支持管道与重定向,但不承诺 argv 级安全,这一点在 README 里明说。
- **生命周期**:进程退出后标签保留输出并标注退出码;预设开启「退出后重试」时按 1s→2s→…(上限 30s)退避重开,适合 `adb logcat` 设备重连、容器重启;用户手动关闭标签即停止并回收子进程。输出缓存(spool)让重连前后的内容连续保留,暂停/回看也不会丢日志。
- **不做的事**:不提供通用 SSH 终端;不做 PTY;不模拟终端重绘。ANSI 只剥离控制序列并渲染行内 SGR 颜色/字形,不实现光标移动与全屏终端语义。
