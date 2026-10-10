mod actions;
mod assets;
mod bucket_view;
mod common;
mod globals;
mod main_view;
mod object_view;
mod settings;
mod job;

#[cfg(target_os = "linux")]
use gpui_kit::WindowDecorations;
use gpui_kit::{
    AppContext, WindowBounds, WindowKind, WindowOptions,
    component::{Theme, ThemeMode, ThemeRegistry, TitleBar},
    px, size,
};
use reqwest_client::ReqwestClient;

use crate::{assets::AppAssets, globals::APP_ID, main_view::MainView, settings::AppSettings};

fn main() {
    dotenvy::dotenv().ok();
    let app = gpui_kit::application()
        .with_assets(AppAssets)
        .with_http_client(std::sync::Arc::new(ReqwestClient::new()));
    app.run(|cx| {
        gpui_kit::init(cx);
        cx.set_app_identity(APP_ID, "Ossgui");

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
            cx.update(move |cx| {
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

                    #[cfg(target_os = "linux")]
                    window_decorations: Some(WindowDecorations::Client),
                    ..TitleBar::window_options()
                };

                let (_, _) = gpui_kit::open_window(options, cx, |window, cx| {
                    cx.new(|cx| MainView::new(window, cx))
                })
                .expect("Launch application failed");

                cx.activate(true);
            });
        })
        .detach();
    });
}
