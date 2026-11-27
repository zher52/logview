# AGENTS.md — 面向 AI 代理的项目说明

本文件供 AI 编码代理与新贡献者快速了解**当前实现**。改动代码前请先阅读；与用户文档冲突时，以 [README](README.md) 与源码为准。

## 项目概述

**日志查看器（Log Viewer / Просмотр журналов）0.4.1**：Rust + GPUI 自绘 UI 的原生桌面日志查看器（**无 WebView**），交互参考 Chrome DevTools 控制台。

- **四种日志源**
  - 本地文件：stat 轮询 + `seek` 增量读
  - SSH 文件：SFTP 分块 `read_at`
  - HTTP(S)：`Range` 增量拉取，内置 SSRF 防护
  - 命令：本机 `spawn` / SSH `exec`，stdout+stderr 合并后先写入本机 spool，再由 worker 补发
- 正则级别分析（内置 + 自定义多配置）、级别着色与筛选、行内 ANSI SGR 颜色、高亮/过滤检索、自动追加（轮换/截断检测）、行内 URL/路径/JSON 识别、代码高亮与 Markdown 预览。
- 许可：**AGPL-3.0-only**（源码头部均有 SPDX 标识）；MSRV **1.92**（`Cargo.toml` 的 `rust-version`）。

### 技术栈

| 层 | 选择 |
|---|---|
| UI | `gpui-kit 0.11`（GPUI / gpui-component），组件与主题令牌，locale 跟随界面语言（`zh-CN` / `en` / `ru`） |
| 并发 | 每标签一个 worker 线程；`crossbeam-channel` 传递命令与消息 |
| 网络 | `reqwest` blocking + rustls；`ssh2`（libssh2）blocking |
| 凭据 | `keyring`（macOS / Windows / Linux 原生后端） |
| 其他 | `regex`、`ropey`、`serde/serde_json`、`chrono`、`dirs`、`open` |

**不要**引入 WebView、tokio 运行时或其他异步栈；保持 blocking + channel 模型。

## 常用命令

```bash
cargo build                      # 调试构建(依赖按 opt-level=2 编译)
cargo run -- sample/app.log      # 打开示例日志
cargo test --locked              # 100 项单元测试
cargo fmt --all                  # CI 会检查
cargo clippy --all-targets --locked

./sample/gen_log.sh sample/app.log 1   # 演示自动追加(会写入受控的示例文件)
```

- `dev.sh` / `package.sh` 是维护者本机脚本，**不入库**（已在 `.gitignore`）；发布构建用 `scripts/make_app.sh`，支持 `TARGET=aarch64-apple-darwin|x86_64-apple-darwin` 交叉构建（产物固定在 `target/日志查看器.app`）。Linux 用 `scripts/make_linux.sh`（需在 Linux 上执行，产出 `target/logview-linux-<arch>.tar.gz` 与可选 AppImage；`SKIP_BUILD=1` 可只做打包自检），桌面资源在 `assets/linux/`。
- 运行 `gen_log.sh` 后提交前请还原：`git checkout -- sample/app.log`。
- CI：`.github/workflows/ci.yml`（macOS 阻塞；Windows/Linux 实验性；clippy 暂不阻塞），`audit.yml` 每周依赖审计，`release.yml` 由 `v*` tag 触发。新增逻辑必须配单测。

## 目录结构

```
src/
├── main.rs        入口:解析 CLI argv、创建主窗口与 gpui-kit 主题
├── app_view.rs    主视图:标签、工具栏、虚拟列表、检索、设置页、打开源对话框、命令对话框、消息轮询
├── theme.rs       antd v5 令牌(暗/亮)、主题色、字号/行高、系统字体回退
├── assets.rs      内置图标 SVG 表(ionicons 子集),其余回退 gpui-kit
├── ansi.rs        行内 ANSI SGR 解析(剥离控制序列,产出纯文本 + 样式区间)
├── level.rs       Level 常量/别名、LevelConfig/Rule、Engine(正则编译 + 逐行判定)
├── lines.rs       LineStore:字节流→行(UTF-8 安全)、行数上限环形丢弃
├── search.rs      SearchState + Matcher(文本/正则、大小写、高亮区间)
├── links.rs       URL/POSIX/Windows/相对路径识别、重叠消解、JSON 行探测
├── ssrf.rs        URL 校验与网段表(纯函数,已被单测覆盖)
├── source.rs      四种源的 open/meta/read_at、SSH 连接与命令执行、HTTP 固定 IP client、凭据结构
├── command.rs     命令预设数据模型:argv 模板、字段、取值命令(resolve)、shell 引用、内置预设
├── tail.rs        worker 协议与两种循环:文件轮询(worker_file) / 命令流式(worker_stream)
├── store.rs       settings.json 读写(含 configs、ssh_hosts、command_presets)、旧数据迁移
├── secrets.rs     系统钥匙串存取(不可用时退回仅内存)
├── ssh_config.rs  解析系统 ssh 配置,导入已保存主机(跳过通配与 Match)
├── i18n.rs       语义 ID 取词：t/tf/td/tn；译文在 locales/*.toml，由 build.rs 编译期生成查表
├── code_input.rs  正则输入框的词法着色
└── code.rs        代码/配置语法高亮、Markdown 渲染辅助、大小限制常量
```

## 架构要点

### 1. 线程模型

UI 主线程 + 每个标签一个 worker。只通过两条 channel 通信，**不要共享锁**：

- UI→worker：`Cmd { Stop, Pause(bool), SeekStart, SeekEnd, Clear }`
- worker→UI：`Msg { Append{data,offset,size,at}, Replace{…}, Partial{loaded,total}, Complete, Truncated{size}, NoFollow, Error{msg,fatal}, Exited{code,retry_in} }`

文件源走 `worker_file`（轮询 + 增量读，1MB/块、每周期 ≤8MB）；命令源走 `worker_stream`（阻塞读 stdout+stderr 合并流，退出后按 `restart` 退避重连，最多 6 次，`backoff()` 在 `tail.rs`）。

`Cmd` 必须在 ~100ms 内响应（`recv_timeout` + 200ms 读超时），不要把控制循环改成长阻塞。命令源把读线程的字节先追加到 spool，再由控制循环按 4MB/轮的预算补发给 UI；暂停只是停止补发，不丢数据。

### 2. 行模型与视图

- `LineStore::push_bytes` 按 `\n` 切字节再解码，天然 UTF-8 断字安全；默认上限 20 万行，超出环形丢弃。ANSI 控制序列在这里剥离，`Line.ansi` 保存纯文本上的样式区间，检索/链接/复制只处理纯文本。
- 级别在 worker 追加时由 `Engine::eval` 判定；配置变更后由 UI 分帧 `relevel_step`（10 万行/帧）重判定。
- `Tab.view: Vec<u32>`（过滤/隐藏级别的可见行）与 `Tab.matched`（检索命中）在追加时增量维护，条件变化时全量重建。
- 虚拟滚动用 `v_virtual_list` + `VirtualListScrollHandle`；自动换行时 `heights` 缓存行高（宽度变化需失效）。
- 跟随语义：贴底才跟进；上滚自动暂停并累计 `pending_new`；回到底部恢复。“跳至末尾”先 `SeekEnd` 再等 `Replace`，banner 由 `pending_tail_banner` 控制。

### 3. 命令源（0.3.0）

- `command.rs` 是**纯数据 + 展开**：`CommandPreset { id, name, local, ssh, argv, fields, resolve, level_config_id, builtin }`；`plan()` 校验必填字段、展开 `${key}`，`resolve` 用于打开前取值（输出第一个词，如 adb PID），每次打开/重连都重新求值。
- 持久化只存 `preset id + values + restart + raw`（`SourceParams::Command`），不存当次 PID。
- `source::open_command`：本地直接 `Command::new(argv[0]).args(…)`（**不经过 shell**）；SSH 走登录 shell，非 `raw` 时逐项单引号转义，`raw` 允许管道/重定向。
- 本地 stdout/stderr 由独立读线程汇入同一 channel；Windows 使用 `CREATE_NO_WINDOW` 避免黑框。
- 命令源有本机 spool（`dirs::cache_dir()/logview/spool/`，单文件上限 128MB，0600）：`SeekStart` 从缓存开头回放，`SeekEnd` 取缓存尾部，`Clear` 截断缓存；暂停期间继续落盘、恢复后补发。缓存关闭标签/退出时删除，异常退出残留在启动时清理（超过 24h）。缓存不可写时退化为直通模式并在 UI 提示。

### 4. 配置与持久化

- 路径：`dirs::config_dir()/logview/settings.json`。
- `settings_rev` 是配置变更的驱动信号：变更后各标签重编译 `Engine` 并按需重判定，同时防抖落盘。
- 标签页的启用配置是 `Tab.enabled_ids`；设置里的 `configs[].enabled` 只是新标签页默认值。
- `command_presets` 与内置预设按 id 合并（升级后新增内置项自动补齐）。

### 5. HTTP 与 SSRF

`ssrf::validate_url` 校验协议、拒绝 URL 内嵌凭据与 localhost/.local，解析 DNS 后对**全部**地址校验，连接固定到首个合法 IP（防 rebinding），重定向每一跳重新校验并重建 client。网段表见 `ssrf.rs`，已有单测覆盖。

### 6. 凭据与 SSH

- 密码/口令/token/私钥正文默认仅在内存；勾选「记住连接」后写入系统钥匙串（服务名 `io.github.zher52.logview`，与包标识一致），设置文件永远不含凭据，`SourceParams::sanitized()` 用于最近打开。
- 旧版明文凭据在加载时迁移，迁移失败也要清除明文。
- SSH 连接握手后比对 `~/.ssh/known_hosts`：**密钥不一致直接拒绝**；未知主机弹出指纹确认（TOFU），可选仅本次信任或写入 `known_hosts`。

### 7. UI 与主题

- 全部自绘：`theme.rs` 提供 antd v5 语义令牌（暗/亮两套 `Palette`）、自定义主题色、字号（9–24pt）与行高。
- 字体从系统读取（不打包字体），中文按系统字体栈回退；`mono_family()` 供日志区。
- 新增界面元素一律用 `theme::...` 访问器与缩放字号，不要硬编码颜色/字号。
- 打开入口：CLI argv、拖入窗口（`ExternalPaths`）、macOS Finder「打开方式」、命令对话框（SSH 对话框内「文件 | 命令」分页 / 欢迎页「命令」）。

### 8. 代码视图

`code.rs` 提供按扩展名的语言判定与逐行高亮；`MAX_CODE_BYTES = 2 MiB`（超出按日志打开）、`MAX_PREVIEW_BYTES = 512 KiB`（Markdown 预览上限）；Markdown 的本地图片按文档目录解析，缺失时显示提示。

## 硬性安全约束（不得弱化）

1. HTTP 源只允许 `http/https`，保持 DNS 全地址校验 + 固定 IP + 逐跳重定向复检。
2. 凭据只进钥匙串或内存，任何情况下不得明文落盘；新增字段先过 `sanitized()`。
3. 命令源不设命令白名单：预设由用户维护，UI 不得自动执行未展示给用户的命令；SSH 命令保持单引号转义（`raw` 模式除外，需明示风险）。
4. SSH 主机密钥不一致必须拒绝连接。
5. 修改 `ssrf.rs` / `source.rs` 的请求或执行路径时，在 PR 中说明安全影响。

## 代码约定

- 错误：面向用户的消息用可读 `String`（中文为源文案，带插值时用 `i18n::tf`），可操作；worker 通过 `Msg::Error` 上报。
- 注释：模块顶部 `//!` 一句话职责；非直观逻辑写中文行内注释。
- 命名：类型/文件与职责对应；UI 以中文为源文案，经 `i18n` 词典切换英文，协议字段英文。
- 依赖：新增依赖需说明理由；UI 层锁定 `gpui-kit 0.11`，升级需同步适配 API。
- 许可：新增源文件头部加
  ```rust
  // SPDX-License-Identifier: AGPL-3.0-only
  // Copyright (C) 2026 zher52
  ```
- 提交前：`cargo fmt --all`、`cargo build`、`cargo test --locked`。
- i18n：文案用语义 ID（如 `i18n::t("common.cancel")`），译文只改 `locales/{zh,en,ru}.toml`，`build.rs` 校验三语键一致后才生成查表；带插值用 `i18n::tf("id", args)`，计数用 `i18n::tn(n, "status.rows")`（英文 one、俄语 one/few），运行时消息与历史数据（预设名、配置名）用 `i18n::td(&text)` 按中文原文反查。新增文案必须同时补三份 TOML，`cargo test` 会校验源码里的 ID 是否存在。

## 已知限制（勿当 bug 修）

- HTTP 服务不支持 `Range` 时只能整页查看，无法自动追加（发送 `NoFollow`）。
- 内网/环回 HTTP 地址被 SSRF 策略拒绝，属有意行为。
- 命令源历史受 spool 上限（128MB）约束；超出后只保留最近一半，更早的内容请用命令参数获取（`journalctl -n`、`docker logs --tail`）。
- SSH 未知主机采用 TOFU 确认（仅在无法读取 `known_hosts` 时静默放行，见 `SECURITY.md`）。
- Windows / Linux 仅做 CI 构建与打包检查，交互未充分验证；Linux 的文件关联依赖 `assets/linux/logview.desktop` 与 `install.sh`（装到 `$PREFIX`），AppImage 由 `AppRun` 启动。
- 界面支持中文 / English / Русский 切换（设置 → 界面语言）；工具栏、对话框、设置页、状态栏、命令预设与常见错误提示均已覆盖，未收录词条依次回退英文、中文。

## Git 约定

- 提交信息中文：`<类型>: <一句话摘要>` + 空行 + 要点；类型 `feat/fix/refactor/docs/chore/perf/test`。
- `Cargo.lock` 入库；`target/`、`.DS_Store`、`.mimosa/`、`dev.sh`、`package.sh` 已忽略。
- 版本号唯一来源是 `Cargo.toml`；构建提交由 `build.rs` 用 `git rev-parse --short=8 HEAD` 注入 `GIT_HASH`（无 `.git` 时为 `unknown`），「关于」页展示并提供复制；`README` 与 `CHANGELOG` 需随功能同步。
---

## 调试约定（gpui-devtools）

本项目可选集成 gpui-devtools 0.6，仅在带 `devtools` feature 的开发构建中可用。

### 启用方式

- 构建：`cargo run --features devtools`
- 快捷键：macOS `cmd-alt-i`，其他平台 `ctrl-alt-i`
- release 构建不包含检查器，这是预期行为，不要在 release 构建中尝试打开检查器。

### 使用原则

1. **先定位，后修改。** 遇到 UI 布局或样式问题时，先用元素选择器选中目标元素，查看其源码位置、元素 ID、bounds、margin/border/padding，再决定改哪段代码。
2. **以源码位置为准。** 检查器给出的源码位置比肉眼推断更可靠；如果检查器显示的位置与预期不符，以检查器为准。
3. **键盘问题看 Keyboard 标签。** 如果涉及快捷键或 focus 行为，使用检查器的 Keyboard 标签查看 key context 栈、可用 action 及其绑定、最近击键触发的 action。GPUI 不暴露“哪个元素持有 focus”，面板只告诉你“有东西被 focus”，不要依赖它做精确断言。
4. **检查器不替代测试。** 检查器用于人工诊断和定位问题；自动化验证仍然通过 `cargo test` 或应用内的测试基础设施完成，不要试图让 AI 通过检查器“驱动” UI 来做回归测试。
5. **不要为使用检查器而引入额外依赖。** 如果某个问题在没有检查器的情况下也能通过日志和测试定位，优先用现有手段。

### 失败处理

- 如果 `cargo run --features devtools` 编译失败，先检查 `cargo tree -i gpui-pre` 是否唯一，以及 Rust 版本是否 >= 1.98。
- 如果检查器面板不出现，确认是否传入了 `--features devtools`，并确认初始化代码确实位于 `app.run` 闭包内且未被条件编译排除。
- 不要在 release 构建中调试检查器；release 不包含它是设计目标，不是 bug。
