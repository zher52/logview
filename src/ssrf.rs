// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! SSRF 防护:仅 http/https;DNS 全地址逐个校验,拒绝 localhost/环回/私有/保留网段;
//! 返回固定的首个合法 IP 供连接使用,防 DNS rebinding

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

pub struct Validated {
    pub url: reqwest::Url,
    pub addr: SocketAddr,
}

pub fn validate_url(raw: &str) -> Result<Validated, String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("URL 无效: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        s => return Err(format!("仅允许 http/https 协议(收到: {s})")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("不支持携带用户名/密码的 URL".into());
    }
    let host = url.host_str().ok_or("URL 缺少主机名")?.to_ascii_lowercase();
    let port = url.port_or_known_default().ok_or("URL 缺少端口")?;
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return Err("拒绝 localhost/.local 地址(SSRF 防护)".into());
    }
    let ips: Vec<IpAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![ip]
    } else {
        (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| format!("DNS 解析失败: {e}"))?
            .map(|a| a.ip())
            .collect()
    };
    if ips.is_empty() {
        return Err("DNS 解析无结果".into());
    }
    for ip in &ips {
        if is_banned(*ip) {
            return Err(format!(
                "拒绝内网/环回/保留地址 {ip}(SSRF 防护,仅允许公网地址)"
            ));
        }
    }
    Ok(Validated {
        url,
        addr: SocketAddr::new(ips[0], port),
    })
}

pub fn is_banned(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => banned_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(m) = v6.to_ipv4_mapped() {
                return banned_v4(m);
            }
            banned_v6(v6)
        }
    }
}

fn banned_v4(ip: Ipv4Addr) -> bool {
    let n = u32::from(ip);
    const RANGES: [(u32, u32); 15] = [
        (0x0000_0000, 8),  // 0.0.0.0/8
        (0x0A00_0000, 8),  // 10/8
        (0x6440_0000, 10), // 100.64/10
        (0x7F00_0000, 8),  // 127/8 环回
        (0xA9FE_0000, 16), // 169.254/16 链路本地
        (0xAC10_0000, 12), // 172.16/12 私有
        (0xC000_0000, 24), // 192.0.0/24
        (0xC000_0200, 24), // 192.0.2/24 TEST-NET
        (0xC058_6300, 24), // 192.88.99/24
        (0xC0A8_0000, 16), // 192.168/16 私有
        (0xC612_0000, 15), // 198.18/15 基准测试
        (0xC633_6400, 24), // 198.51.100/24 TEST-NET
        (0xCB00_7100, 24), // 203.0.113/24 TEST-NET
        (0xE000_0000, 4),  // 组播
        (0xF000_0000, 4),  // 保留
    ];
    if ip == Ipv4Addr::BROADCAST {
        return true;
    }
    RANGES.iter().any(|(base, prefix)| {
        let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
        n & mask == base & mask
    })
}

fn banned_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    let s = ip.segments();
    // fc00::/7 独有本地、fe80::/10 链路本地、2001:db8::/32 文档保留
    (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || (s[0] == 0x2001 && s[1] == 0x0db8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse().unwrap())
    }
    fn v6(s: &str) -> IpAddr {
        IpAddr::V6(s.parse().unwrap())
    }

    #[test]
    fn rejects_private_and_reserved_v4() {
        for s in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(is_banned(v4(s)), "{s} 应被拒绝");
        }
    }

    #[test]
    fn allows_public_v4() {
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "172.32.0.1",
            "192.169.0.1",
        ] {
            assert!(!is_banned(v4(s)), "{s} 应被允许");
        }
    }

    #[test]
    fn rejects_reserved_v6_and_mapped_private() {
        for s in [
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(is_banned(v6(s)), "{s} 应被拒绝");
        }
        assert!(is_banned(v6("::ffff:10.0.0.1")), "IPv4-mapped 私网应被拒绝");
    }

    #[test]
    fn allows_public_v6() {
        assert!(!is_banned(v6("2606:4700:4700::1111")));
    }

    #[test]
    fn rejects_non_http_scheme() {
        assert!(validate_url("ftp://example.com/x").is_err());
        assert!(validate_url("file:///etc/hosts").is_err());
    }

    #[test]
    fn rejects_userinfo_in_url() {
        assert!(validate_url("http://user:pw@example.com/").is_err());
        assert!(validate_url("https://user@example.com/").is_err());
    }

    #[test]
    fn rejects_local_hostnames() {
        assert!(validate_url("http://localhost:8080/").is_err());
        assert!(validate_url("http://foo.localhost/").is_err());
        assert!(validate_url("http://printer.local/").is_err());
    }

    #[test]
    fn rejects_private_literal_ip() {
        assert!(validate_url("http://127.0.0.1/").is_err());
        assert!(validate_url("http://10.0.0.5:8080/").is_err());
        assert!(validate_url("http://[::1]/").is_err());
    }

    #[test]
    fn pins_public_ip_and_default_port() {
        let v = validate_url("https://8.8.8.8/x").expect("公网 IP 应通过");
        assert_eq!(v.addr.port(), 443);
        assert_eq!(v.addr.ip(), v4("8.8.8.8"));

        let v = validate_url("http://1.1.1.1:8080/health").expect("带端口应通过");
        assert_eq!(v.addr.port(), 8080);
    }
}
