// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! 凭据存储:优先写入操作系统钥匙串(macOS Keychain / Windows 凭据管理器 / Linux Secret Service),
//! 后端不可用时由调用方降级为「仅本次会话内存 + 手动输入」。
//!
//! 设计要点:
//! - `settings.json` 只保存 `SourceParams::sanitized()` 的结果,任何密码/token 都不落盘;
//! - 同一认证主体(SSH 用户@主机:端口 / HTTP URL)的秘密合并为一条 JSON 写入钥匙串,
//!   减少条目数量与系统弹窗次数;
//! - SSH 私钥正文(`SshAuth::KeyText`)不写入钥匙串:Windows 凭据管理器单条上限
//!   2560 字节,常见私钥正文会超限,且私钥应通过 `KeyFile` 路径引用。

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::i18n;
use crate::source::{SourceParams, SshAuth};

/// 钥匙串服务名:条目形如 (服务=logview, 账号=ssh|user@host:port)
const SERVICE: &str = "logview";

/// 一条日志源需要保存的秘密(全部可选,非空字段才写入)
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Secret {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_value: Option<String>,
}

fn non_empty(v: &str) -> Option<String> {
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

fn non_empty_opt(v: &Option<String>) -> Option<String> {
    v.as_deref().and_then(non_empty)
}

impl Secret {
    /// 从源参数中提取可写入钥匙串的秘密(私钥正文等无法保存的内容被忽略)
    pub fn from_params(p: &SourceParams) -> Secret {
        match p {
            SourceParams::Ssh { auth, .. } => match auth {
                SshAuth::Password(pw) => Secret {
                    password: non_empty(pw),
                    ..Secret::default()
                },
                SshAuth::KeyText { passphrase, .. } | SshAuth::KeyFile { passphrase, .. } => {
                    Secret {
                        passphrase: non_empty_opt(passphrase),
                        ..Secret::default()
                    }
                }
                SshAuth::Agent => Secret::default(),
            },
            SourceParams::Http {
                password,
                token,
                header_value,
                ..
            } => Secret {
                password: non_empty_opt(password),
                token: non_empty_opt(token),
                header_value: non_empty_opt(header_value),
                ..Secret::default()
            },
            SourceParams::Command {
                target:
                    crate::source::ExecTarget::Ssh {
                        host,
                        port,
                        user,
                        auth,
                    },
                ..
            } => Secret::from_params(&SourceParams::Ssh {
                host: host.clone(),
                port: *port,
                user: user.clone(),
                auth: auth.clone(),
                path: String::new(),
            }),
            SourceParams::Command { .. } | SourceParams::Local { .. } => Secret::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.password.is_none()
            && self.passphrase.is_none()
            && self.token.is_none()
            && self.header_value.is_none()
    }

    /// 把保存的秘密回填到参数中缺失的字段,返回是否填入至少一项
    pub fn apply(&self, p: &mut SourceParams) -> bool {
        fn fill_opt(dst: &mut Option<String>, src: &Option<String>) -> bool {
            if dst.as_deref().is_some_and(|s| !s.is_empty()) {
                return false;
            }
            match src {
                Some(v) if !v.is_empty() => {
                    *dst = Some(v.clone());
                    true
                }
                _ => false,
            }
        }

        match p {
            SourceParams::Ssh { auth, .. } => match auth {
                SshAuth::Password(pw) => match &self.password {
                    Some(v) if pw.is_empty() => {
                        *pw = v.clone();
                        true
                    }
                    _ => false,
                },
                SshAuth::KeyText { passphrase, .. } | SshAuth::KeyFile { passphrase, .. } => {
                    fill_opt(passphrase, &self.passphrase)
                }
                SshAuth::Agent => false,
            },
            SourceParams::Http {
                password,
                token,
                header_value,
                ..
            } => {
                let a = fill_opt(password, &self.password);
                let b = fill_opt(token, &self.token);
                let c = fill_opt(header_value, &self.header_value);
                a || b || c
            }
            SourceParams::Command {
                target:
                    crate::source::ExecTarget::Ssh {
                        host,
                        port,
                        user,
                        auth,
                    },
                ..
            } => {
                let mut s = SourceParams::Ssh {
                    host: host.clone(),
                    port: *port,
                    user: user.clone(),
                    auth: auth.clone(),
                    path: String::new(),
                };
                let ok = self.apply(&mut s);
                if let SourceParams::Ssh { auth: a, .. } = s {
                    *auth = a;
                }
                ok
            }
            SourceParams::Command { .. } | SourceParams::Local { .. } => false,
        }
    }
}

pub fn ssh_key(user: &str, host: &str, port: u16) -> String {
    format!("ssh|{user}@{host}:{port}")
}

pub fn http_key(url: &str) -> String {
    format!("http|{url}")
}

/// 源参数对应的钥匙串账号;本地文件没有凭据,返回 None
pub fn key_of(p: &SourceParams) -> Option<String> {
    match p {
        SourceParams::Ssh {
            host, port, user, ..
        } => Some(ssh_key(user, host, *port)),
        SourceParams::Http { url, .. } => Some(http_key(url)),
        SourceParams::Command { .. } | SourceParams::Local { .. } => None,
    }
}

fn entry(key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, key)
        .map_err(|e| i18n::tf("初始化系统钥匙串失败: {e}", &[("e", e.to_string())]))
}

/// 写入(覆盖)一条凭据
pub fn save(key: &str, s: &Secret) -> Result<(), String> {
    let txt = serde_json::to_string(s)
        .map_err(|e| i18n::tf("凭据序列化失败: {e}", &[("e", e.to_string())]))?;
    entry(key)?
        .set_password(&txt)
        .map_err(|e| i18n::tf("写入系统钥匙串失败: {e}", &[("e", e.to_string())]))
}

/// 读取一条凭据;条目不存在返回 Ok(None),后端异常返回 Err
pub fn load(key: &str) -> Result<Option<Secret>, String> {
    match entry(key)?.get_password() {
        Ok(txt) => serde_json::from_str(&txt)
            .map(Some)
            .map_err(|e| i18n::tf("已保存的凭据无法解析: {e}", &[("e", e.to_string())])),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(i18n::tf("读取系统钥匙串失败: {e}", &[("e", e.to_string())])),
    }
}

/// 删除一条凭据(不存在视为成功)
pub fn delete(key: &str) -> Result<(), String> {
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(i18n::tf(
            "删除系统钥匙串条目失败: {e}",
            &[("e", e.to_string())],
        )),
    }
}

/// 系统钥匙串是否可用(首次调用探测一次并缓存;探测只读,不会创建条目)
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| match entry("__probe__") {
        // 正常应答(无论条目是否存在)说明后端工作正常
        Ok(e) => matches!(e.get_password(), Ok(_) | Err(keyring::Error::NoEntry)),
        Err(_) => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_roundtrip() {
        // 无可用钥匙串后端的环境(如无 Secret Service 的 Linux)跳过
        if !available() {
            return;
        }
        let key = "__selftest__";
        let s = Secret {
            password: Some("p".into()),
            ..Secret::default()
        };
        println!("available = {}", available());
        save(key, &s).expect("save failed");
        assert_eq!(load(key).unwrap().unwrap().password.as_deref(), Some("p"));
        delete(key).expect("delete failed");
        assert!(load(key).unwrap().is_none());
    }
}
