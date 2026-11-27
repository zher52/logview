// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 zher52

//! SSH 会话层:连接复用、空闲自动关闭、断线按需重连。
//!
//! 同一「用户@主机:端口 + 凭据」只保留一条连接:多标签(SSH 文件源、SSH 命令源、
//! 日志里点开的远端路径)共享同一个 [`SshSession`],不再各自握手认证。
//! 所有标签关闭后连接保留 [`IDLE_GRACE`],超时由清扫线程断开;
//! 连接意外断开时,下一次操作按需重连(同一会话的所有标签共用这次重连)。
//!
//! 共享会话工作在**非阻塞**模式:一次读不会卡住其它标签(阻塞调用会锁住整个会话)。
//! 会话内的操作因此必须允许 `EAGAIN`:SFTP 调用用 [`retry`] 重试,命令通道由读端自己退避。

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::i18n;
use crate::source::SshAuth;

/// 首次连接(握手 + 主机密钥校验 + 认证)的超时
const CONNECT_TIMEOUT_MS: u32 = 20_000;
/// 复连超时:比首次短,断网时不至于长时间占住工作线程
const RECONNECT_TIMEOUT_MS: u32 = 10_000;
/// 单次会话操作(含非阻塞重试)的总预算
const OP_TIMEOUT: Duration = Duration::from_secs(10);
/// 两次重连尝试的最小间隔,避免断网时每个标签都阻塞重连
const RETRY_GAP: Duration = Duration::from_secs(2);
/// 所有标签关闭后保留连接的时间,便于立即重开时免重连
const IDLE_GRACE: Duration = Duration::from_secs(30);
/// 空闲清扫节拍
const SWEEP_EVERY: Duration = Duration::from_secs(1);
/// libssh2 的 `LIBSSH2_ERROR_EAGAIN`:非阻塞会话的正常返回值
const EAGAIN: i32 = -37;

// ---------- 主机密钥(TOFU) ----------

/// 待确认的未知主机密钥
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

/// 首次连接时如何对待未知主机密钥
enum Trust<'a> {
    /// 新会话:交给 UI 确认
    AskUi(HostKeyAsk<'a>),
    /// 复连:沿用首次连接的确认结果(主机密钥变化仍会被 known_hosts 拒绝)
    Recorded(HostKeyDecision),
}

fn host_key_algo(key: &[u8]) -> String {
    (|| {
        let n = u32::from_be_bytes(key.get(..4)?.try_into().ok()?) as usize;
        std::str::from_utf8(key.get(4..4 + n)?)
            .ok()
            .map(str::to_owned)
    })()
    .unwrap_or_else(|| i18n::t("error.ssh.unknown_algorithm").into())
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
    let path = known_hosts_path().ok_or(i18n::t("error.known_hosts.no_home").to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| i18n::tf("error.known_hosts.mkdir", &[("e", e.to_string())]))?;
    }
    let mut known_hosts = sess
        .known_hosts()
        .map_err(|e| i18n::tf("error.known_hosts.init", &[("e", e.to_string())]))?;
    if path.is_file() {
        known_hosts
            .read_file(&path, ssh2::KnownHostFileKind::OpenSSH)
            .map_err(|e| i18n::tf("error.known_hosts.read", &[("e", e.to_string())]))?;
    }
    let (key, kind) = sess
        .host_key()
        .ok_or(i18n::t("error.ssh.host_key_missing").to_string())?;
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
        .map_err(|e| i18n::tf("error.known_hosts.write", &[("e", e.to_string())]))?;
    known_hosts
        .write_file(&path, ssh2::KnownHostFileKind::OpenSSH)
        .map_err(|e| i18n::tf("error.known_hosts.save", &[("e", e.to_string())]))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 已知主机密钥不一致时拒绝;未知主机交由 `ask` 决定(TOFU)。
///
/// 返回 `None` 表示密钥与 known_hosts 一致,`Some(…)` 表示用户对未知主机的决定。
fn verify_known_host(
    sess: &ssh2::Session,
    host: &str,
    port: u16,
    ask: HostKeyAsk,
) -> Result<Option<HostKeyDecision>, String> {
    let Some((key, _kind)) = sess.host_key() else {
        return Ok(None);
    };
    let mut known_hosts = match sess.known_hosts() {
        Ok(kh) => kh,
        Err(_) => return Ok(None),
    };
    if let Some(path) = known_hosts_path() {
        if path.is_file()
            && known_hosts
                .read_file(&path, ssh2::KnownHostFileKind::OpenSSH)
                .is_err()
        {
            return Err(i18n::tf(
                "error.known_hosts.unreadable",
                &[("host", host.to_string()), ("port", port.to_string())],
            ));
        }
    }
    match known_hosts.check_port(host, port, key) {
        ssh2::CheckResult::Match => Ok(None),
        ssh2::CheckResult::Mismatch => Err(i18n::tf(
            "error.known_hosts.mismatch",
            &[("host", host.to_string()), ("port", port.to_string())],
        )),
        ssh2::CheckResult::Failure => Err(i18n::tf(
            "error.known_hosts.unreadable",
            &[("host", host.to_string()), ("port", port.to_string())],
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
                    Ok(Some(HostKeyDecision::Accept { remember }))
                }
                HostKeyDecision::Reject => Err(i18n::tf(
                    "error.ssh.host_key_unconfirmed",
                    &[("host", host.to_string()), ("port", port.to_string())],
                )),
            }
        }
    }
}

// ---------- 连接目标与复用键 ----------

/// SSH 连接目标(不含路径):会话复用的依据
#[derive(Clone)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
}

impl SshTarget {
    pub fn new(host: impl Into<String>, port: u16, user: impl Into<String>, auth: SshAuth) -> Self {
        Self {
            host: host.into(),
            port,
            user: user.into(),
            auth,
        }
    }

    /// 复用键:主机、端口、用户与凭据摘要。凭据不同的连接不共享会话。
    fn key(&self) -> SessionKey {
        SessionKey {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth: auth_digest(&self.auth),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    host: String,
    port: u16,
    user: String,
    /// 凭据材料的 SHA-256(只用于区分会话,不保留明文)
    auth: String,
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let short = &self.auth[..self.auth.len().min(8)];
        write!(f, "{}@{}:{}#{short}", self.user, self.host, self.port)
    }
}

/// 凭据摘要:同一凭据得到同一摘要,不同凭据(含口令、私钥路径)互不相同
fn auth_digest(auth: &SshAuth) -> String {
    let mut h = Sha256::new();
    match auth {
        SshAuth::Password(p) => {
            h.update(b"password\0");
            h.update(p.as_bytes());
        }
        SshAuth::KeyText { key, passphrase } => {
            h.update(b"keytext\0");
            h.update(key.as_bytes());
            h.update(b"\0");
            h.update(passphrase.as_deref().unwrap_or("").as_bytes());
        }
        SshAuth::KeyFile { path, passphrase } => {
            h.update(b"keyfile\0");
            h.update(path.as_bytes());
            h.update(b"\0");
            h.update(passphrase.as_deref().unwrap_or("").as_bytes());
        }
        SshAuth::Agent => h.update(b"agent"),
    }
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(h.finalize())
}

// ---------- 会话句柄 ----------

/// 共享的 SSH 会话句柄(可克隆);最后一个句柄释放后由清扫线程延迟关闭连接。
#[derive(Clone)]
pub struct SshSession {
    inner: Arc<Shared>,
}

impl SshSession {
    /// 远端文件大小(与 `stat` 一致)
    pub fn stat(&self, path: &str) -> Result<u64, String> {
        self.inner.with_sftp(|sftp| {
            let st = retry("error.sftp.stat", || sftp.stat(Path::new(path)))?;
            Ok(st.size.unwrap_or(0))
        })
    }

    /// 读取 [off, off+max) 字节,返回 (数据, 文件总大小)
    pub fn read_at(&self, path: &str, off: u64, max: usize) -> Result<(Vec<u8>, u64), String> {
        self.inner.with_sftp(|sftp| {
            let st = retry("error.sftp.stat", || sftp.stat(Path::new(path)))?;
            let total = st.size.unwrap_or(0);
            if off >= total {
                return Ok((Vec::new(), total));
            }
            let mut f = retry("error.sftp.open", || sftp.open(Path::new(path)))?;
            f.seek(SeekFrom::Start(off)).map_err(|e| Fail {
                msg: i18n::tf("error.sftp.seek", &[("e", e.to_string())]),
                fatal: false,
            })?;
            let want = ((total - off) as usize).min(max);
            let mut buf = vec![0u8; want];
            let mut got = 0;
            while got < want {
                match retry_io("error.sftp.read", || f.read(&mut buf[got..]))? {
                    0 => break,
                    n => got += n,
                }
            }
            buf.truncate(got);
            Ok((buf, total))
        })
    }

    /// 在远端执行命令,stdout+stderr 合并为单向流
    pub fn exec_stream(&self, command: &str) -> Result<RemoteStream, String> {
        match self.inner.open_channel(command) {
            Ok((ch, generation)) => Ok(RemoteStream::new(ch, self.clone(), generation)),
            Err(f) => {
                let session_lost = f.fatal;
                let first = self.inner.fail(f);
                if !session_lost {
                    return Err(first);
                }
                // 会话刚断开:重连后只再试一次,避免把一次网络抖动放大成重复执行
                let (ch, generation) = self
                    .inner
                    .open_channel(command)
                    .map_err(|f| self.inner.fail(f))?;
                Ok(RemoteStream::new(ch, self.clone(), generation))
            }
        }
    }

    /// 执行一条短命令并返回 stdout(用于 adb pidof 之类的取值)
    pub fn capture(&self, command: &str) -> Result<String, String> {
        let (mut ch, generation) = self
            .inner
            .open_channel(command)
            .map_err(|f| self.inner.fail(f))?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 8 << 10];
        let deadline = Instant::now() + OP_TIMEOUT;
        loop {
            match ch.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        self.inner.mark_dead_if(generation);
                        return Err(timeout());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => {
                    self.inner.mark_dead_if(generation);
                    return Err(i18n::tf(
                        "error.command.remote_value_read",
                        &[("e", e.to_string())],
                    ));
                }
            }
        }
        let _ = ch.wait_close();
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    #[cfg(test)]
    fn inner_is_connected(&self) -> bool {
        self.inner.state().conn.is_some()
    }
}

/// 命令输出流:非阻塞读,EOF 后补取退出码
pub struct RemoteStream {
    ch: ssh2::Channel,
    session: SshSession,
    /// 通道所属连接代次:过期通道的读错误不能把刚重建的连接标记为断开
    generation: u64,
    stop: Arc<AtomicBool>,
    exit: Arc<Mutex<Option<i32>>>,
}

impl RemoteStream {
    fn new(ch: ssh2::Channel, session: SshSession, generation: u64) -> Self {
        Self {
            ch,
            session,
            generation,
            stop: Arc::new(AtomicBool::new(false)),
            exit: Arc::new(Mutex::new(None)),
        }
    }

    /// 停止标志:读端据此尽快返回 EOF
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// 退出码槽位:读端读到 EOF 后写入
    pub fn exit_slot(&self) -> Arc<Mutex<Option<i32>>> {
        self.exit.clone()
    }
}

impl Read for RemoteStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
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
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(0)
            }
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                Err(io::Error::from(io::ErrorKind::WouldBlock))
            }
            Err(e) => {
                // 通道读失败通常意味着连接已断:标记后会由下一次操作重连
                self.session.inner.mark_dead_if(self.generation);
                Err(e)
            }
        }
    }
}

// ---------- 会话内部状态 ----------

struct Shared {
    target: SshTarget,
    state: Mutex<State>,
    /// 最近一次被使用的时刻(Unix 毫秒),用于空闲清扫
    last_used: AtomicI64,
}

struct State {
    conn: Option<Conn>,
    /// 首次连接时的主机密钥决定;复连沿用
    trust: Option<HostKeyDecision>,
    /// 最近一次连接尝试的时刻(失败后短时间内不再重试)
    last_attempt: Option<Instant>,
    /// 连接代次计数:重连后递增,用于忽略旧通道的迟到错误
    next_generation: u64,
}

struct Conn {
    sess: ssh2::Session,
    /// 懒初始化的 SFTP 子系统,复用后每次读取不必重建通道
    sftp: Option<ssh2::Sftp>,
    /// 本条连接的代次
    generation: u64,
}

impl Drop for Conn {
    fn drop(&mut self) {
        // SFTP 子系统的 Drop 会访问会话内部指针,必须先于会话本身释放
        self.sftp = None;
    }
}

impl Shared {
    fn new(target: SshTarget, sess: ssh2::Session, trust: HostKeyDecision) -> Self {
        Self {
            target,
            state: Mutex::new(State {
                conn: Some(Conn {
                    sess,
                    sftp: None,
                    generation: 1,
                }),
                trust: Some(trust),
                last_attempt: Some(Instant::now()),
                next_generation: 2,
            }),
            last_used: AtomicI64::new(now_ms()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn touch(&self) {
        self.last_used.store(now_ms(), Ordering::Relaxed);
    }

    fn last_used(&self) -> i64 {
        self.last_used.load(Ordering::Relaxed)
    }

    /// 确保连接可用:已连接直接返回,否则按需重连。
    ///
    /// 上次尝试失败后先等满 [`RETRY_GAP`] 再试:多个标签轮询时不会各自发起重连,
    /// 也不会因“太快失败”把一次网络抖动累计成致命错误。
    fn ensure_connected(&self) -> Result<(), String> {
        let mut st = self.state();
        if st.conn.is_some() {
            return Ok(());
        }
        if let Some(t) = st.last_attempt {
            let waited = t.elapsed();
            if waited < RETRY_GAP {
                std::thread::sleep(RETRY_GAP - waited);
            }
            if st.conn.is_some() {
                // 等待期间别的标签已经重连成功
                return Ok(());
            }
        }
        st.last_attempt = Some(Instant::now());
        let trust = st
            .trust
            .ok_or_else(|| i18n::t("error.ssh.disconnected").to_string())?;
        // 持锁重连:其他标签的操作等待这次结果,不会各自再连一条
        let (sess, _) =
            connect_transport(&self.target, Trust::Recorded(trust), RECONNECT_TIMEOUT_MS)?;
        let generation = st.next_generation;
        st.next_generation = generation.wrapping_add(1);
        st.conn = Some(Conn {
            sess,
            sftp: None,
            generation,
        });
        self.touch();
        Ok(())
    }

    /// 标记连接失效:下一次操作会重连;重置节流以便立即重试
    fn mark_dead(&self) {
        let mut st = self.state();
        st.conn = None;
        st.last_attempt = None;
    }

    /// 只清掉指定代次的连接:迟到的旧通道错误不能把刚重建的连接标记为断开
    fn mark_dead_if(&self, generation: u64) {
        let mut st = self.state();
        if st.conn.as_ref().is_some_and(|c| c.generation == generation) {
            st.conn = None;
            st.last_attempt = None;
        }
    }

    /// 主动关闭连接(空闲清扫 / 丢弃多余连接)
    fn disconnect(&self) {
        let conn = {
            let mut st = self.state();
            st.conn.take()
        };
        if let Some(mut conn) = conn {
            // 先释放 SFTP 子系统,再断开并释放会话
            conn.sftp = None;
            let _ = conn.sess.disconnect(None, "logview: session closed", None);
            drop(conn);
        }
    }

    /// 在连接可用时执行一次 SFTP 操作;失败时按需标记连接失效
    fn with_sftp<T>(&self, f: impl FnOnce(&ssh2::Sftp) -> Result<T, Fail>) -> Result<T, String> {
        self.touch();
        let r = (|| -> Result<T, Fail> {
            self.ensure_connected()
                .map_err(|msg| Fail { msg, fatal: false })?;
            let mut st = self.state();
            let Some(conn) = st.conn.as_mut() else {
                return Err(disconnected());
            };
            if conn.sftp.is_none() {
                conn.sftp = Some(retry("error.sftp.channel", || conn.sess.sftp())?);
            }
            let sftp = conn.sftp.as_ref().expect("刚刚初始化");
            f(sftp)
        })();
        match r {
            Ok(v) => Ok(v),
            Err(f) => Err(self.fail(f)),
        }
    }

    /// 打开一个执行通道(与文件读取共用同一会话),同时返回连接代次
    fn open_channel(&self, command: &str) -> Result<(ssh2::Channel, u64), Fail> {
        self.ensure_connected()
            .map_err(|msg| Fail { msg, fatal: false })?;
        let conn = {
            let st = self.state();
            st.conn.as_ref().map(|c| (c.sess.clone(), c.generation))
        };
        let Some((sess, generation)) = conn else {
            return Err(disconnected());
        };
        let mut ch = retry("error.ssh.channel", || sess.channel_session())?;
        retry("error.ssh.stderr_merge", || {
            ch.handle_extended_data(ssh2::ExtendedData::Merge)
        })?;
        retry("error.command.remote_run", || ch.exec(command))?;
        Ok((ch, generation))
    }

    fn fail(&self, f: Fail) -> String {
        if f.fatal {
            self.mark_dead();
        }
        f.msg
    }
}

// ---------- 注册表与清扫 ----------

fn registry() -> &'static Mutex<HashMap<SessionKey, Arc<Shared>>> {
    static REG: OnceLock<Mutex<HashMap<SessionKey, Arc<Shared>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registry_lock() -> MutexGuard<'static, HashMap<SessionKey, Arc<Shared>>> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// 首次使用时启动清扫线程:关闭所有标签都已退出、且超过宽限期的连接
fn start_janitor() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("ssh-session-janitor".into())
            .spawn(|| loop {
                std::thread::sleep(SWEEP_EVERY);
                sweep(now_ms());
            });
    });
}

/// 连接复用入口:命中已有会话则直接共享,否则新建并登记。
///
/// `ask` 只在新会话遇到未知主机时使用;命中复用不会再次询问。
pub fn acquire(target: &SshTarget, ask: HostKeyAsk) -> Result<SshSession, String> {
    start_janitor();
    let key = target.key();
    if let Some(shared) = {
        let reg = registry_lock();
        reg.get(&key).cloned()
    } {
        shared.touch();
        // 复用前确认连接仍可用(已连接时是空操作)
        shared.ensure_connected()?;
        return Ok(SshSession { inner: shared });
    }

    let (sess, trust) = connect_transport(target, Trust::AskUi(ask), CONNECT_TIMEOUT_MS)?;
    let fresh = Arc::new(Shared::new(target.clone(), sess, trust));
    // 并发打开同一主机时可能已有人先建好:优先复用,丢弃本次多余的连接
    let chosen = {
        let mut reg = registry_lock();
        match reg.get(&key) {
            Some(existing) => {
                existing.touch();
                existing.clone()
            }
            None => {
                reg.insert(key, fresh.clone());
                fresh.clone()
            }
        }
    };
    if !Arc::ptr_eq(&chosen, &fresh) {
        fresh.disconnect();
        chosen.ensure_connected()?;
    }
    Ok(SshSession { inner: chosen })
}

/// 关闭全部会话(退出或测试清理用)
pub fn close_all() {
    let all: Vec<Arc<Shared>> = {
        let mut reg = registry_lock();
        reg.drain().map(|(_, s)| s).collect()
    };
    for s in all {
        s.disconnect();
    }
}

/// 关闭已无标签使用、且空闲超过宽限期的会话
fn sweep(now: i64) {
    let expired = {
        let mut reg = registry_lock();
        let keys = expired_keys(&reg, now, IDLE_GRACE.as_millis() as i64);
        keys.into_iter()
            .filter_map(|k| reg.remove(&k))
            .collect::<Vec<_>>()
    };
    for s in expired {
        s.disconnect();
    }
}

/// 仅剩注册表持有(所有标签已关闭)且空闲超过宽限期的会话键
fn expired_keys(
    map: &HashMap<SessionKey, Arc<Shared>>,
    now: i64,
    grace_ms: i64,
) -> Vec<SessionKey> {
    map.iter()
        .filter(|(_, s)| Arc::strong_count(s) == 1 && now.saturating_sub(s.last_used()) > grace_ms)
        .map(|(k, _)| k.clone())
        .collect()
}

// ---------- 连接与重试 ----------

/// 建立连接:握手 → 主机密钥校验 → 认证,随后切换为非阻塞共享模式
fn connect_transport(
    target: &SshTarget,
    trust: Trust<'_>,
    timeout_ms: u32,
) -> Result<(ssh2::Session, HostKeyDecision), String> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port)).map_err(|e| {
        i18n::tf(
            "error.ssh.connect",
            &[
                ("host", target.host.clone()),
                ("port", target.port.to_string()),
                ("e", e.to_string()),
            ],
        )
    })?;
    let _ = tcp.set_nodelay(true);
    let mut sess = ssh2::Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.set_timeout(timeout_ms);
    sess.handshake()
        .map_err(|e| i18n::tf("error.ssh.handshake", &[("e", e.to_string())]))?;
    let decision = match trust {
        Trust::AskUi(ask) => verify_known_host(&sess, &target.host, target.port, ask)?,
        Trust::Recorded(d) => verify_known_host(&sess, &target.host, target.port, &mut |_| d)?,
    };
    let ok = match &target.auth {
        SshAuth::Password(p) => sess.userauth_password(&target.user, p).is_ok(),
        SshAuth::KeyText { key, passphrase } => sess
            .userauth_pubkey_memory(&target.user, None, key, passphrase.as_deref())
            .is_ok(),
        SshAuth::KeyFile { path, passphrase } => sess
            .userauth_pubkey_file(&target.user, None, Path::new(path), passphrase.as_deref())
            .is_ok(),
        SshAuth::Agent => sess.userauth_agent(&target.user).is_ok(),
    };
    if !ok || !sess.authenticated() {
        return Err(i18n::t("error.ssh.auth").to_string());
    }
    // 共享会话必须非阻塞:一个标签的读不能卡住其它标签
    sess.set_blocking(false);
    sess.set_keepalive(true, 30);
    // 已知主机复连时没有用户决定,统一按「仅本次」处理:主机密钥变化仍会被拒绝
    Ok((
        sess,
        decision.unwrap_or(HostKeyDecision::Accept { remember: false }),
    ))
}

#[derive(Debug)]
struct Fail {
    msg: String,
    fatal: bool,
}

fn disconnected() -> Fail {
    Fail {
        msg: i18n::t("error.ssh.disconnected").to_string(),
        fatal: false,
    }
}

fn timeout() -> String {
    i18n::tf(
        "error.ssh.timeout",
        &[("secs", OP_TIMEOUT.as_secs().to_string())],
    )
}

/// 非阻塞重试:会话为非阻塞模式,`EAGAIN` 需要在预算内反复调用
fn retry<T>(id: &'static str, mut op: impl FnMut() -> Result<T, ssh2::Error>) -> Result<T, Fail> {
    let deadline = Instant::now() + OP_TIMEOUT;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if e.code() == ssh2::ErrorCode::Session(EAGAIN) => {
                if Instant::now() >= deadline {
                    return Err(Fail {
                        msg: timeout(),
                        fatal: true,
                    });
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => {
                // SFTP 状态码是文件级问题(不存在、无权限),不代表连接断开
                let fatal = !matches!(e.code(), ssh2::ErrorCode::SFTP(_));
                return Err(Fail {
                    msg: i18n::tf(id, &[("e", e.message().to_string())]),
                    fatal,
                });
            }
        }
    }
}

/// 文件级错误不视为连接断开
fn io_fatal(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
    )
}

/// 与 [`retry`] 相同,但用于只暴露 `io::Error` 的接口(SFTP `File` 读写)
fn retry_io<T>(id: &'static str, mut op: impl FnMut() -> io::Result<T>) -> Result<T, Fail> {
    let deadline = Instant::now() + OP_TIMEOUT;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(Fail {
                        msg: timeout(),
                        fatal: true,
                    });
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => {
                return Err(Fail {
                    msg: i18n::tf(id, &[("e", e.to_string())]),
                    fatal: io_fatal(e.kind()),
                });
            }
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(password: &str) -> SshTarget {
        SshTarget::new("example.com", 22, "dev", SshAuth::Password(password.into()))
    }

    fn bare_shared() -> Arc<Shared> {
        // 不联网:仅用于注册表/空闲判定,连接从未使用
        let sess = ssh2::Session::new().expect("创建会话");
        Arc::new(Shared::new(
            target("pw"),
            sess,
            HostKeyDecision::Accept { remember: false },
        ))
    }

    #[test]
    fn same_target_shares_session_key() {
        assert_eq!(target("pw").key(), target("pw").key());
        let mut other = target("pw");
        other.port = 2222;
        assert_ne!(target("pw").key(), other.key());
        let mut other = target("pw");
        other.user = "ops".into();
        assert_ne!(target("pw").key(), other.key());
        let mut other = target("pw");
        other.host = "example.net".into();
        assert_ne!(target("pw").key(), other.key());
    }

    #[test]
    fn different_credentials_do_not_share_session() {
        assert_ne!(target("one").key(), target("two").key());
        let key_auth = SshAuth::KeyFile {
            path: "/tmp/id_ed25519".into(),
            passphrase: None,
        };
        let with_pass = SshAuth::KeyFile {
            path: "/tmp/id_ed25519".into(),
            passphrase: Some("secret".into()),
        };
        let a = SshTarget::new("h", 22, "u", key_auth);
        let b = SshTarget::new("h", 22, "u", with_pass);
        assert_ne!(a.key(), b.key());
        assert_ne!(a.key(), SshTarget::new("h", 22, "u", SshAuth::Agent).key());
        // 摘要里不出现明文凭据
        assert!(!auth_digest(&SshAuth::Password("hunter2".into())).contains("hunter2"));
    }

    #[test]
    fn expired_keys_respect_grace_and_users() {
        let key = target("pw").key();
        let shared = bare_shared();
        shared.last_used.store(1_000, Ordering::Relaxed);
        let mut map = HashMap::new();
        map.insert(key.clone(), shared.clone());
        drop(shared); // 只留注册表引用,模拟标签已关闭

        // 刚用过:不清理
        assert!(expired_keys(&map, 1_000 + 5_000, 30_000).is_empty());
        // 超过宽限期:清理
        assert_eq!(
            expired_keys(&map, 1_000 + 40_000, 30_000),
            vec![key.clone()]
        );
        // 仍有标签持有句柄:不清理
        let held = map.get(&key).cloned().expect("注册表条目");
        assert!(expired_keys(&map, 1_000 + 40_000, 30_000).is_empty());
        drop(held);
    }

    #[test]
    fn retry_waits_for_nonblocking_session() {
        let mut calls = 0;
        let v = retry("error.sftp.stat", || {
            calls += 1;
            if calls < 3 {
                Err(ssh2::Error::new(
                    ssh2::ErrorCode::Session(EAGAIN),
                    "operation would block",
                ))
            } else {
                Ok(7u32)
            }
        })
        .expect("EAGAIN 后应重试成功");
        assert_eq!(v, 7);
        assert_eq!(calls, 3);
    }

    #[test]
    fn retry_classifies_file_errors_as_non_fatal() {
        let err = retry::<()>("error.sftp.stat", || {
            Err(ssh2::Error::new(ssh2::ErrorCode::SFTP(2), "no such file"))
        })
        .expect_err("文件不存在应报错");
        assert!(!err.fatal);
        assert!(err.msg.contains("no such file"), "{}", err.msg);

        let err = retry::<()>("error.sftp.stat", || {
            Err(ssh2::Error::new(
                ssh2::ErrorCode::Session(-13),
                "socket disconnect",
            ))
        })
        .expect_err("连接错误应报错");
        assert!(err.fatal, "连接级错误应标记会话失效");
    }

    #[test]
    fn reconnect_without_trust_fails_fast() {
        // 没有首次连接记录时不能静默复连(也就不会凭空信任主机密钥)
        let shared = Arc::new(Shared {
            target: target("pw"),
            state: Mutex::new(State {
                conn: None,
                trust: None,
                last_attempt: None,
                next_generation: 1,
            }),
            last_used: AtomicI64::new(now_ms()),
        });
        let err = shared.ensure_connected().expect_err("缺少信任记录应失败");
        assert_eq!(err, i18n::t("error.ssh.disconnected"));
        assert!(shared.state().conn.is_none());
    }

    #[test]
    fn dead_session_is_not_reused() {
        // 会话被标记失效后,连接槽位为空,下一次操作会走重连分支
        let sess = ssh2::Session::new().expect("创建会话");
        let session = SshSession {
            inner: Arc::new(Shared::new(
                target("pw"),
                sess,
                HostKeyDecision::Accept { remember: false },
            )),
        };
        assert!(session.inner_is_connected());
        session.inner.mark_dead();
        assert!(!session.inner_is_connected());
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
