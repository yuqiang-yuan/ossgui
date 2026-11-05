//! 版本更新检查。
//!
//! 问一次 GitHub 的 releases API，发现比本地新的正式版就提示用户 ——
//! 只做"提示 + 跳转"，不下载、不安装、不重启。
//!
//! 自动检查（启动时）24 小时最多一次，失败静默；手动检查不受节流限制，
//! 结果无论有没有更新都会给个反馈。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// 提示里点一下就跳这里（GitHub 会重定向到最新 tag 的页面）
pub const RELEASES_URL: &str = "https://github.com/yuqiang-yuan/ossgui/releases/latest";

const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/yuqiang-yuan/ossgui/releases/latest";

/// 自动检查的最小间隔
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 启动后等一会儿再查，别和首屏的 bucket 加载抢
pub const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// 单次请求超时 —— 手动检查时用户就在那儿等着，别拖太久
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 本地版本，构建时从 Cargo.toml 写死
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 当前时间（unix 秒），给 24 小时节流用
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    /// 预发布版（-rc / -beta）不提示
    #[serde(default)]
    prerelease: bool,
    /// 草稿正常拿不到，顺手挡一下
    #[serde(default)]
    draft: bool,
}

/// 问一次 GitHub：`Some(版本号)` = 有比本地新的正式版，`None` = 已经是最新。
/// 网络失败、限流、解析失败都是 Err，由调用方决定要不要吭声
pub async fn fetch_newer_version() -> anyhow::Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // GitHub API 强制要求 User-Agent，不带直接 403
        .user_agent(format!("ossgui/{}", current_version()))
        .build()?;

    let response = client
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;

    // 仓库一个 Release 都还没发时是 404 —— 那是"没有更新"，不是错误
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let body = response.error_for_status()?.text().await?;

    let release: Release =
        serde_json::from_str(&body).map_err(|e| anyhow::anyhow!("bad release json: {e}"))?;

    if release.prerelease || release.draft {
        return Ok(None);
    }

    Ok(newer_version(current_version(), &release.tag_name))
}

/// 远端 tag 比本地新就返回去掉 `v` 前缀的版本号
fn newer_version(local: &str, tag: &str) -> Option<String> {
    let remote = tag.trim().trim_start_matches('v');

    is_newer(local, remote).then(|| remote.to_string())
}

/// 逐段比大小：只认 `x.y.z` 这种数字段，缺的段按 0 算
/// （所以 `0.3` 和 `0.3.0` 相等，`0.3` > `0.2.9`）。
///
/// 用 semver crate 更严谨，但这里只需要比大小，不值得多一个依赖
fn is_newer(local: &str, remote: &str) -> bool {
    let parse = |version: &str| -> Vec<u64> {
        version
            // 预发布后缀不参与比较
            .split(['-', '+'])
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };

    let (local, remote) = (parse(local), parse(remote));

    for ix in 0..local.len().max(remote.len()) {
        let (l, r) = (
            local.get(ix).copied().unwrap_or(0),
            remote.get(ix).copied().unwrap_or(0),
        );
        if l != r {
            return r > l;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{is_newer, newer_version};

    #[test]
    fn compares_versions_by_segment() {
        assert!(is_newer("0.2.0", "0.2.1"));
        assert!(is_newer("0.2.0", "0.3.0"));
        assert!(is_newer("0.2.9", "0.3"));
        assert!(is_newer("0.9.0", "1.0.0"));

        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.3", "0.3.0"));
    }

    #[test]
    fn ignores_prerelease_suffix() {
        // 同号的预发布版不算更新
        assert!(!is_newer("0.2.0", "0.2.0-rc1"));
        assert!(is_newer("0.1.0", "0.2.0-rc1"));
    }

    #[test]
    fn strips_v_prefix_from_tag() {
        assert_eq!(newer_version("0.2.0", "v0.2.1"), Some("0.2.1".to_string()));
        assert_eq!(newer_version("0.2.0", "v0.2.0"), None);
    }
}
