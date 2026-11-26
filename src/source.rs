// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 日志源:本地文件 / SSH(SFTP) / HTTP(Range 增量,SSRF 校验)

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

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
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Local { path } => format!("本地 {path}"),
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
            Self::Local { .. } => false,
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
        host: String,
        port: u16,
        user: String,
        auth: SshAuth,
        path: String,
    },
    Http {
        v: Validated,
        supports_range: bool,
        total: Option<u64>,
        auth: Option<HttpAuthHeader>,
    },
}

pub fn open(p: &SourceParams) -> Result<Opened, String> {
    match p {
        SourceParams::Local { path } => {
            if !std::path::Path::new(path).is_file() {
                return Err(format!("文件不存在或不是普通文件: {path}"));
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
            let sess = ssh_connect(host, *port, user, auth)?;
            Ok(Opened::Ssh {
                sess,
                host: host.clone(),
                port: *port,
                user: user.clone(),
                auth: auth.clone(),
                path: path.clone(),
            })
        }
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
                return Err(format!("HTTP {status}(认证失败)"));
            }
            Err(format!("HTTP {status}"))
        }
    }
}

pub fn meta(o: &mut Opened) -> Result<SourceMeta, String> {
    match o {
        Opened::Local { path } => {
            let m = std::fs::metadata(path).map_err(|e| format!("stat 失败: {e}"))?;
            Ok(SourceMeta {
                size: m.len(),
                supports_range: true,
            })
        }
        Opened::Ssh { sess, path, .. } => {
            let sftp = sess.sftp().map_err(|e| format!("SFTP 通道失败: {e}"))?;
            let st = sftp
                .stat(std::path::Path::new(path))
                .map_err(|e| format!("SFTP stat 失败: {e}"))?;
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
    }
}

/// 读取 [off, off+max) 字节,返回 (数据, 文件总大小)
pub fn read_at(o: &mut Opened, off: u64, max: usize) -> Result<(Vec<u8>, u64), String> {
    match o {
        Opened::Local { path } => {
            let total = std::fs::metadata(path.as_path())
                .map_err(|e| format!("stat 失败: {e}"))?
                .len();
            if off >= total {
                return Ok((Vec::new(), total));
            }
            let mut f =
                std::fs::File::open(path.as_path()).map_err(|e| format!("打开失败: {e}"))?;
            f.seek(SeekFrom::Start(off))
                .map_err(|e| format!("seek 失败: {e}"))?;
            let want = ((total - off) as usize).min(max);
            let mut buf = vec![0u8; want];
            let mut got = 0;
            while got < want {
                let n = f
                    .read(&mut buf[got..])
                    .map_err(|e| format!("读取失败: {e}"))?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            buf.truncate(got);
            Ok((buf, total))
        }
        Opened::Ssh { sess, path, .. } => {
            let sftp = sess.sftp().map_err(|e| format!("SFTP 通道失败: {e}"))?;
            let total = sftp
                .stat(std::path::Path::new(path))
                .map_err(|e| format!("SFTP stat 失败: {e}"))?
                .size
                .unwrap_or(0);
            if off >= total {
                return Ok((Vec::new(), total));
            }
            let mut f = sftp
                .open(std::path::Path::new(path))
                .map_err(|e| format!("SFTP open 失败: {e}"))?;
            f.seek(SeekFrom::Start(off))
                .map_err(|e| format!("SFTP seek 失败: {e}"))?;
            let want = ((total - off) as usize).min(max);
            let mut buf = vec![0u8; want];
            let mut got = 0;
            while got < want {
                match f.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(format!("SFTP 读取失败: {e}")),
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
            let mut resp = http_get(&mut client, v, range.as_deref(), auth.as_ref())?;
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
                .map_err(|e| format!("HTTP 读取失败: {e}"))?;
            drop(limited);
            Ok((buf, total))
        }
    }
}

// ---------- SSH ----------

/// 比对 `~/.ssh/known_hosts` 中已有的主机密钥。
///
/// 已知主机的密钥不一致时直接拒绝连接(中间人风险);未知主机按首次连接处理,
/// 不写入 known_hosts(TOFU 确认界面尚未实现,见 SECURITY.md)。
fn verify_known_host(sess: &ssh2::Session, host: &str, port: u16) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let path = home.join(".ssh").join("known_hosts");
    if !path.is_file() {
        return Ok(());
    }
    let mut known_hosts = match sess.known_hosts() {
        Ok(kh) => kh,
        Err(_) => return Ok(()),
    };
    if known_hosts
        .read_file(&path, ssh2::KnownHostFileKind::OpenSSH)
        .is_err()
    {
        return Ok(());
    }
    let Some((key, _kind)) = sess.host_key() else {
        return Ok(());
    };
    match known_hosts.check_port(host, port, key) {
        ssh2::CheckResult::Match | ssh2::CheckResult::NotFound => Ok(()),
        ssh2::CheckResult::Mismatch => Err(format!(
            "SSH 主机密钥与 ~/.ssh/known_hosts 记录不一致({host}:{port}),可能存在中间人攻击;\
             确认服务器换过密钥后请先更新 known_hosts"
        )),
        ssh2::CheckResult::Failure => Err(format!(
            "SSH 主机密钥校验失败({host}:{port}):known_hosts 无法读取"
        )),
    }
}

pub fn ssh_connect(
    host: &str,
    port: u16,
    user: &str,
    auth: &SshAuth,
) -> Result<ssh2::Session, String> {
    let tcp = std::net::TcpStream::connect((host, port))
        .map_err(|e| format!("SSH 连接失败 {host}:{port}: {e}"))?;
    let mut sess = ssh2::Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.set_timeout(20_000);
    sess.handshake().map_err(|e| format!("SSH 握手失败: {e}"))?;
    verify_known_host(&sess, host, port)?;
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
        .map_err(|e| format!("HTTP client 构建失败: {e}"))
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
                .map_err(|e| format!("Bearer token 无效: {e}"))?;
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
                .map_err(|e| format!("请求头名称无效: {e}"))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|e| format!("请求头值无效: {e}"))?;
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
        let resp = req.send().map_err(|e| format!("HTTP 请求失败: {e}"))?;
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
}
