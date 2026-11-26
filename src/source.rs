// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 日志源:本地文件 / SSH(SFTP) / HTTP(Range 增量,SSRF 校验)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::command::CommandPlan;
use crate::i18n;
use crate::ssrf::{validate_url, Validated};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum SshAuth {
    #[serde(rename = "password")]
    Password(String),
    #[serde(rename = "keyText")]
    KeyText {
        key: String,
        passphrase: Option<String>,
    },
    #[serde(rename = "keyFile")]
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
    #[serde(rename = "agent")]
    Agent,
}

/// URL(HTTP)认证方式:无 / Basic / Bearer / API Key / 自定义请求头
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum HttpAuthKind {
    #[default]
    None,
    Basic,
    Bearer,
    ApiKey,
    Header,
}

/// 已校验、可直接附加到请求上的认证信息
#[derive(Clone)]
pub enum HttpAuthHeader {
    Basic {
        user: String,
        password: String,
    },
    Header {
        name: reqwest::header::HeaderName,
        value: reqwest::header::HeaderValue,
    },
}

/// 命令在何处执行(本机 / 远端 SSH)
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecTarget {
    Local,
    Ssh {
        host: String,
        port: u16,
        user: String,
        auth: SshAuth,
    },
}

impl ExecTarget {
    pub fn auth_mut(&mut self) -> Option<&mut SshAuth> {
        match self {
            Self::Local => None,
            Self::Ssh { auth, .. } => Some(auth),
        }
    }

    pub fn auth(&self) -> Option<&SshAuth> {
        match self {
            Self::Local => None,
            Self::Ssh { auth, .. } => Some(auth),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceParams {
    Local {
        path: String,
    },
    Ssh {
        host: String,
        port: u16,
        user: String,
        auth: SshAuth,
        path: String,
    },
    Http {
        url: String,
        /// 认证方式(旧数据缺省为无认证)
        #[serde(default)]
        auth: HttpAuthKind,
        /// HTTP Basic 用户名
        #[serde(default)]
        user: Option<String>,
        /// HTTP Basic 密码(默认不写入最近打开)
        #[serde(default)]
        password: Option<String>,
        /// Bearer token(默认不写入最近打开)
        #[serde(default)]
        token: Option<String>,
        /// 自定义请求头名称(API Key 默认 X-API-Key)
        #[serde(default)]
        header_name: Option<String>,
        /// 自定义请求头值(默认不写入最近打开)
        #[serde(default)]
        header_value: Option<String>,
    },
    /// 命令源:本机子进程或 SSH 远端命令,stdout 作为日志流
    Command {
        target: ExecTarget,
        /// 已展开的 argv(展示 + 无预设时的执行依据);SSH 自定义命令时只有一项
        #[serde(default)]
        argv: Vec<String>,
        /// 预设 id(重新打开时按预设 + values 重新展开,避免沿用旧 PID)
        #[serde(default)]
        preset: Option<String>,
        /// 预设字段值
        #[serde(default)]
        values: BTreeMap<String, String>,
        /// 退出后自动重试(SSH 断线、adb 设备重连等)
        #[serde(default)]
        restart: bool,
        /// 自定义命令原文(SSH 时不加引号,允许管道/重定向)
        #[serde(default)]
        raw: bool,
    },
}

impl SourceParams {
    pub fn label(&self) -> String {
        match self {
            Self::Local { path } => std::path::Path::new(path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone()),
            Self::Ssh {
                host, user, path, ..
            } => {
                let f = std::path::Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                format!("{f} · {user}@{host}")
            }
            Self::Http { url, .. } => url
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(url)
                .to_string(),
            Self::Command { target, argv, .. } => {
                let cmd = crate::command::display(argv);
                let short = shorten(&cmd, 48);
                match target {
                    ExecTarget::Local => short,
                    ExecTarget::Ssh { host, user, .. } => format!("{short} · {user}@{host}"),
                }
            }
        }
    }

    /// 状态栏展示的位置信息:本地 = 路径,SSH = 地址:路径,HTTP = URL
    /// 非默认端口一并带上(`user@host:2222:/path`),默认 22 端口省略以免误读
    pub fn location(&self) -> String {
        match self {
            Self::Local { path } => path.clone(),
            Self::Ssh {
                host,
                port,
                user,
                path,
                ..
            } => {
                if *port == 22 {
                    format!("{user}@{host}:{path}")
                } else {
                    format!("{user}@{host}:{port}:{path}")
                }
            }
            Self::Http { url, .. } => url.clone(),
            Self::Command { target, argv, .. } => {
                let cmd = crate::command::display(argv);
                match target {
                    ExecTarget::Local => cmd,
                    ExecTarget::Ssh {
                        host, port, user, ..
                    } => {
                        if *port == 22 {
                            format!("{user}@{host}: {cmd}")
                        } else {
                            format!("{user}@{host}:{port}: {cmd}")
                        }
                    }
                }
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Local { path } => i18n::tf("本地 {path}", &[("path", path.clone())]),
            Self::Ssh {
                host,
                port,
                user,
                path,
                ..
            } => format!("SSH {user}@{host}:{port} {path}"),
            Self::Http { url, user, .. } => match user {
                Some(u) if !u.is_empty() => format!("HTTP {u}@{url}"),
                _ => format!("HTTP {url}"),
            },
            Self::Command { target, argv, .. } => {
                let cmd = crate::command::display(argv);
                match target {
                    ExecTarget::Local => i18n::tf("命令(本机) {cmd}", &[("cmd", cmd.clone())]),
                    ExecTarget::Ssh {
                        host, port, user, ..
                    } => i18n::tf(
                        "命令 {user}@{host}:{port} {cmd}",
                        &[
                            ("user", user.clone()),
                            ("host", host.clone()),
                            ("port", port.to_string()),
                            ("cmd", cmd.clone()),
                        ],
                    ),
                }
            }
        }
    }

    /// 去除秘密(密码/令牌/口令/私钥正文),用于"最近打开"的持久化。
    /// 私钥文件路径、HTTP 用户名与请求头名称不是秘密,保留以免重开后失效。
    pub fn sanitized(&self) -> Self {
        let mut c = self.clone();
        match &mut c {
            Self::Ssh { auth, .. } => match auth {
                SshAuth::Password(p) => *p = String::new(),
                SshAuth::KeyText { key, passphrase } => {
                    *key = String::new();
                    *passphrase = None;
                }
                SshAuth::KeyFile { passphrase, .. } => *passphrase = None,
                SshAuth::Agent => {}
            },
            Self::Http {
                password,
                token,
                header_value,
                ..
            } => {
                *password = None;
                *token = None;
                *header_value = None;
            }
            Self::Command { target, .. } => {
                if let Some(auth) = target.auth_mut() {
                    scrub_auth(auth);
                }
            }
            Self::Local { .. } => {}
        }
        c
    }

    /// 是否含有真实凭据材料(用于判断是否需要写入钥匙串)
    pub fn has_secret(&self) -> bool {
        match self {
            Self::Ssh { auth, .. } => match auth {
                SshAuth::Password(p) => !p.is_empty(),
                SshAuth::KeyText { key, passphrase } => {
                    !key.is_empty() || passphrase.as_ref().is_some_and(|s| !s.is_empty())
                }
                SshAuth::KeyFile { passphrase, .. } => {
                    passphrase.as_ref().is_some_and(|s| !s.is_empty())
                }
                SshAuth::Agent => false,
            },
            Self::Http {
                password,
                token,
                header_value,
                ..
            } => {
                password.as_ref().is_some_and(|p| !p.is_empty())
                    || token.as_ref().is_some_and(|t| !t.is_empty())
                    || header_value.as_ref().is_some_and(|v| !v.is_empty())
            }
            Self::Command { target, .. } => target.auth().is_some_and(auth_has_secret),
            Self::Local { .. } => false,
        }
    }

    /// 按当前认证方式是否缺少必需凭据(仅覆盖界面可输入的组合,决定是否回填钥匙串/弹表单)
    pub fn needs_secret(&self) -> bool {
        match self {
            Self::Ssh {
                auth: SshAuth::Password(p),
                ..
            } => p.is_empty(),
            Self::Ssh { .. } => false,
            Self::Http {
                auth,
                password,
                token,
                header_value,
                ..
            } => match auth {
                HttpAuthKind::None => false,
                HttpAuthKind::Basic => !password.as_ref().is_some_and(|p| !p.is_empty()),
                HttpAuthKind::Bearer => !token.as_ref().is_some_and(|t| !t.is_empty()),
                HttpAuthKind::ApiKey | HttpAuthKind::Header => {
                    !header_value.as_ref().is_some_and(|v| !v.is_empty())
                }
            },
            Self::Command {
                target:
                    ExecTarget::Ssh {
                        auth: SshAuth::Password(p),
                        ..
                    },
                ..
            } => p.is_empty(),
            Self::Command { .. } | Self::Local { .. } => false,
        }
    }
}

pub struct SourceMeta {
    pub size: u64,
    pub supports_range: bool,
}

pub enum Opened {
    Local {
        path: PathBuf,
    },
    Ssh {
        sess: ssh2::Session,
        path: String,
    },
    Http {
        v: Validated,
        supports_range: bool,
        total: Option<u64>,
        auth: Option<HttpAuthHeader>,
    },
    Stream(StreamHandle),
}

/// 命令输出流:单向、不可 seek
pub struct StreamHandle {
    /// stdout(本机已把 stderr 并入同一通道)
    pub reader: Box<dyn Read + Send>,
    /// 本机子进程;停止时 kill。SSH 命令为 None(靠 stop 标志关通道)
    pub child: Option<std::process::Child>,
    /// 停止标志:读端据此尽快退出
    pub stop: Arc<AtomicBool>,
    /// SSH 通道退出码(读端读到 EOF 后写入)
    pub exit: Arc<Mutex<Option<i32>>>,
}

pub fn open(p: &SourceParams, ask: HostKeyAsk) -> Result<Opened, String> {
    match p {
        SourceParams::Local { path } => {
            if !std::path::Path::new(path).is_file() {
                return Err(i18n::tf(
                    "文件不存在或不是普通文件: {path}",
                    &[("path", path.to_string())],
                ));
            }
            Ok(Opened::Local {
                path: PathBuf::from(path),
            })
        }
        SourceParams::Ssh {
            host,
            port,
            user,
            auth,
            path,
        } => {
            let sess = ssh_connect(host, *port, user, auth, ask)?;
            Ok(Opened::Ssh {
                sess,
                path: path.clone(),
            })
        }
        SourceParams::Command { .. } => Err("命令源需通过 open_command 打开".into()),
        SourceParams::Http { url, .. } => {
            let mut v = validate_url(url)?;
            let auth = build_http_auth(p)?;
            let mut client = http_client(&v)?;
            let resp = http_get(&mut client, &mut v, Some("bytes=0-0"), auth.as_ref())?;
            let status = resp.status();
            if status == reqwest::StatusCode::PARTIAL_CONTENT {
                let total =
                    parse_content_range_total(resp.headers().get(reqwest::header::CONTENT_RANGE));
                return Ok(Opened::Http {
                    v,
                    supports_range: true,
                    total,
                    auth,
                });
            }
            if status.is_success() {
                let total = resp.content_length();
                return Ok(Opened::Http {
                    v,
                    supports_range: false,
                    total,
                    auth,
                });
            }
            // 401/403 标记为认证失败,由界面弹出连接表单让用户重填凭据
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(i18n::tf(
                    "HTTP {status}(认证失败)",
                    &[("status", status.to_string())],
                ));
            }
            Err(i18n::tf("HTTP {status}", &[("status", status.to_string())]))
        }
    }
}

pub fn meta(o: &mut Opened) -> Result<SourceMeta, String> {
    match o {
        Opened::Local { path } => {
            let m = std::fs::metadata(path)
                .map_err(|e| i18n::tf("stat 失败: {e}", &[("e", e.to_string())]))?;
            Ok(SourceMeta {
                size: m.len(),
                supports_range: true,
            })
        }
        Opened::Ssh { sess, path, .. } => {
            let sftp = sess
                .sftp()
                .map_err(|e| i18n::tf("SFTP 通道失败: {e}", &[("e", e.to_string())]))?;
            let st = sftp
                .stat(std::path::Path::new(path))
                .map_err(|e| i18n::tf("SFTP stat 失败: {e}", &[("e", e.to_string())]))?;
            Ok(SourceMeta {
                size: st.size.unwrap_or(0),
                supports_range: true,
            })
        }
        Opened::Http {
            total,
            supports_range,
            ..
        } => Ok(SourceMeta {
            size: total.unwrap_or(0),
            supports_range: *supports_range,
        }),
        Opened::Stream(_) => Err("命令流不支持大小/随机读取".into()),
    }
}

/// 读取 [off, off+max) 字节,返回 (数据, 文件总大小)
pub fn read_at(o: &mut Opened, off: u64, max: usize) -> Result<(Vec<u8>, u64), String> {
    match o {
        Opened::Local { path } => {
            let total = std::fs::metadata(path.as_path())
                .map_err(|e| i18n::tf("stat 失败: {e}", &[("e", e.to_string())]))?
                .len();
            if off >= total {
                return Ok((Vec::new(), total));
            }
            let mut f = std::fs::File::open(path.as_path())
                .map_err(|e| i18n::tf("打开失败: {e}", &[("e", e.to_string())]))?;
            f.seek(SeekFrom::Start(off))
                .map_err(|e| i18n::tf("seek 失败: {e}", &[("e", e.to_string())]))?;
            let want = ((total - off) as usize).min(max);
            let mut buf = vec![0u8; want];
            let mut got = 0;
            while got < want {
                let n = f
                    .read(&mut buf[got..])
                    .map_err(|e| i18n::tf("读取失败: {e}", &[("e", e.to_string())]))?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            buf.truncate(got);
            Ok((buf, total))
        }
        Opened::Ssh { sess, path, .. } => {
            let sftp = sess
                .sftp()
                .map_err(|e| i18n::tf("SFTP 通道失败: {e}", &[("e", e.to_string())]))?;
            let total = sftp
                .stat(std::path::Path::new(path))
                .map_err(|e| i18n::tf("SFTP stat 失败: {e}", &[("e", e.to_string())]))?
                .size
                .unwrap_or(0);
            if off >= total {
                return Ok((Vec::new(), total));
            }
            let mut f = sftp
                .open(std::path::Path::new(path))
                .map_err(|e| i18n::tf("SFTP open 失败: {e}", &[("e", e.to_string())]))?;
            f.seek(SeekFrom::Start(off))
                .map_err(|e| i18n::tf("SFTP seek 失败: {e}", &[("e", e.to_string())]))?;
            let want = ((total - off) as usize).min(max);
            let mut buf = vec![0u8; want];
            let mut got = 0;
            while got < want {
                match f.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(i18n::tf("SFTP 读取失败: {e}", &[("e", e.to_string())])),
                }
            }
            buf.truncate(got);
            Ok((buf, total))
        }
        Opened::Http {
            v,
            supports_range,
            auth,
            ..
        } => {
            let mut client = http_client(v)?;
            let range = if *supports_range {
                Some(format!("bytes={off}-"))
            } else {
                None
            };
            let resp = http_get(&mut client, v, range.as_deref(), auth.as_ref())?;
            let status = resp.status();
            if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err(format!("HTTP {status}"));
            }
            let total = total_from_headers(&resp, v).unwrap_or(0);
            if *supports_range && status.is_success() && off > 0 {
                return Err("服务器忽略 Range 请求,无法增量读取".into());
            }
            let mut buf = Vec::new();
            use std::io::Read;
            let mut limited = resp.take(max as u64);
            limited
                .read_to_end(&mut buf)
                .map_err(|e| i18n::tf("HTTP 读取失败: {e}", &[("e", e.to_string())]))?;
            drop(limited);
            Ok((buf, total))
        }
        Opened::Stream(_) => Err("命令流不支持随机读取".into()),
    }
}

// ---------- SSH ----------

/// 比对 `~/.ssh/known_hosts` 中已有的主机密钥。
///
/// 待确认的未知主机密钥(TOFU)
#[derive(Clone, Debug)]
pub struct HostKeyInfo {
    pub host: String,
    pub port: u16,
    /// 密钥算法,如 ssh-ed25519
    pub algo: String,
    /// OpenSSH 风格指纹:SHA256:...
    pub fingerprint: String,
}

/// 未知主机的处理结果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// 接受;`remember` 为 true 时写入 known_hosts
    Accept {
        remember: bool,
    },
    Reject,
}

/// 未知主机询问回调:在 worker 线程中等待 UI 决策(期间仍可响应 Stop)
pub type HostKeyAsk<'a> = &'a mut dyn FnMut(&HostKeyInfo) -> HostKeyDecision;

fn host_key_algo(key: &[u8]) -> String {
    (|| {
        let n = u32::from_be_bytes(key.get(..4)?.try_into().ok()?) as usize;
        std::str::from_utf8(key.get(4..4 + n)?)
            .ok()
            .map(str::to_owned)
    })()
    .unwrap_or_else(|| "未知算法".into())
}

fn host_key_fingerprint(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

fn known_hosts_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

/// 把当前会话的主机密钥追加到 known_hosts(保留已有条目)
fn trust_host(sess: &ssh2::Session, host: &str, port: u16) -> Result<(), String> {
    let path = known_hosts_path().ok_or("找不到用户目录,无法写入 known_hosts")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建 ~/.ssh 失败:{e}"))?;
    }
    let mut known_hosts = sess
        .known_hosts()
        .map_err(|e| format!("known_hosts 初始化失败:{e}"))?;
    if path.is_file() {
        known_hosts
            .read_file(&path, ssh2::KnownHostFileKind::OpenSSH)
            .map_err(|e| format!("读取 known_hosts 失败:{e}"))?;
    }
    let (key, kind) = sess.host_key().ok_or("无法获取服务器主机密钥")?;
    // 非默认端口需写成 [host]:port,与 OpenSSH 一致
    let name = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    known_hosts
        .add(
            &name,
            key,
            "added by logview",
            ssh2::KnownHostKeyFormat::from(kind),
        )
        .map_err(|e| format!("写入 known_hosts 失败:{e}"))?;
    known_hosts
        .write_file(&path, ssh2::KnownHostFileKind::OpenSSH)
        .map_err(|e| format!("保存 known_hosts 失败:{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 已知主机密钥不一致时拒绝;未知主机交由 `ask` 决定(TOFU)
fn verify_known_host(
    sess: &ssh2::Session,
    host: &str,
    port: u16,
    ask: HostKeyAsk,
) -> Result<(), String> {
    let Some((key, _kind)) = sess.host_key() else {
        return Ok(());
    };
    let mut known_hosts = match sess.known_hosts() {
        Ok(kh) => kh,
        Err(_) => return Ok(()),
    };
    if let Some(path) = known_hosts_path() {
        if path.is_file()
            && known_hosts
                .read_file(&path, ssh2::KnownHostFileKind::OpenSSH)
                .is_err()
        {
            return Err(format!(
                "SSH 主机密钥校验失败({host}:{port}):known_hosts 无法读取"
            ));
        }
    }
    match known_hosts.check_port(host, port, key) {
        ssh2::CheckResult::Match => Ok(()),
        ssh2::CheckResult::Mismatch => Err(format!(
            "SSH 主机密钥与 known_hosts 记录不一致({host}:{port}),可能存在中间人攻击;\
             确认服务器换过密钥后请先更新 known_hosts"
        )),
        ssh2::CheckResult::Failure => Err(format!(
            "SSH 主机密钥校验失败({host}:{port}):known_hosts 无法读取"
        )),
        ssh2::CheckResult::NotFound => {
            let info = HostKeyInfo {
                host: host.to_string(),
                port,
                algo: host_key_algo(key),
                fingerprint: host_key_fingerprint(key),
            };
            match ask(&info) {
                HostKeyDecision::Accept { remember } => {
                    if remember {
                        trust_host(sess, host, port)?;
                    }
                    Ok(())
                }
                HostKeyDecision::Reject => Err(i18n::tf(
                    "已取消连接:{host}:{port} 的主机密钥未确认",
                    &[("host", host.to_string()), ("port", port.to_string())],
                )),
            }
        }
    }
}

pub fn ssh_connect(
    host: &str,
    port: u16,
    user: &str,
    auth: &SshAuth,
    ask: HostKeyAsk,
) -> Result<ssh2::Session, String> {
    ssh_connect_t(host, port, user, auth, 20_000, ask)
}

/// 连接并认证;`timeout_ms` 作用于阻塞调用(0 = 不限时)
fn ssh_connect_t(
    host: &str,
    port: u16,
    user: &str,
    auth: &SshAuth,
    timeout_ms: u32,
    ask: HostKeyAsk,
) -> Result<ssh2::Session, String> {
    let tcp = std::net::TcpStream::connect((host, port)).map_err(|e| {
        i18n::tf(
            "SSH 连接失败 {host}:{port}: {e}",
            &[
                ("host", host.to_string()),
                ("port", port.to_string()),
                ("e", e.to_string()),
            ],
        )
    })?;
    let mut sess = ssh2::Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.set_timeout(timeout_ms);
    sess.handshake()
        .map_err(|e| i18n::tf("SSH 握手失败: {e}", &[("e", e.to_string())]))?;
    verify_known_host(&sess, host, port, ask)?;
    let ok = match auth {
        SshAuth::Password(p) => sess.userauth_password(user, p).is_ok(),
        SshAuth::KeyText { key, passphrase } => sess
            .userauth_pubkey_memory(user, None, key, passphrase.as_deref())
            .is_ok(),
        SshAuth::KeyFile { path, passphrase } => sess
            .userauth_pubkey_file(
                user,
                None,
                std::path::Path::new(path),
                passphrase.as_deref(),
            )
            .is_ok(),
        SshAuth::Agent => sess.userauth_agent(user).is_ok(),
    };
    if !ok || !sess.authenticated() {
        return Err("SSH 认证失败(检查用户名/密码/密钥/agent)".into());
    }
    Ok(sess)
}

// ---------- HTTP ----------

pub fn http_client(v: &Validated) -> Result<reqwest::blocking::Client, String> {
    let host = v.url.host_str().ok_or("URL 缺少主机名")?.to_owned();
    reqwest::blocking::Client::builder()
        .resolve(host.as_str(), v.addr)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("LogView/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| i18n::tf("HTTP client 构建失败: {e}", &[("e", e.to_string())]))
}

/// 根据源参数构造认证头(只保留用户显式填写的方式)
fn build_http_auth(p: &SourceParams) -> Result<Option<HttpAuthHeader>, String> {
    let SourceParams::Http {
        auth,
        user,
        password,
        token,
        header_name,
        header_value,
        ..
    } = p
    else {
        return Ok(None);
    };
    match auth {
        HttpAuthKind::None => Ok(None),
        HttpAuthKind::Basic => {
            let Some(user) = user.as_ref().filter(|u| !u.is_empty()) else {
                return Ok(None);
            };
            Ok(Some(HttpAuthHeader::Basic {
                user: user.clone(),
                password: password.clone().unwrap_or_default(),
            }))
        }
        HttpAuthKind::Bearer => {
            let Some(token) = token.as_ref().filter(|t| !t.is_empty()) else {
                return Ok(None);
            };
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| i18n::tf("Bearer token 无效: {e}", &[("e", e.to_string())]))?;
            Ok(Some(HttpAuthHeader::Header {
                name: reqwest::header::AUTHORIZATION,
                value,
            }))
        }
        HttpAuthKind::ApiKey | HttpAuthKind::Header => {
            let name = header_name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or("X-API-Key");
            let Some(value) = header_value.as_deref().filter(|v| !v.is_empty()) else {
                return Ok(None);
            };
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "host" | "content-length" | "transfer-encoding" | "connection"
            ) {
                return Err(format!("不允许设置请求头: {name}"));
            }
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| i18n::tf("请求头名称无效: {e}", &[("e", e.to_string())]))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|e| i18n::tf("请求头值无效: {e}", &[("e", e.to_string())]))?;
            Ok(Some(HttpAuthHeader::Header { name, value }))
        }
    }
}

/// 带手动重定向的 GET(每一跳重新走 SSRF 校验并重建固定 IP 的 client)
fn http_get(
    client: &mut reqwest::blocking::Client,
    v: &mut Validated,
    range: Option<&str>,
    auth: Option<&HttpAuthHeader>,
) -> Result<reqwest::blocking::Response, String> {
    let mut cur = v.url.clone();
    // 认证只发给初始主机,避免跟随重定向时泄露到其他域名
    let auth_host = auth.and_then(|_| v.url.host_str().map(|h| h.to_owned()));
    for _ in 0..6 {
        let mut req = client
            .get(cur.clone())
            .header(reqwest::header::ACCEPT, "*/*");
        if let Some(r) = range {
            req = req.header(reqwest::header::RANGE, r);
        }
        if let (Some(auth), Some(host)) = (auth, &auth_host) {
            if cur.host_str() == Some(host.as_str()) {
                req = match auth {
                    HttpAuthHeader::Basic { user, password } => {
                        req.basic_auth(user, Some(password))
                    }
                    HttpAuthHeader::Header { name, value } => {
                        req.header(name.clone(), value.clone())
                    }
                };
            }
        }
        let resp = req
            .send()
            .map_err(|e| i18n::tf("HTTP 请求失败: {e}", &[("e", e.to_string())]))?;
        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .ok_or("重定向缺少 Location")?
                .to_owned();
            cur = v
                .url
                .join(&loc)
                .map_err(|_| "重定向 Location 无效".to_string())?;
            *v = validate_url(cur.as_str())?;
            *client = http_client(v)?;
            continue;
        }
        return Ok(resp);
    }
    Err("重定向次数过多".into())
}

fn total_from_headers(resp: &reqwest::blocking::Response, v: &Validated) -> Option<u64> {
    if let Some(cr) = resp.headers().get(reqwest::header::CONTENT_RANGE) {
        if let Ok(s) = cr.to_str() {
            if let Some(total) = s.rsplit('/').next() {
                if let Ok(n) = total.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(s) = cl.to_str() {
            if let Ok(n) = s.parse::<u64>() {
                return Some(n);
            }
        }
    }
    let _ = v;
    None
}

fn parse_content_range_total(h: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    let s = h?.to_str().ok()?;
    s.rsplit('/').next()?.parse::<u64>().ok()
}

// ---------- 命令源 ----------

/// 命令读端:非阻塞轮询,便于停止标志及时生效
struct SshReader {
    ch: ssh2::Channel,
    stop: Arc<AtomicBool>,
    exit: Arc<Mutex<Option<i32>>>,
}

pub(crate) fn is_retryable(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

impl Read for SshReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return Ok(0);
        }
        match self.ch.read(buf) {
            Ok(0) => {
                // 读完后补取退出码(非阻塞模式下可能还要等一拍)
                for _ in 0..20 {
                    if let Ok(code) = self.ch.exit_status() {
                        if let Ok(mut slot) = self.exit.lock() {
                            *slot = Some(code);
                        }
                        break;
                    }
                    let _ = self.ch.wait_close();
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Ok(0)
            }
            Ok(n) => Ok(n),
            Err(e) if is_retryable(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
            }
            Err(e) => Err(e),
        }
    }
}

/// 本机 stdout/stderr 合并读端(两个转发线程写入同一通道)
struct MergedReader {
    rx: crossbeam_channel::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for MergedReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.pos < self.buf.len() {
                let n = (self.buf.len() - self.pos).min(out.len());
                out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            match self.rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
    }
}

fn spawn_pipe_reader(mut r: impl Read + Send + 'static, tx: crossbeam_channel::Sender<Vec<u8>>) {
    let _ = std::thread::Builder::new()
        .name("cmd-out".into())
        .spawn(move || {
            let mut buf = vec![0u8; 32 << 10];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
}

fn spawn_error(prog: &str, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => i18n::tf(
            "未找到命令 {prog}(请确认已安装,并在 PATH 中)",
            &[("prog", prog.to_string())],
        ),
        std::io::ErrorKind::PermissionDenied => {
            i18n::tf("没有执行权限:{prog}", &[("prog", prog.to_string())])
        }
        _ => i18n::tf(
            "无法启动命令 {prog}:{e}",
            &[("prog", prog.to_string()), ("e", e.to_string())],
        ),
    }
}

fn spawn_local(argv: Vec<String>) -> Result<Opened, String> {
    let prog = argv
        .first()
        .filter(|s| !s.trim().is_empty())
        .ok_or("命令为空")?;
    let mut cmd = std::process::Command::new(prog);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW:GUI 进程拉起控制台程序时不闪黑框
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| spawn_error(prog, &e))?;
    let (tx, rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    if let Some(out) = child.stdout.take() {
        spawn_pipe_reader(out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_pipe_reader(err, tx.clone());
    }
    drop(tx);
    Ok(Opened::Stream(StreamHandle {
        reader: Box::new(MergedReader {
            rx,
            buf: Vec::new(),
            pos: 0,
        }),
        child: Some(child),
        stop: Arc::new(AtomicBool::new(false)),
        exit: Arc::new(Mutex::new(None)),
    }))
}

/// 执行一条短命令并返回 stdout(用于 adb pidof 之类的取值)
fn capture_local(argv: &[String]) -> Result<String, String> {
    let prog = argv
        .first()
        .filter(|s| !s.trim().is_empty())
        .ok_or(i18n::t("取值命令为空").to_string())?;
    let mut cmd = std::process::Command::new(prog);
    cmd.args(&argv[1..]).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| spawn_error(prog, &e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(if err.is_empty() {
            i18n::tf(
                "取值命令失败(code={code})",
                &[("code", out.status.code().unwrap_or(-1).to_string())],
            )
        } else {
            i18n::tf("取值命令失败:{err}", &[("err", err.to_string())])
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn capture_ssh(target: &ExecTarget, argv: &[String], ask: HostKeyAsk) -> Result<String, String> {
    let ExecTarget::Ssh {
        host,
        port,
        user,
        auth,
    } = target
    else {
        return Err("不是 SSH 目标".into());
    };
    let sess = ssh_connect_t(host, *port, user, auth, 15_000, ask)?;
    let remote = argv
        .iter()
        .map(|s| crate::command::shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    let mut ch = sess
        .channel_session()
        .map_err(|e| i18n::tf("SSH 通道创建失败:{e}", &[("e", e.to_string())]))?;
    ch.handle_extended_data(ssh2::ExtendedData::Merge)
        .map_err(|e| i18n::tf("SSH stderr 合并失败:{e}", &[("e", e.to_string())]))?;
    ch.exec(&remote)
        .map_err(|e| i18n::tf("远端取值命令执行失败:{e}", &[("e", e.to_string())]))?;
    let mut buf = String::new();
    ch.read_to_string(&mut buf)
        .map_err(|e| i18n::tf("远端取值命令读取失败:{e}", &[("e", e.to_string())]))?;
    let _ = ch.wait_close();
    Ok(buf)
}

/// 命令流会话:不限时 + 非阻塞读 + keepalive(空闲时不报超时)
fn ssh_connect_stream(
    host: &str,
    port: u16,
    user: &str,
    auth: &SshAuth,
    ask: HostKeyAsk,
) -> Result<ssh2::Session, String> {
    let sess = ssh_connect_t(host, port, user, auth, 0, ask)?;
    sess.set_blocking(false);
    sess.set_keepalive(true, 30);
    Ok(sess)
}

/// 打开命令源:先求值(如 PID),再 spawn 子进程 / 建立 SSH exec 通道
pub fn open_command(
    p: &SourceParams,
    plan: &CommandPlan,
    ask: HostKeyAsk,
) -> Result<Opened, String> {
    let SourceParams::Command { target, .. } = p else {
        return Err("不是命令源".into());
    };
    let mut argv = plan.argv.clone();
    for r in &plan.resolve {
        let out = match target {
            ExecTarget::Local => capture_local(&r.argv)?,
            t => capture_ssh(t, &r.argv, ask)?,
        };
        let value = out.split_whitespace().next().unwrap_or("").to_string();
        if value.is_empty() {
            return Err(i18n::tf(
                "取值失败:{key} 没有输出(目标是否在运行?参数是否正确?)",
                &[("key", r.key.clone())],
            ));
        }
        let token = format!("${{{}}}", r.key);
        for a in &mut argv {
            *a = a.replace(&token, &value);
        }
    }
    if let Some(a) = argv.iter().find(|a| a.contains("${")) {
        return Err(i18n::tf(
            "命令中有未解析的占位符:{a}",
            &[("a", a.to_string())],
        ));
    }
    match target {
        ExecTarget::Local => spawn_local(argv),
        ExecTarget::Ssh {
            host,
            port,
            user,
            auth,
        } => {
            let remote = if plan.raw {
                argv.join(" ")
            } else {
                argv.iter()
                    .map(|s| crate::command::shell_quote(s))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let sess = ssh_connect_stream(host, *port, user, auth, ask)?;
            let mut ch = sess
                .channel_session()
                .map_err(|e| i18n::tf("SSH 通道创建失败:{e}", &[("e", e.to_string())]))?;
            ch.handle_extended_data(ssh2::ExtendedData::Merge)
                .map_err(|e| i18n::tf("SSH stderr 合并失败:{e}", &[("e", e.to_string())]))?;
            ch.exec(&remote)
                .map_err(|e| i18n::tf("远端命令执行失败:{e}", &[("e", e.to_string())]))?;
            let stop = Arc::new(AtomicBool::new(false));
            let exit = Arc::new(Mutex::new(None));
            Ok(Opened::Stream(StreamHandle {
                reader: Box::new(SshReader {
                    ch,
                    stop: stop.clone(),
                    exit: exit.clone(),
                }),
                child: None,
                stop,
                exit,
            }))
        }
    }
}

// ---------- 小工具 ----------

/// 按字符截断并补省略号(标题/最近打开用)
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn auth_has_secret(auth: &SshAuth) -> bool {
    match auth {
        SshAuth::Password(p) => !p.is_empty(),
        SshAuth::KeyText { key, passphrase } => {
            !key.is_empty() || passphrase.as_ref().is_some_and(|s| !s.is_empty())
        }
        SshAuth::KeyFile { passphrase, .. } => passphrase.as_ref().is_some_and(|s| !s.is_empty()),
        SshAuth::Agent => false,
    }
}

fn scrub_auth(auth: &mut SshAuth) {
    match auth {
        SshAuth::Password(p) => *p = String::new(),
        SshAuth::KeyText { key, passphrase } => {
            *key = String::new();
            *passphrase = None;
        }
        SshAuth::KeyFile { passphrase, .. } => *passphrase = None,
        SshAuth::Agent => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(
        auth: HttpAuthKind,
        user: Option<&str>,
        password: Option<&str>,
        token: Option<&str>,
        name: Option<&str>,
        value: Option<&str>,
    ) -> SourceParams {
        SourceParams::Http {
            url: "https://example.com/app.log".into(),
            auth,
            user: user.map(str::to_string),
            password: password.map(str::to_string),
            token: token.map(str::to_string),
            header_name: name.map(str::to_string),
            header_value: value.map(str::to_string),
        }
    }

    #[test]
    fn basic_auth_builds_credentials() {
        let p = http(HttpAuthKind::Basic, Some("u"), Some("p"), None, None, None);
        match build_http_auth(&p).unwrap() {
            Some(HttpAuthHeader::Basic { user, password }) => {
                assert_eq!(user, "u");
                assert_eq!(password, "p");
            }
            _ => panic!("应构造 Basic 认证"),
        }
    }

    #[test]
    fn bearer_auth_builds_authorization_header() {
        let p = http(HttpAuthKind::Bearer, None, None, Some("abc"), None, None);
        match build_http_auth(&p).unwrap() {
            Some(HttpAuthHeader::Header { name, value }) => {
                assert_eq!(name, reqwest::header::AUTHORIZATION);
                assert_eq!(value, "Bearer abc");
            }
            _ => panic!("应构造 Bearer 头"),
        }
    }

    #[test]
    fn api_key_defaults_to_x_api_key() {
        let p = http(HttpAuthKind::ApiKey, None, None, None, None, Some("secret"));
        match build_http_auth(&p).unwrap() {
            Some(HttpAuthHeader::Header { name, value }) => {
                assert_eq!(name.as_str(), "x-api-key");
                assert_eq!(value, "secret");
            }
            _ => panic!("应构造 API Key 头"),
        }
    }

    #[test]
    fn custom_header_and_invalid_name() {
        let p = http(
            HttpAuthKind::Header,
            None,
            None,
            None,
            Some("X-Token"),
            Some("v"),
        );
        match build_http_auth(&p).unwrap() {
            Some(HttpAuthHeader::Header { name, value }) => {
                assert_eq!(name.as_str(), "x-token");
                assert_eq!(value, "v");
            }
            _ => panic!("应构造自定义头"),
        }
        let bad = http(
            HttpAuthKind::Header,
            None,
            None,
            None,
            Some("bad name"),
            Some("v"),
        );
        assert!(build_http_auth(&bad).is_err());
    }

    #[test]
    fn sanitized_drops_all_http_secrets() {
        let p = http(
            HttpAuthKind::Bearer,
            Some("u"),
            Some("p"),
            Some("t"),
            Some("X-Key"),
            Some("v"),
        );
        match p.sanitized() {
            SourceParams::Http {
                password,
                token,
                header_value,
                ..
            } => {
                assert!(password.is_none() && token.is_none() && header_value.is_none());
            }
            _ => panic!("类型不应改变"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_stream_merges_stderr_and_reports_exit() {
        let argv: Vec<String> = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'a\\nb\\n'; printf 'e\\n' 1>&2; exit 3".into(),
        ];
        let plan = CommandPlan {
            argv: argv.clone(),
            resolve: Vec::new(),
            raw: true,
        };
        let params = SourceParams::Command {
            target: ExecTarget::Local,
            argv,
            preset: None,
            values: Default::default(),
            restart: false,
            raw: true,
        };
        let mut o = open_command(&params, &plan, &mut |_| HostKeyDecision::Accept {
            remember: false,
        })
        .expect("启动命令");
        let Opened::Stream(s) = &mut o else {
            panic!("应为流式句柄")
        };
        let mut buf = String::new();
        s.reader.read_to_string(&mut buf).expect("读取输出");
        assert!(buf.contains("a\n"), "{buf}");
        assert!(buf.contains("e\n"), "stderr 未合并: {buf}");
        let code = s
            .child
            .take()
            .and_then(|mut c| c.wait().ok())
            .and_then(|st| st.code());
        assert_eq!(code, Some(3));
    }

    #[test]
    fn command_reports_missing_program() {
        let argv = vec!["logview-no-such-command-xyz".to_string()];
        let plan = CommandPlan {
            argv: argv.clone(),
            resolve: Vec::new(),
            raw: true,
        };
        let params = SourceParams::Command {
            target: ExecTarget::Local,
            argv,
            preset: None,
            values: Default::default(),
            restart: false,
            raw: true,
        };
        let err = match open_command(&params, &plan, &mut |_| HostKeyDecision::Accept {
            remember: false,
        }) {
            Err(e) => e,
            Ok(_) => panic!("不存在的命令不应启动成功"),
        };
        assert!(
            err.contains("未找到命令") || err.contains("Command not found"),
            "{err}"
        );
    }

    #[test]
    fn host_key_algo_and_fingerprint() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&11u32.to_be_bytes());
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&[7u8; 32]);
        assert_eq!(host_key_algo(&blob), "ssh-ed25519");
        let fp = host_key_fingerprint(&blob);
        assert!(fp.starts_with("SHA256:"), "{fp}");
        // SHA-256 为 32 字节,base64 无填充后固定 43 字符
        assert_eq!(fp.len(), "SHA256:".len() + 43);
        assert_eq!(host_key_algo(&[0, 1]), "未知算法");
    }
}
