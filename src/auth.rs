//! 凭证管理: 到期预判 + 单飞刷新 + 落盘。

use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::oauth;
use crate::provider::Provider;
use crate::store::{self, Credential};

/// 提前多久刷新 (秒)。留足网络抖动与重试余量。
const REFRESH_MARGIN: u64 = 300;

type Slot = Arc<Mutex<Option<Credential>>>;

pub struct AuthManager {
    http: reqwest::Client,
    anthropic: Slot,
    codex: Slot,
}

impl AuthManager {
    pub fn load(http: reqwest::Client) -> Result<Arc<Self>> {
        let store = store::load()?;
        Ok(Arc::new(Self {
            http,
            anthropic: Arc::new(Mutex::new(store.get(Provider::Anthropic.key()).cloned())),
            codex: Arc::new(Mutex::new(store.get(Provider::Codex.key()).cloned())),
        }))
    }

    fn slot(&self, p: Provider) -> &Slot {
        match p {
            Provider::Anthropic => &self.anthropic,
            Provider::Codex => &self.codex,
        }
    }

    pub async fn snapshot(&self, p: Provider) -> Option<Credential> {
        self.slot(p).lock().await.clone()
    }

    /// 取可用 access token。锁住整段 -> 并发请求只会触发一次刷新。
    ///
    /// `rejected_token` = 上游刚拒绝的 token；若并发请求已刷新，则直接复用新 token。
    pub async fn token(&self, p: Provider, rejected_token: Option<&str>) -> Result<Credential> {
        let mut guard = self.slot(p).clone().lock_owned().await;
        // login/logout 是独立进程；每次请求同步小文件，保证切换账号立即生效。
        *guard = store::load()?.get(p.key()).cloned();
        let cred = logged_in(p, guard.clone())?;
        if !refresh_needed(&cred, rejected_token) {
            return Ok(cred);
        }
        // 刷新跑在独立 task 里, 锁随之移交: 客户端断开只取消这里的等待。
        // 实测坑: 刷新内联在请求 future 里时, 客户端恰在刷新途中断开 -> 上游已轮换 refresh token,
        // 新值却没来得及落盘 -> 下一个请求拿旧 refresh token 去换, 持续 invalid_grant 直到重新 login。
        let http = self.http.clone();
        let rejected = rejected_token.map(str::to_string);
        tokio::spawn(async move { refresh(&http, p, &mut guard, rejected.as_deref()).await })
            .await
            .map_err(|e| anyhow!("{p} token 刷新任务异常: {e}"))?
    }

    /// 登录成功后写盘并热更新内存。
    pub async fn set(&self, p: Provider, cred: Credential) -> Result<()> {
        store::put(p, &cred)?;
        *self.slot(p).lock().await = Some(cred);
        Ok(())
    }
}

async fn refresh(
    http: &reqwest::Client,
    p: Provider,
    guard: &mut OwnedMutexGuard<Option<Credential>>,
    rejected_token: Option<&str>,
) -> Result<Credential> {
    let _cross_process = store::refresh_lock(p).await?;
    // 等锁期间别的进程可能已刷新过 -> 以盘上为准重判一次。
    **guard = store::load()?.get(p.key()).cloned();
    let cred = logged_in(p, guard.clone())?;
    if !refresh_needed(&cred, rejected_token) {
        return Ok(cred);
    }
    let after_unauthorized = rejected_token == Some(cred.access_token.as_str());
    tracing::info!(provider = %p, after_unauthorized, "刷新 access token");
    match oauth::refresh(http, p, &cred).await {
        Ok(fresh) => {
            store::put(p, &fresh)?;
            **guard = Some(fresh.clone());
            Ok(fresh)
        }
        // 提前量内的刷新失败 (网络抖动 / 上游 5xx) 不该让还能用的 token 陪葬。
        Err(e) if !after_unauthorized && !cred.stale(0) => {
            tracing::warn!(provider = %p, error = %e, "刷新 access token 失败, 旧 token 未过期, 继续沿用");
            Ok(cred)
        }
        Err(e) => {
            tracing::warn!(provider = %p, error = %e, "刷新 access token 失败");
            if e.to_string().contains("invalid_grant") {
                return Err(e.context(format!(
                    "{p} refresh token 已失效, 请重新执行 `jj-agentic-proxy login {p}`"
                )));
            }
            Err(e)
        }
    }
}

fn logged_in(p: Provider, cred: Option<Credential>) -> Result<Credential> {
    cred.ok_or_else(|| anyhow!("{p} 未登录: 先执行 `jj-agentic-proxy login {p}`"))
}

fn refresh_needed(cred: &Credential, rejected_token: Option<&str>) -> bool {
    cred.stale(REFRESH_MARGIN)
        || rejected_token.is_some_and(|token| token == cred.access_token.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(token: &str, expires_at: u64) -> Credential {
        Credential {
            access_token: token.into(),
            refresh_token: "refresh".into(),
            expires_at,
            account: None,
            account_id: None,
            plan: None,
        }
    }

    #[test]
    fn unauthorized_refresh_reuses_concurrently_rotated_token() {
        let old = cred("old", u64::MAX);
        assert!(refresh_needed(&old, Some("old")));

        let fresh = cred("fresh", u64::MAX);
        assert!(!refresh_needed(&fresh, Some("old")));
        assert!(!refresh_needed(&fresh, None));
    }

    /// 回归 09-15 事故: 刷新途中调用方被取消, 上游轮换出的新 refresh token 仍须落盘。
    #[tokio::test]
    async fn cancelled_caller_does_not_lose_rotated_refresh_token() {
        use std::io::{Read as _, Write as _};

        let _dir_guard = store::TEST_DIR_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("jj-auth-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        *store::TEST_CONFIG_DIR.lock().unwrap() = Some(dir.clone());

        // 假 token 端点: 收到请求后故意拖 300ms 再回轮换后的 token。
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        *oauth::TOKEN_URL_OVERRIDE.lock().unwrap() = Some(format!("http://{addr}/token"));
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = s.read(&mut [0u8; 4096]);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let body =
                r#"{"access_token":"new-access","refresh_token":"rotated","expires_in":28800}"#;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });

        let mut stale = cred("old-access", 0);
        stale.refresh_token = "original".into();
        store::put(Provider::Anthropic, &stale).unwrap();

        let auth = AuthManager::load(reqwest::Client::new()).unwrap();
        // 调用方 50ms 后放弃 = 客户端断开
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            auth.token(Provider::Anthropic, None),
        )
        .await;

        // 下一个请求等到刷新完成, 拿到的是新 token 而不是再拿旧 refresh token 去换
        let next = auth.token(Provider::Anthropic, None).await.unwrap();
        assert_eq!(next.access_token, "new-access");
        let saved = store::load().unwrap()[Provider::Anthropic.key()].clone();
        assert_eq!(saved.refresh_token, "rotated");

        *oauth::TOKEN_URL_OVERRIDE.lock().unwrap() = None;
        *store::TEST_CONFIG_DIR.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
