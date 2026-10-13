pub static APP_ID: &str = "app.ossgui";
pub static APP_NAME: &str = "Ossgui";

/// Linux 窗口的 app_id（Wayland 的 app_id / X11 的 WM_CLASS）。
///
/// 必须等于打包时生成的桌面文件名。cargo-packager 固定把它命名成
/// `<二进制名>.desktop`，也就是 `ossgui.desktop`，所以这里只能是 `ossgui`，
/// 不能跟着 [`APP_ID`] 走 —— 否则窗口和桌面项关联不上，任务栏图标显示不出来。
pub static APP_DESKTOP_ID: &str = "ossgui";
