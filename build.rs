//! 给 Windows 的 exe 嵌入图标、版本信息和 manifest。
//!
//! 其他平台这个脚本是空操作 —— `winresource` 只在
//! `[target.'cfg(windows)'.build-dependencies]` 里，而且资源编译器 rc.exe
//! 也只有 Windows SDK 才有。

fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=packaging/icons/icon.ico");
        println!("cargo:rerun-if-changed=build.rs");

        let mut res = winresource::WindowsResource::new();

        res.set_icon("packaging/icons/icon.ico");
        res.set("ProductName", "Ossgui");
        res.set("FileDescription", "Aliyun OSS desktop client");
        res.set("CompanyName", "Yuan YQ");
        res.set("LegalCopyright", "Copyright (c) 2026 Yuan YQ");
        res.set("OriginalFilename", "ossgui.exe");

        let v = version_u64();
        res.set_version_info(winresource::VersionInfo::FILEVERSION, v);
        res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, v);

        // 这个 manifest 是必须的，不是为了好看：
        //
        // gpui-pre-windows 用 GetDpiForWindow() 拿显示器 DPI 来算缩放比，
        // 也处理 WM_DPICHANGED。但进程如果不声明 DPI 感知，Windows 会把
        // 这个 API 的返回值固定成 96 —— 于是 gpui 按 1.0 倍渲染，
        // 系统再把整个窗口位图拉伸到实际 DPI，高分屏上就是一片模糊。
        //
        // PerMonitorV2 让进程拿到真实 DPI，多显示器之间拖动时也能正确切换。
        res.set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Ossgui" version="1.0.0.0"/>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
    </windowsSettings>
  </application>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#,
        );

        res.compile()
            .expect("嵌入 Windows 资源失败（需要 Windows SDK 的 rc.exe）");
    }
}

/// 把 `CARGO_PKG_VERSION` 编码成 Windows 版本资源要的 u64：
/// `MAJOR << 48 | MINOR << 32 | PATCH << 16 | BUILD`
///
/// 从环境变量读而不是写死，避免版本号两处不一致。
/// 预发布后缀（`0.2.0-beta.1`）会被忽略，只取前三个数字。
#[cfg(windows)]
fn version_u64() -> u64 {
    let raw = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut parts = raw
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u16>().ok());

    let major = parts.next().unwrap_or(0) as u64;
    let minor = parts.next().unwrap_or(0) as u64;
    let patch = parts.next().unwrap_or(0) as u64;

    (major << 48) | (minor << 32) | (patch << 16)
}
