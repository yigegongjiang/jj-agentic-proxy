//! 凭证落盘: `~/.config/jj-agentic-proxy/auth.json`, 0600, 原子写。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::provider::Provider;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub access_token: String,
    pub refresh_token: String,
    /// unix 秒
    pub expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Codex: `chatgpt_account_id`; Anthropic: account uuid
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

impl Credential {
    /// 距到期不足 `margin` 秒即视为需要刷新。
    pub fn stale(&self, margin: u64) -> bool {
        now().saturating_add(margin) >= self.expires_at
    }
}

pub type Store = BTreeMap<String, Credential>;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 测试用临时目录; 生产恒为 `None`。改它的测试先持 `TEST_DIR_LOCK`, 免得并行测试互相改指。
#[cfg(test)]
pub(crate) static TEST_DIR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
pub(crate) static TEST_CONFIG_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn config_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG_DIR.lock().unwrap().clone() {
        return dir;
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME")
                .ok()
                .filter(|dir| !dir.is_empty())
                .unwrap_or_else(|| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("jj-agentic-proxy")
}

pub fn auth_path() -> PathBuf {
    config_dir().join("auth.json")
}

pub fn load() -> Result<Store> {
    let path = auth_path();
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("解析凭证文件失败: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::new()),
        Err(e) => Err(e).with_context(|| format!("读取凭证文件失败: {}", path.display())),
    }
}

fn save(store: &Store) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir).with_context(|| format!("创建目录失败: {}", dir.display()))?;
    let path = auth_path();
    let tmp = dir.join(format!("auth.json.tmp.{}", std::process::id()));

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("写入临时文件失败: {}", tmp.display()))?;
    f.write_all(&serde_json::to_vec_pretty(store)?)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &path).with_context(|| format!("落盘失败: {}", path.display()))?;
    Ok(())
}

/// 跨进程串行化同一 provider 的 token 刷新, 返回值持有即持锁 (drop = 关 fd = 释放)。
///
/// 后台 daemon 与 `models` 等 CLI 子进程各有一把进程内锁, 互相看不见:
/// 两边同时拿同一个 refresh token 去换, 上游轮换后后到的那份作废。
/// 与 `auth.lock` 分开: 刷新持锁期间要 `put` 落盘, flock 按打开的文件描述计, 同一把会自锁。
pub async fn refresh_lock(provider: Provider) -> Result<fs::File> {
    let path = config_dir().join(format!("refresh-{}.lock", provider.key()));
    tokio::task::spawn_blocking(move || -> Result<fs::File> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut opts = fs::OpenOptions::new();
        opts.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let lock = opts
            .open(&path)
            .with_context(|| format!("打开刷新锁失败: {}", path.display()))?;
        fs4::FileExt::lock(&lock).with_context(|| format!("锁定刷新锁失败: {}", path.display()))?;
        Ok(lock)
    })
    .await
    .context("刷新锁任务异常")?
}

/// 串行化 read-modify-write, 防止两家 provider 同时刷新时互相覆盖。
static RMW_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn update<T>(change: impl FnOnce(&mut Store) -> Result<(T, bool)>) -> Result<T> {
    let _guard = RMW_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = config_dir();
    fs::create_dir_all(&dir).with_context(|| format!("创建目录失败: {}", dir.display()))?;
    let lock_path = dir.join("auth.lock");
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let lock = opts
        .open(&lock_path)
        .with_context(|| format!("打开凭证锁失败: {}", lock_path.display()))?;
    fs4::FileExt::lock(&lock)
        .with_context(|| format!("锁定凭证文件失败: {}", lock_path.display()))?;

    let mut store = load()?;
    let (result, changed) = change(&mut store)?;
    if changed {
        save(&store)?;
    }
    Ok(result)
}

/// 读取 -> 替换单个 provider 条目 -> 写回, 不影响另一家。
pub fn put(provider: Provider, cred: &Credential) -> Result<()> {
    update(|store| {
        store.insert(provider.key().to_string(), cred.clone());
        Ok(((), true))
    })
}

/// 返回是否真的删掉了条目。
pub fn remove(provider: Provider) -> Result<bool> {
    update(|store| {
        let existed = store.remove(provider.key()).is_some();
        Ok((existed, existed))
    })
}
