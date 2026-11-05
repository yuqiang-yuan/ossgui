use std::{
    collections::{HashMap, HashSet},
    fs,
};

use gpui_kit::Global;
use serde::{Deserialize, Serialize};

use crate::globals::APP_ID;

/// `serde(default)`：老配置文件里缺字段（新版本加的）时按 Default 补，
/// 而不是整个反序列化失败 —— 那样会把用户已存的窗口大小、列宽一起清掉
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub is_dark: Option<bool>,
    pub window_width: Option<f32>,
    pub window_height: Option<f32>,
    pub window_maximized: Option<bool>,
    pub font_size: Option<f32>,
    /// bucket 表用户调整过的列宽：列 key → 宽度（px）。
    /// 按 key 存，列增删后老宽度还能对上
    pub bucket_column_widths: Option<HashMap<String, f32>>,
    /// object 表同上
    pub object_column_widths: Option<HashMap<String, f32>>,
    /// bucket 表被隐藏的列（Columns 按钮）。空/None = 全显示。
    /// 存列 key 不存下标，列增删后老配置还能对上
    pub bucket_hidden_columns: Option<HashSet<String>>,
    /// 上次检查更新的时间（unix 秒）。自动检查 24 小时最多一次
    pub last_update_check: Option<u64>,
    /// 已经弹过提示的版本号 —— 同一个版本只提醒一次，不然每次启动都弹
    pub last_notified_version: Option<String>,
}

impl Global for AppSettings {}

impl AppSettings {
    /// Save settings. if save failed, just ignored
    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string(self)
            && let Some(p) = dirs::config_local_dir() {
                let output_file_path = p.join(APP_ID);
                if (!output_file_path.exists() && fs::create_dir(&output_file_path).is_ok())
                    || (output_file_path.exists())
                {
                    let output_file = output_file_path.join("settings.json");
                    let _ = fs::write(output_file, s);
                }
            }
    }

    // Load settings. if load failed, return the default
    pub fn load() -> Self {
        if let Some(p) = dirs::config_local_dir() {
            let file_path = p.join(APP_ID).join("settings.json");
            if file_path.exists()
                && let Ok(s) = fs::read_to_string(file_path)
                && let Ok(settings) = serde_json::from_str::<AppSettings>(&s)
            {
                settings
            } else {
                Self::default()
            }
        } else {
            Self::default()
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            is_dark: Some(true),
            window_maximized: Some(false),
            window_width: Some(1200.0),
            window_height: Some(800.0),
            font_size: Some(16.0),
            bucket_column_widths: None,
            object_column_widths: None,
            bucket_hidden_columns: None,
            last_update_check: None,
            last_notified_version: None,
        }
    }
}
