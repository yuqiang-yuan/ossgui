# Aliyun OSS 图形客户端

[English](README.md)

本项目是学习 GPUI、[GPUI-Kit](https://github.com/longbridge/gpui-kit) 的实验项目，目前仅完成了查询、上传、下载、删除和账号管理。

为什么要学习 GPUI 和 GPUI-Kit 呢？有这么几个原因吧：

1. 对于我这个长期在 Ubuntu 桌面工作的人而言，GPUI 跨平台的能力很有吸引力
2. 厌倦了基于浏览器（无论是 Tauri 还是 Electron ）图形界面的笨重
3. GPUI-Kit 提供了完整的套件，包括统一的视觉设计，这个很棒。感谢 Longbridge，感谢 @huacnlee （Jason Lee）
4. GPUI-Kit 迭代频繁，非常活跃。虽然还会有不兼容更新，但是我觉得可以接收
5. 我不拒绝 AI，但是在用 AI 写代码之前，我要搞懂这样的框架和功能，古法编程怎么写，可以理解为纯粹的好奇心

做完这个项目的收获：

1. 如何使用 GPUI Kit 的常用组件，以及组件间通信
2. 如何修改配色主题
3. 如何在 GPUI 下桥接 Tokio 运行时
4. 如何跨平台保存敏感信息
5. 不同平台文件选择对话框的差异
6. 如何给桌面应用做三平台打包

## 下载

安装包由 [Package](.github/workflows/package.yml) 这个 workflow 产出，在 Actions 页面手动触发。

| 平台 | 产物 | 架构 |
| --- | --- | --- |
| macOS | `Ossgui_*.dmg` | Apple Silicon |
| Windows | `ossgui.exe`，免安装，双击就能跑 | x64 |
| Linux | `.deb` / `.rpm` / `.AppImage` | x64 和 arm64 |

### macOS：没有签名

dmg 既没签名也没公证，所以 Gatekeeper 会拦住第一次打开。**右键点应用 → 打开**并确认，或者清掉隔离标记：

```sh
xattr -cr /Applications/Ossgui.app
```

## 打包

用到两个工具：

- [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) 负责 `.deb`、`.AppImage`、`.dmg` —— 配置在 `Cargo.toml` 的 `[package.metadata.packager]`
- [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm) 负责 `.rpm`，因为 cargo-packager 不支持 RPM

Windows 两个都不用：免安装版就是 `target/release/ossgui.exe` 本身。图标和 manifest 由 `build.rs` 嵌进去，`.cargo/config.toml` 开了 `+crt-static`，所以不需要用户装 VC++ 运行库。

本地构建（Linux）：

```sh
cargo install cargo-packager cargo-generate-rpm --locked
cargo packager --release --formats deb,appimage
cargo generate-rpm -o dist
```

要出包就在 Actions 页面手动跑 **Package**，然后下载 artifact。

### 图标

`packaging/icons/` 是从 `packaging/app-icon.svg` 生成的：

```sh
./scripts/gen-icons.sh
```

生成的文件会提交进仓库，所以 CI 不用装图像工具链。

### 两个值得知道的点

- Linux 的 job 特意跑在 Ubuntu 22.04 上。构建机的 glibc 版本会写进包的元数据（rpm 的 requires 里能看到 `libm.so.6(GLIBC_2.43)`），在 24.04 上构建出来的包只能装在 24.04 及以上。
- TLS 后端用的是 rustls 而不是默认的 native-tls。这是 `libssl`/`libcrypto` 能不进运行时依赖的原因——否则包会被钉死在 OpenSSL 3 的发行版上。
