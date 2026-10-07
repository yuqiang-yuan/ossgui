use std::{collections::HashMap, sync::OnceLock};

use chrono::{DateTime, Local};

pub fn tokio_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2) // 纯网络等待，1 个 worker 够；CPU 任务多再加
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    })
}

#[derive(Debug, Clone, Copy)]
pub enum LoadState {
    Idle,
    Loading,
    Loaded,
    Failed,
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

pub fn oss_region_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([
            ("cn-hangzhou", "oss-cn-hangzhou.aliyuncs.com"),
            ("cn-shanghai", "oss-cn-shanghai.aliyuncs.com"),
            ("cn-nanjing", "oss-cn-nanjing.aliyuncs.com"),
            ("cn-fuzhou", "oss-cn-fuzhou.aliyuncs.com"),
            ("cn-wuhan-lr", "oss-cn-wuhan-lr.aliyuncs.com"),
            ("cn-qingdao", "oss-cn-qingdao.aliyuncs.com"),
            ("cn-beijing", "oss-cn-beijing.aliyuncs.com"),
            ("cn-zhangjiakou", "oss-cn-zhangjiakou.aliyuncs.com"),
            ("cn-huhehaote", "oss-cn-huhehaote.aliyuncs.com"),
            ("cn-wulanchabu", "oss-cn-wulanchabu.aliyuncs.com"),
            ("cn-shenzhen", "oss-cn-shenzhen.aliyuncs.com"),
            ("cn-heyuan", "oss-cn-heyuan.aliyuncs.com"),
            ("cn-guangzhou", "oss-cn-guangzhou.aliyuncs.com"),
            ("cn-chengdu", "oss-cn-chengdu.aliyuncs.com"),
            ("cn-zhongwei", "oss-cn-zhongwei.aliyuncs.com"),
            ("cn-hongkong", "oss-cn-hongkong.aliyuncs.com"),
            ("rg-china-mainland", "oss-rg-china-mainland.aliyuncs.com"),
            ("ap-northeast-1", "oss-ap-northeast-1.aliyuncs.com"),
            ("ap-northeast-2", "oss-ap-northeast-2.aliyuncs.com"),
            ("ap-southeast-1", "oss-ap-southeast-1.aliyuncs.com"),
            ("ap-southeast-3", "oss-ap-southeast-3.aliyuncs.com"),
            ("ap-southeast-5", "oss-ap-southeast-5.aliyuncs.com"),
            ("ap-southeast-6", "oss-ap-southeast-6.aliyuncs.com"),
            ("ap-southeast-7", "oss-ap-southeast-7.aliyuncs.com"),
            ("ap-southeast-8", "oss-ap-southeast-8.aliyuncs.com"),
            ("sa-east-1", "oss-sa-east-1.aliyuncs.com"),
            ("eu-central-1", "oss-eu-central-1.aliyuncs.com"),
            ("eu-west-1", "oss-eu-west-1.aliyuncs.com"),
            ("us-west-1", "oss-us-west-1.aliyuncs.com"),
            ("us-east-1", "oss-us-east-1.aliyuncs.com"),
            ("na-south-1", "oss-na-south-1.aliyuncs.com"),
            ("eu-west-2", "oss-eu-west-2.aliyuncs.com"),
            ("me-east-1", "oss-me-east-1.aliyuncs.com"),
            ("cn-hangzhou-finance", "oss-cn-hzfinance.aliyuncs.com"),
            ("cn-shanghai-finance-1", "oss-cn-shanghai-finance-1-pub.aliyuncs.com"),
            ("cn-shenzhen-finance-1", "oss-cn-szfinance.aliyuncs.com"),
            ("cn-beijing-finance-1", "oss-cn-beijing-finance-1-pub.aliyuncs.com"),
            ("cn-north-2-gov-1", "oss-cn-north-2-gov-1.aliyuncs.com"),
        ])
    })
}
