use std::sync::Arc;

use ali_oss_rs::Client;
use gpui_fps::fps_monitor;
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Menu, MenuItem,
    ParentElement, Render, Styled, Subscription, Window,
    assets::IconName,
    base::{Placement, StyledExt, resizable_panel},
    component::{
        ActiveTheme, Sizable, Theme, ThemeMode, TitleBar,
        WindowExt,
        button::{Button, ButtonVariant, ButtonVariants},
        h_resizable,
        menu::{AppMenuBar, DropdownMenu, PopupMenuItem},
        status_bar::StatusBar,
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::{
    actions::{AboutAction, QuitAction},
    bucket_view::BucketListPanel,
    common::format_file_size,
    job::{JobKind, JobPanel, JobQueue, JobsSummary, TransferSpeed},
    object_view::ObjectListPanel,
    settings::AppSettings,
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
    /// 整体传输速率（字节/秒），由 JobQueue 每秒重算
    transfer_speed: TransferSpeed,
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

        // 队列每次 notify 都会走到这里（包括每秒一次的速率重算），
        // 只有"状态栏上会显示的东西"变了才真的重绘
        // 窗口管理器要求关闭（Alt+F4 / 任务栏）时也能拦下来问一句。
        // 注意这条只覆盖 WM 的关闭请求，标题栏的 X 按钮走的是 remove_window，
        // 要单独用 TitleBar::on_close_window 接管。
        window.on_window_should_close(cx, {
            let this_weak = cx.weak_entity();
            move |window, cx| {
                let busy = this_weak
                    .update(cx, |main_view, cx| main_view.has_pending_jobs(cx))
                    .unwrap_or(false);

                if !busy {
                    return true; // 没任务，放行
                }

                // 有任务：拦下这次关闭，弹确认框（确认后走 cx.quit()）
                this_weak
                    .update(cx, |main_view, cx| main_view.request_quit(window, cx))
                    .ok();
                false
            }
        });

        let job_sub = cx.observe(&job_queue, |this, entity, cx| {
            let (summary, speed) = {
                let queue = entity.read(cx);
                (queue.summary(), queue.speed())
            };

            if summary != this.jobs_summary || speed != this.transfer_speed {
                this.jobs_summary = summary;
                this.transfer_speed = speed;
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
            transfer_speed: TransferSpeed::default(),
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
        let job_queue = self.job_queue.clone();
        self.object_list_panel = Some(cx.new(|cx| {
            ObjectListPanel::new(
                this_weak.clone(),
                ossclient,
                job_queue,
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

    fn on_quit_action(&mut self, _: &QuitAction, window: &mut Window, cx: &mut Context<Self>) {
        self.request_quit(window, cx);
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

                // 展开面板时重新测量宽度
                // if this.jobs_open {
                //     let panel = this.job_panel.clone();
                //     cx.on_next_frame(window, move |_, window, _| {
                //         window.on_next_frame(move |_, cx| {
                //             panel.update(cx, |panel, cx| panel.refresh_list(cx));
                //         });
                //     });
                // }

                cx.notify();
            }))
    }

    /// 有任务在排队或运行中吗？（退出确认用）
    fn has_pending_jobs(&self, cx: &App) -> bool {
        let s = self.job_queue.read(cx).summary();
        s.queued + s.running > 0
    }

    /// 退出前的统一入口：有任务在跑就先问一句。
    ///
    /// 三个入口都走这里：
    /// - 菜单 File → Quit（QuitAction）
    /// - 标题栏的 X（TitleBar::on_close_window —— 它默认直接 remove_window，
    ///   **不经过** on_window_should_close，所以必须单独拦）
    /// - 窗口管理器要求关闭（Alt+F4 / 任务栏，走 on_window_should_close）
    fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_pending_jobs(cx) {
            cx.quit();
            return;
        }

        let s = self.job_queue.read(cx).summary();
        let title = match (s.running, s.queued) {
            (0, q) => format!("{q} transfers are still queued"),
            (r, 0) => format!("{r} transfers are still running"),
            (r, q) => format!("{} transfers are still running or queued", r + q),
        };

        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .confirm()
                .title(title.clone())
                .description("Quitting now stops them mid-transfer.")
                .ok_text("Quit anyway")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Cancel")
                .on_ok(|_, _, cx| {
                    cx.quit();
                    true
                })
        });
    }

    /// 整体传输速率。只在真有数据在传时才出现，空闲时这段不渲染。
    fn speed_indicator(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let speed = self.transfer_speed;

        div()
            .h_flex()
            .items_center()
            .gap_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .when(speed.up > 0, |this| {
                this.child(format!("↑ {}/s", format_file_size(speed.up)))
            })
            .when(speed.down > 0, |this| {
                this.child(format!("↓ {}/s", format_file_size(speed.down)))
            })
    }

    /// 批量入队（上传一个文件夹时可能有几千个文件）
    pub fn enqueue_jobs(
        &mut self,
        kinds: Vec<JobKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kinds.is_empty() {
            return;
        }

        let client = self.ossclient.clone();
        self.job_queue.update(cx, |state, cx| {
            state.enqueue_many(kinds.into_iter().map(|kind| (kind, client.clone())), cx);
        });

        // 有任务进来就弹出面板并滚到最新一条：
        // 不弹的话用户点了上传看不到任何变化，弹了不滚的话看到的是列表顶部的一堆历史任务
        self.jobs_open = true;
        self.job_panel
            .update(cx, |panel, cx| panel.scroll_to_newest(window, cx));
        cx.notify();
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
            .on_action(cx.listener(Self::on_quit_action))
            .child(
                TitleBar::new()
                    // X 按钮默认是 window.remove_window()，绕过了 on_window_should_close，
                    // 所以这里单独接管（Linux 专用，其他平台这个设置会被忽略）
                    .on_close_window({
                        let main_view = cx.weak_entity();
                        move |_, window, cx| {
                            main_view
                                .update(cx, |main_view, cx| main_view.request_quit(window, cx))
                                .ok();
                        }
                    })
                    .child(
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
                                    .size(px(400.0))
                                    // .size_range(px(300.0)..px(400.0))
                                    .visible(self.jobs_open)
                                    .child(
                                        div()
                                            .min_w_0()
                                            .size_full()
                                            .v_flex()
                                            .child(
                                                div()
                                                    .p_2()
                                                    .border_b_1()
                                                    .border_color(cx.theme().border)
                                                    .child("Tasks"),
                                            )
                                            .child(
                                                div()
                                                    .w_full()
                                                    .p_2()
                                                    .flex_grow_1()
                                                    .child(self.job_panel.clone()),
                                            ),
                                    ),
                            ),
                    )
                    .when(self.show_fps, |this| this.child(fps_monitor(window, cx))),
            )
            .child(
                // 任务面板从右侧推出，所以开关和速率也放状态栏右侧：
                // 按钮就在面板右下角的正下方，而且跟面板右边缘对齐成一条竖线。
                // 摘要按钮贴最右（它就是面板的开关），速率在它左边。
                StatusBar::new().right(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .child(self.speed_indicator(cx))
                        .child(self.jobs_summary_button(cx)),
                ),
            )
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
