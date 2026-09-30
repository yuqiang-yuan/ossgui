use std::fs;

use gpui_kit::Global;
use serde::{Deserialize, Serialize};

use crate::globals::APP_ID;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AppSettings {
    pub is_dark: Option<bool>,
    pub window_width: Option<f32>,
    pub window_height: Option<f32>,
    pub window_maximized: Option<bool>,
    pub font_size: Option<f32>,
}

impl Global for AppSettings {}

impl AppSettings {
    /// Save settings. if save failed, just ignored
    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            if let Some(p) = dirs::config_local_dir() {
                let output_file_path = p.join(APP_ID);
                if (!output_file_path.exists() && fs::create_dir(&output_file_path).is_ok())
                    || (output_file_path.exists())
                {
                    let output_file = output_file_path.join("settings.json");
                    let _ = fs::write(output_file, s);
                }
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
        }
    }
}
