use gpui_fps::fps_monitor;
use gpui_kit::{
    App, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Menu, MenuItem, ParentElement, Render, Styled, Window, base::{Placement, StyledExt}, component::{
        ActiveTheme, IconName, Theme, ThemeMode, TitleBar, button::{Button, ButtonVariants}, menu::{AppMenuBar, DropdownMenu, PopupMenuItem}, status_bar::StatusBar,
    }, div, prelude::FluentBuilder, px,
};

use crate::actions::{AboutAction, QuitAction};

pub struct MainView {
    focus_handle: FocusHandle,
    menubar: Entity<AppMenuBar>,
    show_fps: bool,
}

impl MainView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        #[cfg(target_os = "macos")]
        {
            cx.set_menus(build_menus());
        }

        #[cfg(not(target_os = "macos"))]
        {
            use gpui_kit::base::GlobalState;

            let menus = build_menus().into_iter().map(|menu| menu.owned()).collect();
            GlobalState::global_mut(cx).set_app_menus(menus);
        }

        Self {
            focus_handle,
            menubar: AppMenuBar::new(cx),
            show_fps: true,
        }
    }
}

impl Render for MainView {
    fn render(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::prelude::Context<Self>,
    ) -> impl gpui_kit::prelude::IntoElement {
        div()
            .id("main-view")
            .relative()
            .track_focus(&self.focus_handle)
            .size_full()
            .v_flex()
            .child(
                TitleBar::new().child(
                    div()
                        .size_full()
                        .h_flex()
                        .child(self.menubar.clone())
                        .child(div().flex_grow_1())
                        .child(font_size_button())
                        .child(theme_button(cx)),
                ),
            )
            .child(
                div()
                .size_full()
                .relative()
                .when(self.show_fps, |this| this.child(fps_monitor(window, cx)))
            )
            .child(StatusBar::new().left("Ready"))
    }
}

/// Build the application menu
fn build_menus() -> Vec<Menu> {
    vec![
        #[cfg(target_os = "macos")]
        {
            Menu {
                name: "Erydian".into(),
                items: vec![
                    MenuItem::action("About", AboutAction),
                    MenuItem::separator(),
                    MenuItem::action("Quit", QuitAction),
                ],
                disabled: false,
            }
        },
        Menu {
            name: "File".into(),
            items: vec![MenuItem::action("Quit", QuitAction)],
            disabled: false,
        },
        Menu {
            name: "Help".into(),

            items: vec![MenuItem::action("About", AboutAction)],
            disabled: false,
        },
    ]
}

/// Add a button to switch theme (dark/light)
fn theme_button(cx: &mut App) -> impl IntoElement {
    Button::new("theme-button")
        .ghost()
        .rounded_none()
        .icon(if cx.theme().is_dark() {
            IconName::Moon
        } else {
            IconName::Sun
        })
        .tooltip(if cx.theme().is_dark() {
            "Swith to light"
        } else {
            "Switch to dark"
        })
        .tooltip_placement(Placement::Bottom)
        .on_click(|_, window, cx| {
            let target_mode = if cx.theme().is_dark() {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };

            Theme::change(target_mode, Some(window), cx);
            cx.refresh_windows();
        })
}

/// Add a font size button
fn font_size_button() -> impl IntoElement {
    Button::new("font-size-button")
        .ghost()
        .rounded_none()
        .icon(IconName::ALargeSmall)
        .tooltip("Change font size")
        .tooltip_placement(Placement::Bottom)
        .dropdown_menu(|menu, _, cx| {
            let current_font_size = cx.theme().font_size.as_f32().round() as i32;

            menu.item(
                PopupMenuItem::new("Small")
                    .checked(current_font_size == 14)
                    .on_click(|_, _, cx| {
                        Theme::global_mut(cx).font_size = px(14.0);
                        Theme::sync_base(cx);
                        cx.refresh_windows();
                    }),
            )
            .item(
                PopupMenuItem::new("Regular")
                    .checked(current_font_size == 16)
                    .on_click(|_, _, cx| {
                        Theme::global_mut(cx).font_size = px(16.0);
                        Theme::sync_base(cx);
                        cx.refresh_windows();
                    }),
            )
            .item(
                PopupMenuItem::new("Large")
                    .checked(current_font_size == 18)
                    .on_click(|_, _, cx| {
                        Theme::global_mut(cx).font_size = px(18.0);
                        Theme::sync_base(cx);
                        cx.refresh_windows();
                    }),
            )
        })
}
