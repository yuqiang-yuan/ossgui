use std::sync::OnceLock;

use chrono::{DateTime, Local};

pub fn tokio_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4) // 纯网络等待，1 个 worker 够；CPU 任务多再加
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    })
}

pub enum LoadState {
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

/// AbortHandle 在 drop 时中止对应的 tokio 任务
pub struct AbortOnDrop(pub tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// OSS 返回的是 RFC3339 UTC，例如 "2015-04-29T02:44:22.000Z"
pub fn format_date(raw: &str) -> String {
    match DateTime::parse_from_rfc3339(raw) {
        Ok(dt) => dt
            .with_timezone(&Local)
            .format("%Y-%m-%d")
            .to_string(),
        Err(_) => raw.to_string(), // 解析失败原样显示，别 panic
    }
}

/// OSS 返回的是 RFC3339 UTC，例如 "2015-04-29T02:44:22.000Z"
pub fn format_datetime(raw: &str) -> String {
    match DateTime::parse_from_rfc3339(raw) {
        Ok(dt) => dt
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => raw.to_string(), // 解析失败原样显示，别 panic
    }
}
