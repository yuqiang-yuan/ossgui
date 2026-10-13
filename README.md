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
6. How to package a desktop app for macOS, Windows and Linux.

## Download

Prebuilt packages come from the [Package](.github/workflows/package.yml) workflow, triggered manually from the Actions tab.

| Platform | Package | Architecture |
| --- | --- | --- |
| macOS | `Ossgui_*.dmg` | Apple Silicon |
| Windows | `ossgui.exe` — portable, just run it | x64 |
| Linux | `.deb` / `.rpm` / `.AppImage` | x64 and arm64 |

### macOS: the app is not signed

The dmg is neither signed nor notarized, so Gatekeeper will refuse the first launch. Either **right-click the app → Open** and confirm, or clear the quarantine flag:

```sh
xattr -cr /Applications/Ossgui.app
```

## Packaging

Two tools do the work:

- [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) for `.deb`, `.AppImage` and `.dmg` — configured in `Cargo.toml` under `[package.metadata.packager]`
- [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm) for `.rpm`, since cargo-packager has no RPM support

Windows uses neither: the portable exe *is* `target/release/ossgui.exe`. `build.rs` embeds the icon and manifest, and `.cargo/config.toml` enables `+crt-static` so it doesn't need the VC++ redistributable.

To build locally (Linux):

```sh
cargo install cargo-packager cargo-generate-rpm --locked
cargo packager --release --formats deb,appimage
cargo generate-rpm -o dist
```

To release, run the **Package** workflow manually from the Actions tab and download the artifacts.

### Icons

`packaging/icons/` is generated from `packaging/app-icon.svg`:

```sh
./scripts/gen-icons.sh
```

The generated files are committed, so CI doesn't need an image toolchain.

### Two things worth knowing

- The Linux jobs run on Ubuntu 22.04 on purpose. The build machine's glibc version ends up in the package metadata (you can see `libm.so.6(GLIBC_2.43)` in the rpm requires), so building on 24.04 would produce packages that only install on 24.04 and newer.
- The TLS backend is rustls instead of the default native-tls. That's what keeps `libssl`/`libcrypto` out of the runtime dependencies — otherwise the packages would be tied to OpenSSL 3 distros.
