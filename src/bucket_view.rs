use std::sync::Arc;

use ali_oss_rs::{
    Client,
    bucket::BucketOperations,
    bucket_common::{BucketSummary, ListBucketsOptions, ListBucketsResult},
    common::StorageClass,
};
use gpui_kit::{
    App, AppContext, Context, Entity, Hsla, IntoElement, ParentElement, Render, Styled, Subscription, Task, Window, assets::IconName, base::{
        Disableable, StyledExt,
        input::{InputEvent, InputState},
    }, component::{
        ActiveTheme, Icon, Sizable, WindowExt, button::Button, input::Input, notification::NotificationType, table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    }, div, px,
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
}

impl BucketTableDelegate {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            filtered_indexes: Vec::new(),
            loading: true, // 首屏直接骨架屏
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

        match col_ix {
            0 => div()
                .text_right()
                .child(format!("{}", row_ix + 1))
                .into_any_element(),
            1 => div()
                .h_flex()
                .gap_1()
                .child(
                    Icon::default()
                        .path("icons/bucket.svg")
                        .size_4()
                        .text_color(get_color(row, cx)),
                )
                .child(row.name.clone())
                .into_any_element(),
            2 => div().child(row.region.clone()).into_any_element(),
            3 => div()
                .child(format_datetime(&row.creation_date))
                .into_any_element(),
            4 => div()
                .child(row.storage_class.to_string())
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
        self.rows.sort_by(|a, b| {
            match col_ix {
                1 => a.name.cmp(&b.name),
                2 => a.region.cmp(&b.region),
                3 => a.creation_date.cmp(&b.creation_date),
                4 => a.storage_class.to_string().cmp(&b.storage_class.to_string()),
                _ => std::cmp::Ordering::Equal,
            }
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

        let this = Self {
            ossclient,
            buckets_load: LoadState::Idle,
            buckets_state: cx.new(|cx| {
                TableState::new(BucketTableDelegate::new(), window, cx)
                    .row_selectable(true)
                    .col_selectable(false)
                    .cell_selectable(false)
            }),
            load_task: Task::ready(()), // 占位，load_buckets 里会替换
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
                        view.buckets_load = LoadState::Failed(msg.clone());
                        window.push_notification((NotificationType::Error, msg), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }
}

impl Render for BucketListPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .gap_2()
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
