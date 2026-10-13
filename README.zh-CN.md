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
