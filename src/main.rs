mod actions;
mod assets;
mod bucket_view;
mod common;
mod globals;
mod main_view;
mod object_view;
mod settings;
mod job;
mod account;

#[cfg(target_os = "linux")]
use gpui_kit::WindowDecorations;
use gpui_kit::{
    App, AppContext, WindowBounds, WindowKind, WindowOptions,
    component::{Theme, ThemeMode, ThemeRegistry, TitleBar},
    px, size,
};
use reqwest_client::ReqwestClient;

use crate::{
    assets::AppAssets,
    globals::{APP_DESKTOP_ID, APP_ID, APP_NAME},
    main_view::MainView,
    settings::AppSettings,
};

fn main() {
    dotenvy::dotenv().ok();
    let app = gpui_kit::application()
        .with_assets(AppAssets)
        .with_http_client(std::sync::Arc::new(ReqwestClient::new()));

    // macOS 上关掉窗口后应用还活着（Dock 图标还在），点 Dock 图标时
    // 系统回调 applicationShouldHandleReopen:，GPUI 转成这个 on_reopen。
    // 它只在"一个窗口都没有"时触发，所以不会和已有窗口打架。
    app.on_reopen(open_main_window);

    app.run(|cx| {
        gpui_kit::init(cx);
        cx.set_app_identity(APP_ID, APP_NAME);

        let settings = AppSettings::load();

        let my_theme = include_str!("../assets/themes/hybrid.json");
        let my_theme_light_name = "Hybrid Light";
        let my_theme_dark_name = "Hybrid Dark";
        {
            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(my_theme)
                .ok();
        }

        // Get both configs out of the registry
        let my_theme_light = ThemeRegistry::global(cx)
            .themes()
            .get(my_theme_light_name)
            .cloned();
        let my_theme_dark = ThemeRegistry::global(cx)
            .themes()
            .get(my_theme_dark_name)
            .cloned();

        {
            if let Some(light) = my_theme_light {
                Theme::global_mut(cx).light_theme = light;
            }
            if let Some(dark) = my_theme_dark {
                Theme::global_mut(cx).dark_theme = dark;
            }
        }

        if let Some(true) = settings.is_dark {
            Theme::change(ThemeMode::Dark, None, cx);
        } else {
            Theme::change(ThemeMode::Light, None, cx);
        }

        if let Some(font_size) = settings.font_size {
            Theme::global_mut(cx).font_size = px(font_size);
        }

        cx.set_global(settings);

        cx.on_app_quit(|cx| {
            // 注意顺序：gpui 的 shutdown 是「先同步跑观察者的函数体 → 再清空窗口
            // → 才置 quitting」，返回的 future 是之后才 await 的。
            // 所以窗口信息必须在这里读完，挪进 async 块就拿不到了。
            if let Some(handle) = cx.windows().first().copied()
                && let Ok((size, maximized)) = cx.update_window(handle, |_, window, _| {
                    (window.bounds().size, window.is_maximized())
                })
            {
                let settings = cx.global_mut::<AppSettings>();
                settings.window_maximized = Some(maximized);

                // 最大化时 bounds 就是屏幕大小，存进去会把「还原后的尺寸」冲掉，
                // 所以只在非最大化时记宽高
                if !maximized {
                    settings.window_width = Some(size.width.as_f32());
                    settings.window_height = Some(size.height.as_f32());
                }
            }

            println!("saving settings before quit");
            cx.global::<AppSettings>().save();
            async {}
        })
        .detach();

        cx.spawn(async move |cx| {
            cx.update(open_main_window);
        })
        .detach();
    });
}

/// 打开主窗口。
///
/// macOS 上点关闭按钮只是把窗口移除，应用本身不退出（系统惯例，退出走 ⌘Q）。
/// 用户再点 Dock 图标时 GPUI 会回调 [`Application::on_reopen`]，那里也走这个
/// 函数把窗口重新开出来 —— 所以窗口的创建逻辑必须能重复执行。
fn open_main_window(cx: &mut App) {
    // on_reopen 本身只在"一个窗口都没有"时才触发，这里再兜一层，
    // 免得以后加了别的入口意外开出第二个窗口。
    if !cx.windows().is_empty() {
        cx.activate(true);
        return;
    }

    let settings = *cx.global::<AppSettings>();
    let is_max = settings.window_maximized.unwrap_or(false);

    // TODO: Test if the window bounds over the screen's bounds.
    let bounds = WindowBounds::centered(
        size(
            px(settings.window_width.unwrap_or(1200.0)),
            px(settings.window_height.unwrap_or(800.0)),
        ),
        cx,
    );

    let options = WindowOptions {
        window_bounds: if is_max {
            Some(WindowBounds::Maximized(bounds.get_bounds()))
        } else {
            Some(bounds)
        },
        kind: WindowKind::Normal,

        // 桌面环境靠这个把窗口和 ossgui.desktop 关联起来。
        // 注意 cx.set_app_identity() 在 Linux 上是被忽略的
        // （gpui-pre-linux 只拿它的 name 去做系统通知），
        // 真正生效的是这个字段。
        app_id: Some(APP_DESKTOP_ID.to_string()),

        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        ..TitleBar::window_options()
    };

    let (_, _) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| MainView::new(window, cx))
    })
    .expect("Launch application failed");

    cx.activate(true);
}
