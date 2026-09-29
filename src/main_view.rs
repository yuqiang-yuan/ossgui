use std::sync::{Arc, OnceLock};

use ali_oss_rs::{Client, bucket::BucketOperations, bucket_common::BucketSummary};
use gpui_fps::fps_monitor;
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Menu, MenuItem, ParentElement, Render, Styled, Task, Window, base::{Placement, StyledExt}, component::{
        ActiveTheme, IconName, Theme, ThemeMode, TitleBar, WindowExt, button::{Button, ButtonVariants}, menu::{AppMenuBar, DropdownMenu, PopupMenuItem}, notification::NotificationType, progress::ProgressCircle, status_bar::StatusBar, table::{Column, DataTable, TableDelegate, TableState},
    }, div, prelude::FluentBuilder, px,
};

use crate::actions::{AboutAction, QuitAction};

enum LoadState {
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

/// AbortHandle 在 drop 时中止对应的 tokio 任务
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct BucketsTableDelegate {
    rows: Vec<BucketSummary>,
    loading: bool,
    columns: Vec<Column>,
}

impl BucketsTableDelegate {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            loading: true, // 首屏直接骨架屏
            columns: vec![Column::new("name", "Bucket").sortable()],
        }
    }

    /// 由 MainView 调用；内部不 notify，通知由调用方统一发
    pub fn set_rows(&mut self, rows: Vec<BucketSummary>) {
        self.rows = rows;
        self.loading = false;
    }

    pub fn row(&self, ix: usize) -> Option<&BucketSummary> {
        self.rows.get(ix)
    }
}

impl TableDelegate for BucketsTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }
    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        match col_ix {
            0 => div().child(row.name.clone()).into_any_element(),
            _ => div().into_any_element(),
        }
    }
}

pub struct MainView {
    focus_handle: FocusHandle,
    menubar: Entity<AppMenuBar>,
    ossclient: Arc<Client>,
    buckets_load: LoadState,
    buckets_state: Entity<TableState<BucketsTableDelegate>>,
    load_task: Task<()>,
    show_fps: bool,
}

fn tokio_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4) // 纯网络等待，1 个 worker 够；CPU 任务多再加
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    })
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

        let this = Self {
            focus_handle,
            menubar: AppMenuBar::new(cx),
            show_fps: true,
            ossclient: Arc::new(Client::from_env()),
            buckets_load: LoadState::Idle,
            buckets_state: cx.new(|cx| TableState::new(BucketsTableDelegate::new(), window, cx)),
            load_task: Task::ready(()), // 占位，load_buckets 里会替换
        };

        cx.on_next_frame(window, |this, _, cx| this.load_buckets(cx));
        this
    }

    fn load_buckets(&mut self, cx: &mut Context<Self>) {
        if matches!(self.buckets_load, LoadState::Loading) {
            return;
        }

        self.buckets_load = LoadState::Loading;
        cx.notify();

        let client = self.ossclient.clone();
        let handle = tokio_runtime().handle().clone();

        self.load_task = cx.spawn(async move |this, cx| {
            // 这段 async 块跑在 tokio runtime 上
            let join = handle.spawn(async move { client.list_buckets(None).await });

            // 在 async 块内部创建：GPUI Task 被 drop → future 被 drop → guard 被 drop → abort
            let _abort_on_drop = AbortOnDrop(join.abort_handle());

            // JoinHandle 在 GPUI 执行器上 await，没问题
            let result = match join.await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
                Err(join_err) => Err(anyhow::anyhow!("oss task failed: {join_err}")),
            };

            this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(results) => {
                        view.buckets_state.update(cx, |state, cx| {
                            state.delegate_mut().set_rows(results.buckets);
                            cx.notify();
                        });
                        view.buckets_load = LoadState::Loaded;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        view.buckets_load = LoadState::Failed(msg.clone());
                        window.push_notification((NotificationType::Error, msg), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn bucket_panel(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.buckets_load {
            LoadState::Idle => div().size_full(),

            LoadState::Loading => div()
                .size_full()
                .items_center()
                .justify_center()
                .v_flex()
                .gap_1()
                .child(ProgressCircle::new("buckets-loading").loading(true))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().secondary)
                        .child("Loading..."),
                ),

            LoadState::Loaded => div().size_full().child(
                DataTable::new(&self.buckets_state)
                    .stripe(true)
                    .scrollbar_visible(true, true),
            ),

            LoadState::Failed(msg) => div().child(msg.clone()),
        }
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
                            .h_full()
                            .border_r_1()
                            .border_color(cx.theme().border),
                    )
                    .child(div().size_full().child(self.bucket_panel(window, cx)))
                    .when(self.show_fps, |this| this.child(fps_monitor(window, cx))),
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
