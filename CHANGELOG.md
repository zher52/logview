# 更新日志

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循语义化版本。

## [Unreleased]

### Added

- Linux 打包与桌面集成:新增 `scripts/make_linux.sh`,产出便携包(可执行文件 + `.desktop` + 256×256 图标 + `install.sh`)与可选 AppImage;`.desktop` 声明 `Exec=logview %f` 与常见日志 MIME 类型,安装后文件管理器「打开方式」直接可用;Release 新增 Linux 打包任务,同时上传 tar.gz 与 AppImage。
- 新增 `tests/packaging.rs`,对 `.desktop` 关键字段、图标尺寸与打包/安装脚本做静态校验,避免打包脚本与资源脱节。
- 日志行支持行内 ANSI SGR 颜色：基础 16 色、256 色与 24 位真彩色，以及粗体、暗淡、斜体、下划线、删除线和反显；控制序列在入库时剥离，检索、复制与链接识别按纯文本工作。
- 设置 → 外观与显示新增「无级别跟随上一条已知级别」(默认开启):无级别续行(异常堆栈等多行输出)沿用上一条有级别行的级别参与着色、计数与筛选。
- 设置 → 外观与显示新增「多行记录默认折叠」与「多行记录折叠行数」(默认 3):两项均依赖上一项;折叠开关控制无级别续行的初始状态(开启=默认收起,关闭=默认展开)。超过保留行数时只显示前 N 行并在末行追加「…」,折叠图标固定在该行行首,点图标切换展开/收起,点日志行仍走原有选中事件。

### Fixed

- 发布任务的 `needs` 补齐为 `[macos, binary, linux]`,并仅在 macOS 产物成功时发布:此前 `binary` 实际未加入依赖,Windows/Linux 产物仍可能在发布之后才上传而被漏掉。
- 发布任务此前只等 macOS 打包结束就开始收集产物，Windows 包常因上传较晚被漏掉：改为同时等待 `binary` 矩阵结束（实验性任务失败仍不阻塞发布）。
- Windows 包的 zip 内曾带 `target/release/` 多层目录，改为把 `logview.exe` 放在压缩包根目录。

## [0.4.1] - 2026-11-27

### Added

- macOS 产物拆成两个架构包：`logview-macos-arm64.zip`（Apple Silicon）与 `logview-macos-x86_64.zip`（Intel）；`make_app.sh` 支持 `TARGET=...` 交叉构建，产物路径不变。

### Fixed

- macOS 产物此前动态链接 Homebrew 的 `openssl@4`（`/usr/local/opt/openssl@4/lib/*.dylib`），没有装该库的用户机器上无法启动；改为 vendored OpenSSL 静态链接，同时解决 x86_64 交叉构建找不到对应架构 OpenSSL 的失败。
- Windows 构建失败：libssh2 默认 WinCNG 后端没有 `userauth_pubkey_memory`（私钥文本认证），Windows 目标改用 OpenSSL 后端（`openssl-on-win32` + vendored）后恢复编译，Release 重新提供 `logview-windows.zip`。
- 依赖审计工作流在 push 事件下报 `Resource not accessible by integration`：`audit.yml` 补 `checks: write`（`cargo audit` 本身一直是 0 漏洞）。

### Changed

- 依赖升级（dependabot）：dirs 5→7、sha2 0.10→0.11、base64 0.22→0.23、toml 0.8→1.2（仅构建期）；GitHub Actions 升级：checkout 4→7、upload-artifact 4→7、download-artifact 4→8、gh-release 2→3。

## [0.4.0] - 2026-11-27

### Added

- 命令源输出缓存(spool):命令标签把 stdout/stderr 落到系统缓存目录的临时文件,暂停/取消跟随不再丢日志,「加载全部」「跳到末尾」与文件源语义一致;清屏同时清空缓存,关闭标签/退出删除,异常退出残留下次启动清理,上限 128MB。
- Android logcat 级别规则支持 `threadtime` / `time` / `brief` / `process` 四种格式与 `A`(assert)。

- 开源配套：AGPL-3.0 许可证、第三方素材声明、贡献与安全策略、Issue/PR 模板。
- GitHub Actions：格式 / lint / 测试，依赖安全审计，依赖许可检查（cargo-deny）与 tag 发布工作流。
- 设置「关于」页：版本号与构建提交（`build.rs` 注入 `GIT_HASH`，无 `.git` 时显示 `unknown`）、许可证、项目链接与所用开源组件清单（含完整第三方声明入口），可一键复制「版本 + 构建 + 平台」用于报障，三种界面语言均已覆盖；「项目链接」分组带 GitHub 图标入口。
- 「检查更新」：手动向 GitHub Releases 查询最新版本并按语义化版本比较；只发送版本号，不上报任何本地数据，有新版本时打开下载页，不自行下载或替换程序。
- 仓库上线：`io.github.zher52.logview` 包标识、钥匙串服务名与 README/Cargo 元数据统一指向 [github.com/zher52/logview](https://github.com/zher52/logview)。
- SSH 连接比对已有主机密钥，不一致时拒绝连接。
- SSH 未知主机 TOFU 确认：弹窗展示算法与 SHA256 指纹，支持仅本次信任或写入 `known_hosts`。
- 中英文界面：设置 → 界面语言即时切换，覆盖工具栏、对话框、设置页、状态栏、命令预设与常见错误提示；未收录词条回退中文。
- 俄语界面：新增 `ru` 语言与完整俄文语言包，设置中的语言改为「中文 / English / Русский」三态选择。
- i18n 重构为「语义 ID + 语言包」：文案键改为 `common.cancel`、`error.sftp.read` 这类 ID，译文移出 Rust 到 `locales/{zh,en,ru}.toml`，由 `build.rs` 编译期生成查表并校验三语键集合一致；新增 `i18n::tn` 按语言处理复数（俄语 one/few/other），修正 `1 строк`/`1 lines` 一类错误；`cargo test` 会扫描源码，确保引用的 ID 全部存在。
- README 增加真实界面截图与自动追加 GIF。
- 补齐 ssrf / search / links / i18n 单元测试（当前共 88 项）。

### Fixed

- 命令标签「跳到末尾」不再清空全部历史;「加载全部」不再点了没反应;暂停期间的新日志会保留在缓存,恢复后补发。
- 命令退出自动重连时保留暂停状态,不再把“暂停”悄悄变回“继续”。
- logcat 规则不再全局默认启用:只在命令预设声明或 argv 含 `logcat` 时按标签自动启用,并优先于「通用单词」,避免正文里的 error/warn 覆盖行首真实级别。
- 切换界面语言后，检索框、设置搜索框及 SSH / URL / 命令对话框的输入框占位符仍显示旧语言。
- 设置页与空白标签页的标签名改为随界面语言动态更新。
- 日志级别「无级别」改为使用语言包，英文界面显示为 `Others`。

### Changed

- 许可证采用 GNU AGPL-3.0（SPDX：`AGPL-3.0-only`）。
- macOS 应用包标识改为 `io.github.zher52.logview`（原 `local.fandd.logview`）；配置与设置仍按目录名 `logview` 存放。
- 钥匙串服务名同步改为 `io.github.zher52.logview`。
- 产品名统一为「日志查看器」（英文 `Log Viewer`、俄文 `Просмотр журналов`）；内部标识（crate、二进制、配置目录）仍为 `logview`，不影响已有设置与脚本。
- 统一 rustfmt 格式。
- 工具栏自动换行开关改回文字按钮（开启时主题色高亮，悬停提示保留开关状态与操作），图标与含义不符的问题不再出现；工具栏宽度估算随之按文字宽度计算。

## [0.3.0] - 2026-11-26

### Added

- 第四种日志源「命令」:本地直接执行,或经 SSH 远端执行,stdout/stderr 合并入标签。
- 命令预设数据模型(`command_presets`):argv 模板、字段、预解析步骤(如 `adb shell pidof`)、关联级别配置;内置 adb logcat(全量/包名/标签)、`tail -F`、`journalctl -f`、`docker logs -f`、`kubectl logs -f`,新增内置项按 id 自动追加。
- 命令源退出码提示与可选自动重试(退避),适配设备重连与服务重启;SSH 命令复用主机凭据与钥匙串条目。
- 内置 `adb logcat -v threadtime` 级别配置。
- 命令流与本地进程测试(当前共 69 项)。

### Changed

- SSH 打开对话框增加「文件 | 命令」分页,命令与目标主机在同一处填写;欢迎页新增「命令」入口。
- 命令源禁用「加载全部」(流式无回溯),「跳到末尾」改为清空缓冲后继续。

## [0.2.0] - 2026-11-26

### Added

- 主界面迁移到 GPUI（自绘 UI，无 WebView），含标签栏、工具栏、日志区与设置页。
- 代码/配置文件逐行高亮与 Markdown 渲染预览，支持 `⌘S` 保存。
- 日志行显示接收时间戳（默认关闭，设置 → 外观与显示）。
- 工具栏图标化，级别 chips 按宽度三档自适应，窄窗口不再撑爆。
- 设置页支持管理已保存 SSH 主机与 URL 凭据、调整最近打开条数并清除记录。
- HTTP 源支持 Basic / Bearer / API Key / 自定义请求头认证。
- 主题色可自定义，随亮暗主题自动切换深色变体。

### Changed

- 凭据改存操作系统钥匙串，设置文件不再包含密码或 token；旧版明文凭据启动时迁移。
- SSH 主机输入框聚焦即展开已保存服务器列表。
- 跟随 / 暂停 / 跳转到末尾的语义与 egui 版对齐。

### Fixed

- 跟随贴底在代码视图与 Markdown 预览下的行为不一致。
- 长行与多字节字符在检索高亮中的区间计算。

## [0.1.0] - 2026-11-25

### Added

- 首个版本：本地 / SSH / HTTP 三种日志源。
- 正则级别配置（内置 5 套 + 自定义导入导出）、级别着色与筛选。
- 高亮 / 过滤检索、自动追加（tail -f）、轮换与截断检测。
- 行内 URL / 路径 / JSON 识别并可点击打开。
- 拖入文件打开、多标签、最近打开、行数上限环形丢弃。
- macOS `.app` 打包与文件类型关联。