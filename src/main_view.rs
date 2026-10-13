use std::sync::Arc;

use ali_oss_rs::Client;
use gpui_fps::fps_monitor;
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Menu, MenuItem,
    ParentElement, Render, Styled, Subscription, Window,
    assets::IconName,
    base::{Disableable, Placement, StyledExt, resizable_panel},
    component::{
        ActiveTheme, Sizable, Theme, ThemeMode, TitleBar, WindowExt,
        button::{Button, ButtonVariant, ButtonVariants},
        h_resizable,
        menu::{AppMenuBar, DropdownMenu, PopupMenuItem},
        progress::ProgressCircle,
        status_bar::StatusBar,
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::{
    account::{Account, AccountPanel, AccountSettings},
    actions::{AboutAction, QuitAction},
    bucket_view::BucketListPanel,
    common::{format_file_size, oss_region_map},
    globals::APP_NAME,
    job::{JobKind, JobPanel, JobQueue, JobsSummary, TransferSpeed},
    object_view::ObjectListPanel,
    settings::AppSettings,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scene {
    /// 启动时正在尝试恢复上次的会话。
    ///
    /// 先不渲染账号屏 —— 恢复成功的话它马上就会被替换掉，用户会看到一次闪烁。
    /// 恢复失败会转成 [`Scene::Accounts`]。
    Restoring,
    Accounts,
    Buckets,
    Objects,
}

/// 当前 OSS 会话。
///
/// client 和 bucket 面板是同生共死的一对：没选定账号就没有 client，
/// 也就没有 bucket 面板。用枚举把它们绑在一起，而不是各自 Option ——
/// 后者会允许"有 client 没面板"这种非法状态，还得在每个使用点分别 unwrap。
enum Session {
    /// 还没选定账号。此时不能发任何 OSS 请求。
    NoAccount,
    /// 已用某个账号建立会话
    Active {
        account: Account,
        client: Arc<Client>,
        /// 切到 Objects 场景后它仍然活着，返回时不用重新加载
        bucket_list_panel: Entity<BucketListPanel>,
    },
}

impl Session {
    fn client(&self) -> Option<&Arc<Client>> {
        match self {
            Self::NoAccount => None,
            Self::Active { client, .. } => Some(client),
        }
    }

    /// 状态栏上显示"现在用的是哪个账号"
    fn account_label(&self) -> Option<String> {
        match self {
            Self::NoAccount => None,
            Self::Active { account, .. } => Some(account.access_key_id.clone()),
        }
    }
}

pub struct MainView {
    focus_handle: FocusHandle,
    menubar: Entity<AppMenuBar>,
    show_fps: bool,
    /// 账号列表由 MainView 持有，AccountPanel 只拿句柄 ——
    /// 两边各 load 一份的话会各看各的，AccountPanel 里加的账号 MainView 看不到。
    account_settings: Entity<AccountSettings>,
    account_panel: Entity<AccountPanel>,
    session: Session,
    object_list_panel: Option<Entity<ObjectListPanel>>,
    bucket_name: Option<String>,
    scene: Scene,

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

        let account_settings = cx.new(|_| AccountSettings::load());

        // 启动时先看看有没有可以恢复的会话。有的话第一屏就是 spinner，
        // 而不是账号列表 —— 免得恢复成功时闪一下。
        let resume_candidate = {
            let settings = account_settings.read(cx);
            settings
                .last_used
                .as_ref()
                .and_then(|id| settings.accounts.iter().find(|a| a.get_id() == id))
                .cloned()
        };

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

        let this = Self {
            focus_handle,
            menubar: AppMenuBar::new(cx),
            show_fps: false,
            account_settings: account_settings.clone(),
            account_panel: cx.new(|cx| {
                AccountPanel::new(this_weak.clone(), account_settings.clone(), window, cx)
            }),
            // client 要等密钥从钥匙串里读出来才有，见 try_resume / activate_account
            session: Session::NoAccount,
            object_list_panel: None,
            bucket_name: None,
            scene: if resume_candidate.is_some() {
                Scene::Restoring
            } else {
                Scene::Accounts
            },
            job_queue,
            job_panel,
            jobs_summary: JobsSummary::default(),
            transfer_speed: TransferSpeed::default(),
            jobs_open: false,
            _subs: vec![job_sub],
        };

        // 等实体构造完再开始异步工作，跟 BucketListPanel 一个路子
        cx.on_next_frame(window, move |this, window, cx| {
            if let Some(account) = resume_candidate {
                this.try_resume(account, window, cx);
            }
        });

        this
    }

    /// 启动时尝试恢复上次使用的账号。
    ///
    /// 只有"确实读到了密钥"才算恢复成功，其余情况一律安静地退回账号屏：
    /// 没存过密钥、钥匙串读不出来、用户在系统授权框上点了取消，
    /// 都是正常的启动路径，不该在刚打开应用时就弹错误。
    fn try_resume(&mut self, account: Account, window: &mut Window, cx: &mut Context<Self>) {
        let load_task = account.load_secret(cx);
        let window_handle = window.window_handle();

        cx.spawn(async move |this, cx| {
            let secret = match load_task.await {
                Ok(Some((_username, secret))) => String::from_utf8(secret).ok(),
                _ => None,
            };

            let _ = cx.update(|cx| {
                cx.update_window(window_handle, |_, window, cx| {
                    this.update(cx, |this, cx| match secret {
                        Some(secret) => this.activate_account(account, secret, window, cx),
                        None => {
                            this.scene = Scene::Accounts;
                            cx.notify();
                        }
                    })
                    .ok();
                })
            });
        })
        .detach();
    }

    /// 用某个账号建立会话：建 client、建 bucket 面板、切到 bucket 场景。
    ///
    /// 由 AccountPanel 在把密钥从钥匙串读出来之后调用 —— 这是整个应用里
    /// 唯一一个会创建 OSS client 的地方。
    pub fn activate_account(
        &mut self,
        account: Account,
        access_key_secret: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let endpoint = oss_region_map()
            .get(account.region.as_str())
            .map(|r| r.to_string())
            .unwrap_or(format!("oss-{}.aliyuncs.com", account.region));

        let client = Arc::new(Client::new(
            &account.access_key_id,
            &access_key_secret,
            &account.region,
            &endpoint,
        ));

        let this_weak = cx.weak_entity();
        let bucket_list_panel =
            cx.new(|cx| BucketListPanel::new(this_weak, client.clone(), window, cx));

        // 记住这次选择，下次启动直接恢复这个账号。
        // 恢复路径也会走到这里，值没变就别再写一次盘。
        let account_id = account.get_id().clone();
        self.account_settings.update(cx, |settings, cx| {
            if settings.last_used.as_deref() == Some(account_id.as_str()) {
                return;
            }

            settings.last_used = Some(account_id);
            settings.save();
            cx.notify();
        });

        self.session = Session::Active {
            account,
            client,
            bucket_list_panel,
        };
        self.object_list_panel = None;
        self.bucket_name = None;
        self.scene = Scene::Buckets;
        cx.notify();
    }

    pub fn goto_object_list(
        &mut self,
        bucket_name: String,
        region: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.session.client().cloned() else {
            return; // 没有会话，不该走到这里
        };

        let this_weak = cx.weak_entity();
        let job_queue = self.job_queue.clone();

        self.bucket_name = Some(bucket_name.clone());
        self.object_list_panel = Some(cx.new(|cx| {
            ObjectListPanel::new(
                this_weak,
                client,
                job_queue,
                bucket_name,
                region,
                window,
                cx,
            )
        }));
        self.scene = Scene::Objects;
        cx.notify();
    }

    /// 切回账号屏挑别的账号。
    ///
    /// 当前会话故意留着不拆：用户可能只是点错了，而且状态栏还能显示
    /// "现在用的是哪个账号"。真正换账号是等他在列表里选定之后，
    /// 由 `activate_account` 整体替换掉。
    fn goto_account_list(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.session, Session::Active { .. }) {
            return;
        }

        self.scene = Scene::Accounts;
        cx.notify();
    }

    pub fn goto_bucket_list(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.session, Session::Active { .. }) {
            return; // 没有会话，不该走到这里
        }

        self.bucket_name = None;
        self.object_list_panel = None;
        self.scene = Scene::Buckets;
        cx.notify();
    }

    fn on_quit_action(&mut self, _: &QuitAction, window: &mut Window, cx: &mut Context<Self>) {
        self.request_quit(window, cx);
    }

    fn on_about_action(&mut self, _: &AboutAction, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, |alert, _, _| {
            alert
                .title(APP_NAME)
                .description(
                    div()
                        .v_flex()
                        .gap_1()
                        .child(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .child("Aliyun OSS desktop client"),
                )
                // 就是个信息框，只有 OK 一个按钮
                .show_cancel(false)
                .ok_text("OK")
        });
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

        let Some(client) = self.session.client().cloned() else {
            return; // 没有会话就没有 client，发不出任务
        };

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
        // 先取出来，后面 div 链里就不用一直借着 self
        let account_label = self.session.account_label();

        div()
            .id("main-view")
            .relative()
            .track_focus(&self.focus_handle)
            .size_full()
            .v_flex()
            .on_action(cx.listener(Self::on_quit_action))
            .on_action(cx.listener(Self::on_about_action))
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
                            .w_12()
                            .flex_shrink_0()
                            .h_full()
                            .py_2()
                            .bg(cx.theme().secondary.opacity(0.5))
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .v_flex()
                            .items_center()
                            .justify_end()
                            .child(
                                Button::new("account-select-screen-button")
                                    .tooltip("Switch account")
                                    .icon(IconName::CircleUserRound)
                                    .ghost()
                                    .rounded_full()
                                    // 没有会话时本来就在账号屏上，按钮没意义
                                    .disabled(!matches!(self.session, Session::Active { .. }))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.goto_account_list(cx);
                                    }))
                            )
                    )
                    .child(
                        h_resizable("h-resizable")
                            .child(
                                resizable_panel().child(
                                    div()
                                        .min_w_0()
                                        .size_full()
                                        .when(matches!(self.scene, Scene::Restoring), |d| {
                                            d.size_full()
                                                .h_flex()
                                                .items_center()
                                                .justify_center()
                                                .child(ProgressCircle::new("restoring-session"))
                                        })
                                        .when(matches!(self.scene, Scene::Accounts), |d| {
                                            d.size_full().h_flex().items_start().justify_center().child(
                                                div().w_128().h_full().child(self.account_panel.clone()),
                                            )
                                        })
                                        .when(matches!(self.scene, Scene::Buckets), |el| {
                                            el.p_2().child(match &self.session {
                                                Session::Active {
                                                    bucket_list_panel, ..
                                                } => bucket_list_panel.clone().into_any_element(),
                                                // scene 是 Buckets 时 session 必然是 Active，
                                                // 这里只是兜底，正常走不到
                                                Session::NoAccount => div()
                                                    .child("No account selected")
                                                    .into_any_element(),
                                            })
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
                StatusBar::new()
                    // 现在用的是哪个账号 —— 有多个 AK 时这个提示很重要
                    .left(
                        div()
                            .h_flex()
                            .items_center()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .when_some(account_label, |this, label| this.child(label)),
                    )
                    .right(
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
/// 应用菜单栏。
///
/// About 放哪儿是跟平台走的：
/// - macOS：属于应用菜单（第一个菜单），而且必须是第一项，Apple 的规范
/// - 其他平台：放在 Help 里
///
/// 所以 macOS 上 Help 里不再放一份，否则菜单栏里会出现两个 About。
fn build_menus() -> Vec<Menu> {
    let about_label = format!("About {APP_NAME}");
    let mut menus = Vec::new();

    #[cfg(target_os = "macos")]
    menus.push(Menu {
        name: APP_NAME.into(),
        items: vec![
            MenuItem::action(about_label, AboutAction),
            MenuItem::separator(),
            MenuItem::action("Quit", QuitAction),
        ],
        disabled: false,
    });

    menus.push(Menu {
        name: "File".into(),
        items: vec![MenuItem::action("Quit", QuitAction)],
        disabled: false,
    });

    #[cfg(not(target_os = "macos"))]
    menus.push(Menu {
        name: "Help".into(),
        items: vec![MenuItem::action(about_label, AboutAction)],
        disabled: false,
    });

    menus
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
