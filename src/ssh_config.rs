//! 解析 `~/.ssh/config`,把其中的具体主机导入为「已保存的 SSH 主机」
//!
//! 只做导入所需的最小解析:`Host` / `HostName` / `Port` / `User`,支持 `#` 注释、
//! 第一个 `Host` 之前的全局默认值。以下情况跳过:
//! - 含通配(`*`、`?`)或取反(`!`)的 Host 块(如默认的 `Host *`)
//! - `Match` 块
//! - `Include` 引用的文件(不递归展开)
//!
//! 结果按 主机+端口+用户名 去重(不区分主机名大小写),同一目标只保留第一处。
//! 别名(第一个具体 Host 名)写入 `SavedHost.name` 作为备注。

use std::path::PathBuf;

use crate::store::SavedHost;

/// `~/.ssh/config` 路径(读不到主目录时为 None)
pub fn config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("config"))
}

/// 读取并解析 `~/.ssh/config`;文件不存在或不可读时返回面向用户的错误
pub fn load() -> Result<Vec<SavedHost>, String> {
    let Some(path) = config_path() else {
        return Err("无法定位用户主目录,未导入".to_string());
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    Ok(parse(&text))
}

/// 解析 ssh_config 文本(已去重);空文本返回空列表
pub fn parse(text: &str) -> Vec<SavedHost> {
    let os_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();

    // 全局默认值:第一个 Host 之前的指令;之后每个 Host 块以它为基础
    let mut def_host = String::new();
    let mut def_port: u16 = 22;
    let mut def_user = os_user;

    // 当前块:Defaults = 仍处于全局段;Host = 具体主机;Skipped = 通配/Match 块
    enum State {
        Defaults,
        Host(SavedHost),
        Skipped,
    }
    let mut state = State::Defaults;
    let mut out: Vec<SavedHost> = Vec::new();

    fn flush(state: &mut State, out: &mut Vec<SavedHost>) {
        if let State::Host(h) = std::mem::replace(state, State::Defaults) {
            if h.user.is_empty() {
                return;
            }
            let host = if h.host.trim().is_empty() {
                h.name.clone()
            } else {
                h.host.trim().to_string()
            };
            out.push(SavedHost { host, ..h });
        }
    }

    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let (key, val) = match line.split_once(char::is_whitespace) {
            Some((k, v)) => (k, v.trim()),
            None => (line, ""),
        };
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                flush(&mut state, &mut out);
                let mut alias: Option<String> = None;
                let mut wildcard = false;
                for pat in val.split_whitespace() {
                    let p = unquote(pat);
                    if p.is_empty() {
                        continue;
                    }
                    if p.contains('*') || p.contains('?') || p.starts_with('!') {
                        wildcard = true;
                    } else if alias.is_none() {
                        alias = Some(p.to_string());
                    }
                }
                state = match (wildcard, alias) {
                    (false, Some(name)) => State::Host(SavedHost {
                        name,
                        host: def_host.clone(),
                        port: def_port,
                        user: def_user.clone(),
                    }),
                    // 通配块,或没有任何具体别名
                    _ => State::Skipped,
                };
            }
            "match" => {
                flush(&mut state, &mut out);
                state = State::Skipped;
            }
            "hostname" => {
                let v = unquote(val).to_string();
                match &mut state {
                    State::Host(h) => h.host = v,
                    State::Defaults => def_host = v,
                    State::Skipped => {}
                }
            }
            "port" => {
                let p = val.parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(22);
                match &mut state {
                    State::Host(h) => h.port = p,
                    State::Defaults => def_port = p,
                    State::Skipped => {}
                }
            }
            "user" => {
                let v = unquote(val).to_string();
                match &mut state {
                    State::Host(h) => h.user = v,
                    State::Defaults => def_user = v,
                    State::Skipped => {}
                }
            }
            // Include / IdentityFile / ProxyJump 等导入不需要,忽略
            _ => {}
        }
    }
    flush(&mut state, &mut out);

    // 按 主机(不区分大小写)+端口+用户名 去重,保留先出现的一条
    let mut uniq: Vec<SavedHost> = Vec::with_capacity(out.len());
    for h in out {
        let dup = uniq.iter().any(|u| {
            u.host.eq_ignore_ascii_case(&h.host) && u.port == h.port && u.user == h.user
        });
        if !dup {
            uniq.push(h);
        }
    }
    uniq
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"').trim_matches('\'')
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_concrete_hosts() {
        let text = "\
# 生产环境
Host prod-db
    HostName 10.0.0.5
    User admin
    Port 2222

Host web
    HostName web.example.com

Host alias-only
    User me
";
        let list = parse(text);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "prod-db");
        assert_eq!(list[0].host, "10.0.0.5");
        assert_eq!(list[0].port, 2222);
        assert_eq!(list[0].user, "admin");
        assert_eq!(list[1].host, "web.example.com");
        assert_eq!(list[1].port, 22);
        // 未写 HostName 时用别名充当主机名
        assert_eq!(list[2].host, "alias-only");
        assert_eq!(list[2].user, "me");
    }

    #[test]
    fn skips_wildcard_and_match_blocks() {
        let text = "\
Host *
    User everyone
    Port 2000

Host !bad good
    User x

Match host foo
    User y

Host real
    HostName 1.2.3.4
    User u
";
        let list = parse(text);
        assert_eq!(list.len(), 1, "{:?}", list.iter().map(|h| &h.name).collect::<Vec<_>>());
        assert_eq!(list[0].name, "real");
        assert_eq!(list[0].port, 22, "通配块的 Port 不应影响后续主机");
        assert_eq!(list[0].user, "u");
    }

    #[test]
    fn applies_global_defaults_before_first_host() {
        let text = "\
User root
Port 2200

Host a
    HostName h1

Host b
    HostName h2
    Port 22
";
        let list = parse(text);
        assert_eq!(list[0].user, "root");
        assert_eq!(list[0].port, 2200);
        assert_eq!(list[1].port, 22);
    }

    #[test]
    fn dedupes_same_target() {
        let text = "\
Host a
    HostName 1.1.1.1
    User u

Host b
    HostName 1.1.1.1
    User u

Host c
    HostName 1.1.1.1
    User other
";
        let list = parse(text);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "a");
        assert_eq!(list[1].user, "other");
    }

    #[test]
    fn single_alias_block_and_comments() {
        let text = "\
Host only # 行尾注释
    HostName \"only.example.com\"
    User \"me\"
";
        let list = parse(text);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].host, "only.example.com");
        assert_eq!(list[0].user, "me");
    }
}
