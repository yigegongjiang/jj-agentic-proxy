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
        now() + margin >= self.expires_at
    }
}

pub type Store = BTreeMap<String, Credential>;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("JJ_PROXY_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
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

pub fn save(store: &Store) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir).with_context(|| format!("创建目录失败: {}", dir.display()))?;
    let path = auth_path();
    let tmp = dir.join("auth.json.tmp");

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

/// 串行化 read-modify-write, 防止两家 provider 同时刷新时互相覆盖。
static RMW_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 读取 -> 替换单个 provider 条目 -> 写回, 不影响另一家。
pub fn put(provider: Provider, cred: &Credential) -> Result<()> {
    let _guard = RMW_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load()?;
    store.insert(provider.key().to_string(), cred.clone());
    save(&store)
}

/// 返回是否真的删掉了条目。
pub fn remove(provider: Provider) -> Result<bool> {
    let _guard = RMW_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load()?;
    let existed = store.remove(provider.key()).is_some();
    if existed {
        save(&store)?;
    }
    Ok(existed)
}
