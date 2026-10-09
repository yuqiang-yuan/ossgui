use std::sync::Arc;

use ali_oss_rs::Client;
use gpui_fps::fps_monitor;
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Menu, MenuItem, ParentElement, Render, Styled, Subscription, Window, assets::IconName, base::{Placement, StyledExt, resizable_panel}, component::{
        ActiveTheme, Sizable, Theme, ThemeMode, TitleBar,
        button::{Button, ButtonVariants},
        h_resizable,
        menu::{AppMenuBar, DropdownMenu, PopupMenuItem},
        status_bar::StatusBar,
    }, div, prelude::FluentBuilder, px,
};

use crate::{
    actions::{AboutAction, QuitAction}, bucket_view::BucketListPanel, job::{JobKind, JobPanel, JobQueue, JobsSummary}, object_view::ObjectListPanel, settings::AppSettings,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scene {
    Buckets,
    Objects,
}

pub struct MainView {
    focus_handle: FocusHandle,
    menubar: Entity<AppMenuBar>,
    show_fps: bool,
    bucket_list_panel: Entity<BucketListPanel>,
    object_list_panel: Option<Entity<ObjectListPanel>>,
    bucket_name: Option<String>,
    scene: Scene,
    ossclient: Arc<Client>,

    job_queue: Entity<JobQueue>,
    job_panel: Entity<JobPanel>,
    jobs_summary: JobsSummary,
    _subs: Vec<Subscription>,
    jobs_open: bool,
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

        let ossclient = Arc::new(Client::from_env());
        let this_weak = cx.weak_entity();

        let job_queue = cx.new(|cx| JobQueue::new(cx));
        let job_panel = cx.new(|cx| JobPanel::new(job_queue.clone(), window, cx));

        let job_sub = cx.observe(&job_queue, |this, entity, cx| {
            println!("job queue changed");
            let next = entity.read(cx).summary();
            if next != this.jobs_summary {
                this.jobs_summary = next;
                cx.notify();
            }
        });

        Self {
            focus_handle,
            menubar: AppMenuBar::new(cx),
            show_fps: false,
            bucket_list_panel: cx
                .new(|cx| BucketListPanel::new(this_weak.clone(), ossclient.clone(), window, cx)),
            object_list_panel: None,
            bucket_name: None,
            scene: Scene::Buckets,
            job_queue,
            job_panel,
            ossclient,
            jobs_summary: JobsSummary::default(),
            jobs_open: false,
            _subs: vec![job_sub],
        }
    }

    pub fn goto_object_list(
        &mut self,
        bucket_name: String,
        region: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        println!("Going to bucket: {}", bucket_name);
        self.bucket_name = Some(bucket_name.clone());
        self.scene = Scene::Objects;
        let this_weak = cx.weak_entity();
        let ossclient = self.ossclient.clone();
        self.object_list_panel = Some(cx.new(|cx| {
            ObjectListPanel::new(
                this_weak.clone(),
                ossclient,
                bucket_name,
                region,
                window,
                cx,
            )
        }));
        cx.notify();
    }

    pub fn goto_bucket_list(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.bucket_name = None;
        self.scene = Scene::Buckets;
        self.object_list_panel = None;
        cx.notify();
    }

    fn jobs_summary_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = &self.jobs_summary;
        let (label, color) = match () {
            _ if s.total == 0 => ("Transfers".to_string(), cx.theme().muted_foreground),
            _ if s.failed > 0 => (
                format!("{} running · {} failed", s.running, s.failed),
                cx.theme().danger,
            ),
            _ => (
                format!("{} running · {} queued", s.running, s.queued),
                cx.theme().foreground,
            ),
        };

        Button::new("jobs-summary")
            .ghost()
            .small()
            .icon(IconName::ArrowUpDown)
            .label(label)
            .text_color(color)
            .on_click(cx.listener(|this, _, _, cx| {
                this.jobs_open = !this.jobs_open;
                cx.notify();
            }))
    }

    pub fn enqueue_job(&mut self, kind: JobKind, cx: &mut Context<Self>) {
        let client = self.ossclient.clone();
        self.job_queue.update(cx, |state, cx| {
            state.enqueue(kind, client, cx);
        });
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .h_flex()
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .h_full()
                            .bg(cx.theme().secondary.opacity(0.75))
                            .border_r_1()
                            .border_color(cx.theme().border),
                    )
                    .child(
                        h_resizable("h-resizable")
                            .child(
                                resizable_panel().child(
                                    div()
                                        .min_w_0()
                                        .size_full()
                                        .when(matches!(self.scene, Scene::Buckets), |div| {
                                            div.p_2().child(self.bucket_list_panel.clone())
                                        })
                                        .when(matches!(self.scene, Scene::Objects), |d| {
                                            d.child(match self.object_list_panel.clone() {
                                                Some(panel) => panel.into_any_element(),
                                                None => {
                                                    div().child("No objects").into_any_element()
                                                }
                                            })
                                        }),
                                ),
                            )
                            .child(
                                resizable_panel()
                                    .size(px(300.0))
                                    .size_range(px(280.0)..px(400.0))
                                    .visible(self.jobs_open)
                                    .child(self.job_panel.clone()),
                            ),
                    )
                    .when(self.show_fps, |this| this.child(fps_monitor(window, cx))),
            )
            .child(StatusBar::new().left(self.jobs_summary_button(cx)))
    }
}

/// Build the application menu
fn build_menus() -> Vec<Menu> {
    vec![
        #[cfg(target_os = "macos")]
        {
            Menu {
                name: "Ossgui".into(),
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
            cx.global_mut::<AppSettings>().is_dark = Some(target_mode == ThemeMode::Dark);
            cx.global_mut::<AppSettings>().save();

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
