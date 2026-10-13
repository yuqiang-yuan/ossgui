//! 给 Windows 的 exe 嵌入图标和版本信息。
//!
//! 其他平台这个脚本是空操作 —— `winresource` 只在
//! `[target.'cfg(windows)'.build-dependencies]` 里，而且资源编译器 rc.exe
//! 也只有 Windows SDK 才有。
//!
//! **不要在这里嵌 manifest。** gpui-pre 的 `windows-manifest` feature 是默认
//! 开启的，它自己的 build.rs 已经通过 `resources/windows/gpui.rc` 嵌了一份，
//! 里面带 PerMonitorV2 DPI 感知、SegmentHeap、supportedOS 和 Common-Controls v6。
//! 再嵌一份会让 CVTRES 报 `CVT1100: duplicate resource. type:MANIFEST`，
//! 链接直接失败。

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
