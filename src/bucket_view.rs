use std::sync::Arc;

use ali_oss_rs::{
    Client, bucket::BucketOperations, bucket_common::{BucketDetail, BucketSummary, ListBucketsOptions, ListBucketsResult}, common::StorageClass,
};
use gpui_kit::{
    App, AppContext, Context, Div, Entity, Hsla, IntoElement, ParentElement, Render, Role::DescriptionList, Styled, Subscription, Task, WeakEntity, Window, assets::IconName, base::{
        Disableable, Placement, StyledExt, input::{InputEvent, InputState},
    }, component::{
        ActiveTheme, Icon, Sizable, WindowExt, button::{Button, ButtonVariants}, input::Input, notification::NotificationType, progress::ProgressCircle, table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    }, div, prelude::FluentBuilder, px,
};

use crate::common::{AbortOnDrop, LoadState, format_datetime, tokio_runtime};

pub struct BucketTableDelegate {
    /// This is all data
    rows: Vec<BucketSummary>,

    /// This are rows after applying filter
    filtered_indexes: Vec<usize>,
    loading: bool,
    columns: Vec<Column>,
    search: String,

    bucket_list_panel: WeakEntity<BucketListPanel>,
}

impl BucketTableDelegate {
    pub fn new(bucket_list_panel: WeakEntity<BucketListPanel>) -> Self {
        Self {
            rows: Vec::new(),
            filtered_indexes: Vec::new(),
            loading: true, // 首屏直接骨架屏
            bucket_list_panel,
            columns: vec![
                Column::new("ix", "#")
                    .width(px(60.0))
                    .text_right()
                    .movable(false),
                Column::new("name", "Bucket")
                    .width(px(260.0))
                    .sortable()
                    .movable(false),
                Column::new("region", "Region")
                    .width(px(160.0))
                    .sortable()
                    .movable(false),
                Column::new("created_at", "Created at")
                    .width(px(160.0))
                    .sortable()
                    .movable(false),
                Column::new("storage_type", "Storage type")
                    .width(px(120.0))
                    .movable(false),
                Column::new("actions", "").width(px(60.0)).movable(false),
            ],
            search: String::new(),
        }
    }

    /// 由 MainView 调用；内部不 notify，通知由调用方统一发
    pub fn set_rows(&mut self, rows: Vec<BucketSummary>) {
        self.rows = rows;
        self.loading = false;
        let s = self.search.clone();
        self.apply_filter(s.as_str());
    }

    pub fn extend_rows(&mut self, rows: Vec<BucketSummary>) {
        self.rows.extend(rows);
        self.loading = false;
        let s = self.search.clone();
        self.apply_filter(s.as_str());
    }

    pub fn row(&self, ix: usize) -> Option<&BucketSummary> {
        self.filtered_indexes
            .get(ix)
            .map(|ix| self.rows.get(*ix))
            .flatten()
    }

    fn apply_filter(&mut self, needle: &str) {
        self.search = needle.to_string();
        let s = self.search.trim().to_ascii_lowercase();
        self.filtered_indexes = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, b)| s.is_empty() || b.name.contains(&s))
            .map(|(ix, _)| ix)
            .collect();
    }
}

/// Get color for bucket icon accoring to bucket storage class
fn get_color(bucket: &BucketSummary, cx: &App) -> Hsla {
    match bucket.storage_class {
        StorageClass::Standard => cx.theme().blue,
        _ => cx.theme().muted_foreground,
    }
}

impl TableDelegate for BucketTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.filtered_indexes.len()
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
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self
            .filtered_indexes
            .get(row_ix)
            .map(|ix| self.rows.get(*ix))
            .flatten()
        else {
            return div().into_any_element();
        };

        let panel = self.bucket_list_panel.clone();
        let name = row.name.clone();

        match col_ix {
            0 => div()
                .text_right()
                .child(format!("{}", row_ix + 1))
                .into_any_element(),
            1 => div()
                .h_flex()
                .items_baseline()
                .gap_1()
                .child(
                    Icon::default()
                        .path("icons/bucket.svg")
                        .size_4()
                        .text_color(get_color(row, cx)),
                )
                .child(
                    Button::new(format!("bucket-{}-button", row.name))
                        .text()
                        .label(row.name.clone()),
                )
                .into_any_element(),
            2 => div().child(row.region.clone()).into_any_element(),
            3 => div()
                .child(format_datetime(&row.creation_date))
                .into_any_element(),
            4 => div()
                .child(row.storage_class.to_string())
                .into_any_element(),
            5 => div()
                .h_full()
                .h_flex()
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    Button::new(format!("bucket-{}-info-button", row.name))
                        .tooltip("Bucket detail")
                        .icon(IconName::Info)
                        .text()
                        .rounded_full()
                        .compact()
                        .on_click(move |_, window, cx| {
                            panel.update(cx, |panel, cx| panel.show_bucket_detail(&name, window, cx)).ok();
                        }),
                )
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.rows.sort_by(|a, b| match col_ix {
            1 => a.name.cmp(&b.name),
            2 => a.region.cmp(&b.region),
            3 => a.creation_date.cmp(&b.creation_date),
            4 => a
                .storage_class
                .to_string()
                .cmp(&b.storage_class.to_string()),
            _ => std::cmp::Ordering::Equal,
        });

        if matches!(sort, ColumnSort::Descending) {
            self.rows.reverse();
        }

        self.apply_filter(&self.search.clone());
        cx.notify();
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.column(col_ix, cx);

        div()
            .size_full()
            .text_align(col.align)
            .child(col.name.clone())
    }
}

pub struct BucketListPanel {
    ossclient: Arc<Client>,
    buckets_load: LoadState,
    buckets_state: Entity<TableState<BucketTableDelegate>>,
    load_task: Task<()>,
    get_detail_task: Task<()>,
    get_detail_load: LoadState,
    search_state: Entity<InputState>,
    is_truncated: bool,
    next_marker: Option<String>,
    _subs: Vec<Subscription>,
}

impl BucketListPanel {
    pub fn new(ossclient: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search")
                .clean_on_escape()
        });

        let _subs =
            vec![cx.subscribe(
                &search_state,
                |view, state, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } => {
                        let s = state.read(cx).value();
                        view.buckets_state.update(cx, |state, cx| {
                            state.delegate_mut().apply_filter(&s);
                            cx.notify();
                        });
                    }
                    _ => {}
                },
            )];
        let this_weak = cx.weak_entity();
        let this = Self {
            ossclient,
            buckets_load: LoadState::Idle,
            buckets_state: cx.new(|cx| {
                TableState::new(BucketTableDelegate::new(this_weak), window, cx)
                    .row_selectable(true)
                    .col_selectable(false)
                    .cell_selectable(false)
            }),
            load_task: Task::ready(()), // 占位，load_buckets 里会替换
            get_detail_task: Task::ready(()),
            get_detail_load: LoadState::Idle,
            search_state,
            is_truncated: false,
            next_marker: None,
            _subs,
        };

        cx.on_next_frame(window, |this, _, cx| this.load_buckets(false, cx));

        this
    }

    /// Load buckets
    ///
    /// if `extend_mode` is set to `true`, new data will be appended into existing data
    fn load_buckets(&mut self, extend_mode: bool, cx: &mut Context<Self>) {
        if matches!(self.buckets_load, LoadState::Loading) {
            return;
        }

        self.buckets_load = LoadState::Loading;
        cx.notify();

        let client = self.ossclient.clone();
        let handle = tokio_runtime().handle().clone();

        let marker = self.next_marker.clone();

        self.load_task = cx.spawn(async move |this, cx| {
            let options = ListBucketsOptions {
                max_keys: Some(200),
                marker: if extend_mode { marker } else { None },
                ..Default::default()
            };

            println!("list buckets request options marker: {:#?}", options.marker);

            // 这段 async 块跑在 tokio runtime 上
            let join = handle.spawn(async move { client.list_buckets(Some(options)).await });

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
                        let ListBucketsResult {
                            is_truncated,
                            next_marker,
                            buckets,
                            ..
                        } = results;
                        view.is_truncated = is_truncated;
                        view.next_marker = if is_truncated { next_marker } else { None };

                        println!(
                            "list buckets result: is_truncated: {}, next marker: {:?}",
                            view.is_truncated, view.next_marker
                        );

                        view.buckets_state.update(cx, |state, cx| {
                            if extend_mode {
                                state.delegate_mut().extend_rows(buckets);
                            } else {
                                state.delegate_mut().set_rows(buckets);
                            }

                            cx.notify();
                        });
                        view.buckets_load = LoadState::Loaded;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        view.buckets_load = LoadState::Failed;
                        window.push_notification((NotificationType::Error, msg), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn show_bucket_detail(&mut self, bucket_name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.get_detail_load, LoadState::Loading) {
            return;
        }

        let client = self.ossclient.clone();
        let bucket_name = bucket_name.to_string();
        let detail_view = cx.new(|cx| BucketDetailPanel::new(client, bucket_name.clone(), window, cx));

        window.open_sheet_at(Placement::Right, cx, move |sheet, _, _| {
            sheet
                .p_0()
                .title(div().text_lg().child(bucket_name.clone()))
                .child(
                    detail_view.clone()
                )
        });
    }
}

impl Render for BucketListPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .gap_2()
            .child(div().text_3xl().child("Buckets"))
            .child(
                div()
                    .w_full()
                    .h_flex()
                    .gap_2()
                    .child(
                        Input::new(&self.search_state)
                            .w_64()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    )
                    .child(div().flex_grow_1())
                    .child(
                        Button::new("load-buckets-button")
                            .label("Refresh")
                            .loading(matches!(self.buckets_load, LoadState::Loading))
                            .icon(IconName::RefreshCw)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.load_buckets(false, cx);
                            })),
                    )
                    .child(
                        Button::new("load-more-buckets-button")
                            .label("Load more")
                            .loading(matches!(self.buckets_load, LoadState::Loading))
                            .icon(IconName::ArrowDownToLine)
                            .disabled(!self.is_truncated)
                            .tooltip(if self.is_truncated {
                                "Load more data"
                            } else {
                                "All buckets are loaded"
                            })
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.load_buckets(true, cx);
                            })),
                    ),
            )
            .child(
                DataTable::new(&self.buckets_state)
                    .stripe(false)
                    .scrollbar_visible(true, true),
            )
    }
}

struct BucketDetailPanel {
    ossclient: Arc<Client>,
    bucket_name: String,
    load_task: Task<()>,
    detail_load: LoadState,
    bucket_detail: Option<BucketDetail>,
}

impl BucketDetailPanel {
    fn new(ossclient: Arc<Client>, bucket_name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let this = Self {
            ossclient,
            bucket_name,
            load_task: Task::ready(()),
            detail_load: LoadState::Idle,
            bucket_detail: None,
        };

        cx.on_next_frame(window, |this, _, cx| this.load_detail(cx));

        this
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        if matches!(self.detail_load, LoadState::Loading) {
            return;
        }

        self.detail_load = LoadState::Loading;
        cx.notify();

        let client = self.ossclient.clone();
        let handle = tokio_runtime().handle().clone();
        let bucket_name = self.bucket_name.clone();

        self.load_task = cx.spawn(async move |this, cx| {
            let join = handle.spawn(async move { client.get_bucket_info(bucket_name).await });
            let _abort_on_drop = AbortOnDrop(join.abort_handle());

            let result = match join.await {
                Ok(Ok(d)) => Ok(d),
                Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
                Err(join_err) => Err(anyhow::anyhow!("oss task failed: {join_err}")),
            };

            this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(d) => {
                        view.bucket_detail = Some(d);
                        view.detail_load = LoadState::Loaded;
                    },
                    Err(e) => {
                        window.push_notification((NotificationType::Error, e.to_string()), cx);
                        view.detail_load = LoadState::Failed;
                    },
                }

                cx.notify();
            }).ok();
        });
    }

    fn render_detail(&self) -> Div {
        if let Some(d) = &self.bucket_detail {
            div()
                .child(
                    gpui_kit::component::description_list::DescriptionList::horizontal()
                        .columns(1)
                        .item("Name", d.name.as_str(), 1)
                        .item("Location", d.location.as_str(), 1)
                        .item("Storage", d.storage_class.to_string(), 1)
                        .item("Created at", format_datetime(&d.creation_date), 1)
                        .item("Acl", d.access_control_list.iter().map(|acl| acl.to_string()).collect::<Vec<_>>().join("\n"), 1)
                )
        } else {
            div().text_center().child("Something went wrong...")
        }

    }
}

impl Render for BucketDetailPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .p_4()
            .child(match self.detail_load {
                LoadState::Idle => div().child("Idle"),
                LoadState::Loading => div().h_flex().justify_center().child(ProgressCircle::new("bucket-detail-loading")),
                LoadState::Loaded => self.render_detail(),
                LoadState::Failed => div().child("Failed"),
            })
    }
}
