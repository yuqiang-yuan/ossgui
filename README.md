# Aliyun OSS GUI Client

[中文](README.zh-CN.md)

An experimental project for learning [GPUI](https://github.com/zed-industries/zed) and [GPUI-Kit](https://github.com/longbridge/gpui-kit). So far it only covers listing, upload, download, delete, and account management.

Why GPUI and GPUI-Kit?

1. As someone who has worked on the Ubuntu desktop for years, GPUI's cross-platform story is appealing.
2. I'm tired of how heavy browser-based GUIs are — Tauri and Electron alike.
3. GPUI-Kit ships a complete kit, including a consistent visual design. That's great. Thanks to Longbridge, and to @huacnlee (Jason Lee).
4. GPUI-Kit iterates fast and is very active. There are breaking changes, but I can live with that.
5. I'm not against AI — but before letting AI write code, I want to understand how to write this framework and these features by hand. Call it pure curiosity.

What I got out of this project:

1. How to use GPUI Kit's common components, and how components talk to each other.
2. How to change the color theme.
3. How to bridge a Tokio runtime under GPUI.
4. How to store secrets cross-platform.
5. How file dialogs differ between platforms.
