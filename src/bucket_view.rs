use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use ali_oss_rs::{
    Client,
    bucket::BucketOperations,
    bucket_common::{BucketDetail, BucketStat, BucketSummary, ListBucketsOptions, ListBucketsResult},
    common::StorageClass,
};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext, Context, Div, Entity, Global, Hsla, IntoElement,
    ParentElement, Pixels, Render, SharedString, Styled, Subscription, Task, TextAlign, WeakEntity,
    Window,
    assets::IconName,
    base::{
        Disableable, Placement, StyledExt,
        input::{InputEvent, InputState},
    },
    component::{
        ActiveTheme, Icon, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        description_list::{DescriptionItem, DescriptionList},
        hover_card::HoverCard,
        input::Input,
        menu::{DropdownMenu, PopupMenuItem},
        notification::NotificationType,
        progress::ProgressCircle,
        table::{Column, ColumnSort, DataTable, TableDelegate, TableState, TableEvent},
    },
    div, px,
};

use crate::{
    common::{
        AbortOnDrop, LoadState, format_count, format_datetime, format_file_size, oss_endpoint,
        tokio_runtime,
    },
    main_view::MainView,
    settings::AppSettings,
};

/// bucket 统计信息的应用级缓存：bucket 名 → 统计。
///
/// bucket 名在 OSS 里全局唯一，直接当 key。命中就不发请求；
/// 刷新按钮清掉它，重新拉。统计接口失败静默，列里显示 "-"。
#[derive(Default)]
pub struct BucketStatsCache {
    stats: std::collections::HashMap<String, BucketStat>,
}

impl Global for BucketStatsCache {}

impl BucketStatsCache {
    fn get(&self, bucket: &str) -> Option<&BucketStat> {
        self.stats.get(bucket)
    }

    fn insert(&mut self, bucket: String, stat: BucketStat) {
        self.stats.insert(bucket, stat);
    }

    fn clear(&mut self) {
        self.stats.clear();
    }

    /// 给 delegate 用的整体快照
    fn snapshot(&self) -> HashMap<String, BucketStat> {
        self.stats.clone()
    }
}

/// 顶部统计块：label + 值。背景/边框 = 主色低透明度叠在主题底色上，
/// 深浅色模式都自适应（不做硬编码颜色）
fn stat_block(label: &'static str, value: String, accent: Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(accent.opacity(0.10))
        .border_1()
        .border_color(accent.opacity(0.25))
        .v_flex()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(accent.opacity(0.85))
                .child(label),
        )
        .child(div().text_xl().text_color(accent).child(value))
}

/// 顶部统计块的合计：遍历已加载 bucket 的统计缓存求和
#[derive(Default)]
struct StatTotals {
    objects: u64,
    total: u64,
    standard: u64,
    infrequent: u64,
    archive: u64,
    cold_archive: u64,
    deep_cold_archive: u64,
}

/// 五种存储类型的用量加总（Total 列/块用）
fn total_storage(s: &BucketStat) -> u64 {
    s.standard_storage
        + s.infrequent_access_storage
        + s.archive_storage
        + s.cold_archive_storage
        + s.deep_cold_archive_storage
}

/// 统计单元格的共同外壳：右对齐 + 垂直居中 + text_sm
fn stat_cell_div(text: String) -> Div {
    div()
        .size_full()
        .text_sm()
        .v_flex()
        .justify_center()
        .text_align(TextAlign::Right)
        .child(text)
}

/// bucket 表所有列的定义（完整列表，未按可见性过滤）。
///
/// 隐藏列的做法是**按 key 过滤这个列表**再让 TableState::refresh 重建表头/单元格，
/// 所以 render_td / 排序都按列 key 分发、不看列下标 —— 下标会随隐藏列漂移
fn bucket_columns() -> Vec<Column> {
    vec![
        Column::new("ix", "#")
            .width(px(40.0))
            .text_right()
            .movable(false)
            .resizable(false),
        Column::new("icon", "")
            .width(px(24.0))
            .movable(false)
            .resizable(false),
        Column::new("name", "Bucket")
            .width(px(180.0))
            .sortable()
            .movable(false),
        Column::new("region", "Region")
            .width(px(140.0))
            .sortable()
            .movable(false),
        Column::new("storage_type", "Storage type")
            .width(px(120.0))
            .movable(false),
        // 统计列（get_bucket_stat）：Objects 是对象总数，Total 是五种类型
        // 存储量的加总，其余五列是该存储类型的存储量
        // （human readable，悬浮显示精确字节）。数值右对齐
        Column::new("object_count", "Objects")
            .width(px(100.0))
            .text_right()
            .movable(false)
            .sortable(),
        Column::new("total_size", "Total")
            .width(px(100.0))
            .text_right()
            .movable(false)
            .sortable(),
        Column::new("standard", "Standard")
            .width(px(100.0))
            .text_right()
            .movable(false)
            .sortable(),
        Column::new("infrequent", "Infrequent")
            .width(px(100.0))
            .text_right()
            .movable(false)
            .sortable(),
        Column::new("archive", "Archive")
            .width(px(100.0))
            .text_right()
            .movable(false)
            .sortable(),
        Column::new("cold_archive", "Cold Archive")
            .width(px(120.0))
            .text_right()
            .movable(false)
            .sortable(),
        Column::new("deep_cold_archive", "Deep Cold Archive")
            .width(px(150.0))
            .text_right()
            .movable(false)
            .sortable(),
        Column::new("actions", "").width(px(60.0)).movable(false),
    ]
}

/// Columns 按钮里可勾选显示/隐藏的列（列 key, 表头名）。
/// 只放统计列：其余列是表格的骨架（名称、操作、序号），藏掉没意义
const TOGGLEABLE_COLUMNS: [(&str, &str); 7] = [
    ("object_count", "Objects"),
    ("total_size", "Total"),
    ("standard", "Standard"),
    ("infrequent", "Infrequent"),
    ("archive", "Archive"),
    ("cold_archive", "Cold Archive"),
    ("deep_cold_archive", "Deep Cold Archive"),
];

pub struct BucketTableDelegate {
    /// This is all data
    rows: Vec<BucketSummary>,

    /// 统计缓存快照：渲染和排序都读它。列表加载时从全局缓存填充，
    /// 统计异步到达时增量更新（并重排序）
    stats: HashMap<String, BucketStat>,

    /// This are rows after applying filter
    filtered_indexes: Vec<usize>,
    loading: bool,
    columns: Vec<Column>,
    search: String,
    current_sort: Option<(usize, ColumnSort)>,
    bucket_list_panel: WeakEntity<BucketListPanel>,
}

impl BucketTableDelegate {
    pub fn new(
        bucket_list_panel: WeakEntity<BucketListPanel>,
        saved_widths: Option<HashMap<String, f32>>,
        hidden_columns: &HashSet<String>,
    ) -> Self {
        let mut columns = bucket_columns();

        // 恢复用户调整过的列宽（按列 key 匹配，列增删不受影响）
        if let Some(widths) = &saved_widths {
            for col in &mut columns {
                if let Some(w) = widths.get(col.key.as_ref()) {
                    col.width = px(*w);
                }
            }
        }

        // 恢复用户隐藏的列（Columns 按钮）
        columns.retain(|col| !hidden_columns.contains(col.key.as_ref()));

        Self {
            rows: Vec::new(),
            stats: HashMap::new(),
            filtered_indexes: Vec::new(),
            loading: true, // 首屏直接骨架屏
            bucket_list_panel,
            columns,
            search: String::new(),
            current_sort: None,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<BucketSummary>) {
        self.rows = rows;
        self.loading = false;
        self.recompute();
    }

    /// 列表加载时用全局缓存快照填充（缓存里可能有上次访问拉到的值）
    fn set_stats(&mut self, stats: HashMap<String, BucketStat>) {
        self.stats = stats;
    }

    /// 单个 bucket 的统计到达：更新并重排序（当前排序键可能正是统计列）
    fn insert_stat(&mut self, bucket: String, stat: BucketStat) {
        self.stats.insert(bucket, stat);
        self.recompute();
    }

    /// 切换列的显示/隐藏：按当前宽度重建列列表，再摘掉隐藏的列。
    ///
    /// 排序状态一并清掉 —— 列下标变了，之前的排序键已经不指向同一列了
    fn set_hidden_columns(&mut self, hidden: &HashSet<String>) {
        // 宽度以 delegate 为准（拖拽后已写回），这样隐藏/显示来回切不会丢宽度
        let widths: HashMap<SharedString, Pixels> = self
            .columns
            .iter()
            .map(|col| (col.key.clone(), col.width))
            .collect();

        let mut columns = bucket_columns();
        for col in &mut columns {
            if let Some(width) = widths.get(&col.key) {
                col.width = *width;
            }
        }
        columns.retain(|col| !hidden.contains(col.key.as_ref()));

        self.columns = columns;
        self.current_sort = None;
        self.recompute();
    }

    /// Objects 列：对象总数
    fn stat_count_cell(&self, bucket: &str) -> Div {
        let text = match self.stats.get(bucket) {
            Some(stat) => format_count(stat.object_count),
            None => "-".to_string(),
        };

        stat_cell_div(text)
    }

    /// 存储量列：human readable（如 "1.5 GB"），悬浮用 HoverCard 显示精确字节数。
    /// 普通 div 挂不了 .tooltip()（ManagedTooltipExt 是 crate 私有），HoverCard
    /// 走 deferred 层绘制，不会被表格裁掉（job.rs 的错误卡片同款做法）。
    ///
    /// 垂直居中放在**外层** div 上：HoverCard 的 trigger 会被包一层无高度的
    /// `div#trigger`，size_full 的单元格放在里面会失效、贴顶
    fn stat_size_cell(&self, bucket: &str, col_key: &str, pick: fn(&BucketStat) -> u64) -> AnyElement {
        let Some(bytes) = self.stats.get(bucket).map(pick) else {
            return stat_cell_div("-".to_string()).into_any_element();
        };

        div()
            .size_full()
            .v_flex()
            .justify_center()
            .child(
                // id 用 bucket + 列 key：同一行里各列各一张卡片，
                // 用列 key 而不是下标，隐藏列后也不会串
                HoverCard::new(SharedString::from(format!("{bucket}:{col_key}")))
                    // 这几列靠右，卡片往左下展开才不会顶出窗口
                    .anchor(Anchor::TopRight)
                    .trigger(
                        div()
                            .text_sm()
                            .text_align(TextAlign::Right)
                            .child(format_file_size(bytes)),
                    )
                    .content(move |_, _, _| {
                        div()
                            .text_sm()
                            .child(format!("{} bytes", format_count(bytes)))
                    }),
            )
            .into_any_element()
    }

    pub fn extend_rows(&mut self, rows: Vec<BucketSummary>) {
        self.rows.extend(rows);
        self.loading = false;
        self.recompute();
    }

    pub fn row(&self, ix: usize) -> Option<&BucketSummary> {
        self.filtered_indexes
            .get(ix)
            .and_then(|ix| self.rows.get(*ix))
    }

    /// 统计列的排序键（按 rows 的原始顺序）。统计还没到的按 0 排
    /// —— 列表刚出来时它们都在路上
    fn stat_sort_keys(&self, pick: fn(&BucketStat) -> u64) -> Vec<u64> {
        self.rows
            .iter()
            .map(|b| self.stats.get(&b.name).map(pick).unwrap_or(0))
            .collect()
    }

    /// 唯一的派生状态重算入口。
    /// filtered_indexes = f(rows, search, current_sort)，任何输入变化后调用一次。
    fn recompute(&mut self) {
        let needle = self.search.trim().to_ascii_lowercase();
        self.filtered_indexes = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, b)| needle.is_empty() || b.name.to_ascii_lowercase().contains(&needle))
            .map(|(ix, _)| ix)
            .collect();

        let Some((col_ix, sort)) = self.current_sort else {
            return; // 没有排序 → 自然序，完事
        };
        let desc = sort == ColumnSort::Descending;

        // 同样按列 key 分发（列可被隐藏，下标不可靠）
        let col_key = self.columns.get(col_ix).map(|col| col.key.clone());

        // 统计列的排序键提前取出来：sort_by 的闭包已经可变借用了
        // filtered_indexes，闭包里不能再整借 self 调方法，只能直接读字段
        let stat_keys: Option<Vec<u64>> = match col_key.as_deref() {
            Some("object_count") => Some(self.stat_sort_keys(|s| s.object_count)),
            Some("total_size") => Some(self.stat_sort_keys(total_storage)),
            Some("standard") => Some(self.stat_sort_keys(|s| s.standard_storage)),
            Some("infrequent") => Some(self.stat_sort_keys(|s| s.infrequent_access_storage)),
            Some("archive") => Some(self.stat_sort_keys(|s| s.archive_storage)),
            Some("cold_archive") => Some(self.stat_sort_keys(|s| s.cold_archive_storage)),
            Some("deep_cold_archive") => {
                Some(self.stat_sort_keys(|s| s.deep_cold_archive_storage))
            }
            _ => None,
        };

        self.filtered_indexes.sort_by(|&a, &b| {
            let o = match col_key.as_deref() {
                Some("name") => self.rows[a].name.cmp(&self.rows[b].name),
                Some("region") => self.rows[a].region.cmp(&self.rows[b].region),
                _ => match &stat_keys {
                    Some(keys) => keys[a].cmp(&keys[b]),
                    None => std::cmp::Ordering::Equal,
                },
            };
            if desc { o.reverse() } else { o }
        });
    }

    fn apply_filter(&mut self, needle: &str) {
        self.search = needle.to_string();
        self.recompute();
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
        let Some(row) = self.row(row_ix) else {
            return div().into_any_element();
        };

        // 按列 key 分发（不是列下标）：Columns 按钮能隐藏列，下标会漂移。
        // key 是 SharedString，克隆一下便宜，也免得借住 self.columns
        let col_key = self.columns[col_ix].key.clone();

        match col_key.as_ref() {
            "ix" => div()
                .size_full()
                .h_flex()
                .items_end()
                .child(
                    div()
                        .size_full()
                        .v_flex()
                        .justify_center()
                        .text_sm()
                        .child(format!("{}", row_ix + 1)),
                )
                .into_any_element(),
            "icon" => div()
                .size_full()
                .h_flex()
                .items_center()
                .child(
                    Icon::default()
                        .path("icons/bucket.svg")
                        .size_4()
                        .text_color(get_color(row, cx)),
                )
                .into_any_element(),
            "name" => {
                let panel = self.bucket_list_panel.clone();
                let name = row.name.clone();
                let region = row.region.clone();

                div()
                    .size_full()
                    .h_flex()
                    .items_start()
                    .child(
                        Button::new(format!("bucket-{}-button", row.name))
                            .h_full()
                            .text()
                            .text_sm()
                            .child(row.name.clone())
                            .on_click(move |_, window, cx| {
                                println!("bucket: {name} is clicked");
                                panel
                                    .update(cx, |panel, cx| {
                                        panel.goto_bucket(&name, &region, window, cx)
                                    })
                                    .ok();
                            }),
                    )
                    .into_any_element()
            }
            "region" => div()
                .size_full()
                .text_sm()
                .v_flex()
                .justify_center()
                .child(row.region.clone())
                .into_any_element(),
            "storage_type" => div()
                .size_full()
                .text_sm()
                .v_flex()
                .justify_center()
                .child(row.storage_class.to_string())
                .into_any_element(),
            "object_count" => self.stat_count_cell(&row.name).into_any_element(),
            "total_size" => self.stat_size_cell(&row.name, "total_size", total_storage),
            "standard" => self.stat_size_cell(&row.name, "standard", |s| s.standard_storage),
            "infrequent" => {
                self.stat_size_cell(&row.name, "infrequent", |s| s.infrequent_access_storage)
            }
            "archive" => self.stat_size_cell(&row.name, "archive", |s| s.archive_storage),
            "cold_archive" => {
                self.stat_size_cell(&row.name, "cold_archive", |s| s.cold_archive_storage)
            }
            "deep_cold_archive" => {
                self.stat_size_cell(&row.name, "deep_cold_archive", |s| s.deep_cold_archive_storage)
            }
            "actions" => {
                let panel = self.bucket_list_panel.clone();
                let name = row.name.clone();
                let region = row.region.clone();
                div()
                    .size_full()
                    .v_flex()
                    .justify_center()
                    .child(
                        Button::new(format!("bucket-{}-info-button", row.name))
                            .tooltip("Bucket detail")
                            .icon(IconName::Info)
                            .ghost()
                            .rounded_full()
                            .compact()
                            .on_click(move |_, window, cx| {
                                panel
                                    .update(cx, |panel, cx| {
                                        panel.show_bucket_detail(&name, &region, window, cx)
                                    })
                                    .ok();
                            }),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.current_sort = Some((col_ix, sort));
        self.recompute();
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
            .text_sm()
            .v_flex()
            .justify_center()
            .child(div().text_align(col.align).child(col.name.clone()))
    }
}

pub struct BucketListPanel {
    main_view: WeakEntity<MainView>,
    ossclient: Arc<Client>,
    load_state: LoadState,
    buckets_state: Entity<TableState<BucketTableDelegate>>,
    load_task: Task<()>,
    search_state: Entity<InputState>,
    is_truncated: bool,
    next_marker: Option<String>,
    /// 正在拉统计的 bucket（防止同一 bucket 重复发请求）
    stats_inflight: HashSet<String>,
    _subs: Vec<Subscription>,
}

impl BucketListPanel {
    pub fn new(
        main_view: WeakEntity<MainView>,
        ossclient: Arc<Client>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search")
                .clean_on_escape()
        });

        let search_sub = cx.subscribe(&search_state, |view, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let s = state.read(cx).value();
                view.buckets_state.update(cx, |state, cx| {
                    state.delegate_mut().apply_filter(&s);
                    cx.notify();
                });
            }
        });

        let this_weak = cx.weak_entity();
        let saved_widths = cx.global::<AppSettings>().bucket_column_widths.clone();
        let hidden_columns = cx
            .global::<AppSettings>()
            .bucket_hidden_columns
            .clone()
            .unwrap_or_default();
        let buckets_state = cx.new(|cx| {
            TableState::new(
                BucketTableDelegate::new(this_weak, saved_widths, &hidden_columns),
                window,
                cx,
            )
            .row_selectable(true)
            .col_selectable(false)
            .cell_selectable(false)
        });

        // 用户拖完列宽（或双击 fit）时把当前宽度存进设置，下次打开恢复
        let widths_sub = cx.subscribe(&buckets_state, |_view, state, event: &TableEvent, cx| {
            let TableEvent::ColumnWidthsChanged(widths) = event else {
                return;
            };

            // 列 key → 宽度（事件给的是按列序的 Vec）。
            // 顺手写回 delegate 的列：隐藏/显示列时要拿它当宽度基准
            let map: HashMap<String, f32> = state.update(cx, |state, _| {
                let delegate = state.delegate_mut();
                for (col, width) in delegate.columns.iter_mut().zip(widths.iter()) {
                    col.width = *width;
                }

                delegate
                    .columns
                    .iter()
                    .zip(widths.iter())
                    .map(|(col, w)| (col.key.to_string(), f32::from(*w)))
                    .collect()
            });

            // 合并而不是覆盖：当前隐藏着的列不在 map 里，不能把它们的宽度冲掉
            let settings = cx.global_mut::<AppSettings>();
            settings
                .bucket_column_widths
                .get_or_insert_with(HashMap::new)
                .extend(map);
            settings.save();
        });

        let this = Self {
            main_view,
            ossclient,
            load_state: LoadState::Idle,
            buckets_state,
            load_task: Task::ready(()), // 占位，load_buckets 里会替换
            search_state,
            is_truncated: false,
            next_marker: None,
            stats_inflight: HashSet::new(),
            _subs: vec![search_sub, widths_sub],
        };

        cx.on_next_frame(window, |this, _, cx| this.load_buckets(false, cx));

        this
    }

    /// Load buckets
    ///
    /// if `extend_mode` is set to `true`, new data will be appended into existing data
    fn load_buckets(&mut self, extend_mode: bool, cx: &mut Context<Self>) {
        if self.load_state == LoadState::Loading {
            return;
        }

        self.load_state = LoadState::Loading;
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
                            // 统计快照先填上：缓存里可能有上次访问拉到的值，
                            // 首帧就能显示，不用等这次请求回来
                            let stats = cx.global::<BucketStatsCache>().snapshot();
                            state.delegate_mut().set_stats(stats);

                            if extend_mode {
                                state.delegate_mut().extend_rows(buckets);
                            } else {
                                state.delegate_mut().set_rows(buckets);
                            }

                            cx.notify();
                        });
                        view.load_state = LoadState::Loaded;
                        // 列表出来了，顺手把还没缓存的 bucket 统计补上
                        view.fetch_missing_stats(cx);
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        view.load_state = LoadState::Failed;
                        window.push_notification((NotificationType::Error, msg), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// 为列表里还没有统计缓存的 bucket 拉取统计。
    ///
    /// - 缓存命中不发请求；正在飞的（`stats_inflight`）也不重复发
    /// - 每个 bucket 用它自己 region 的 client（统计是 bucket 级请求）
    /// - 失败静默：列里保持 "-"，下次刷新再试
    fn fetch_missing_stats(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<(String, String)> = self
            .buckets_state
            .read(cx)
            .delegate()
            .rows
            .iter()
            .filter(|b| {
                cx.global::<BucketStatsCache>().get(&b.name).is_none()
                    && !self.stats_inflight.contains(&b.name)
            })
            .map(|b| (b.name.clone(), b.region.clone()))
            .collect();

        if missing.is_empty() {
            return;
        }

        let handle = tokio_runtime().handle().clone();

        for (name, region) in missing {
            self.stats_inflight.insert(name.clone());

            let client = Arc::new(self.ossclient.clone_to(&region, oss_endpoint(&region)));
            let handle = handle.clone();

            cx.spawn(async move |this, cx| {
                let join = handle.spawn({
                    let client = client.clone();
                    let name = name.clone();
                    async move { client.get_bucket_stat(name.as_str()).await }
                });

                let _abort_on_drop = AbortOnDrop(join.abort_handle());

                let result = match join.await {
                    Ok(Ok(v)) => Ok(v),
                    Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
                    Err(join_err) => Err(anyhow::anyhow!("oss task failed: {join_err}")),
                };

                this.update(cx, |view, cx| {
                    view.stats_inflight.remove(&name);

                    if let Ok(stat) = result {
                        cx.global_mut::<BucketStatsCache>()
                            .insert(name.clone(), stat.clone());

                        view.buckets_state.update(cx, |state, cx| {
                            // delegate 里更新 + 重排序（当前排序键可能就是统计列），
                            // 统计列从 "-" 变成数值
                            state.delegate_mut().insert_stat(name, stat);
                            cx.notify();
                        });

                        // 顶部的合计块在 panel 的 render 里，表格 notify 不够
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn show_bucket_detail(
        &mut self,
        bucket_name: &str,
        region: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // get_bucket_info 是 bucket 级请求，必须用 bucket 所在 region 的 client。
        // 列表用的是账号 region 的 client（GetService 任何 endpoint 都能列），
        // 但详情跨区会报 "must be addressed using the specified endpoint"。
        let client = Arc::new(self.ossclient.clone_to(region, oss_endpoint(region)));
        let bucket_name = bucket_name.to_string();
        let detail_view =
            cx.new(|cx| BucketDetailPanel::new(client, bucket_name.clone(), window, cx));

        window.open_sheet_at(Placement::Right, cx, move |sheet, _, _| {
            sheet
                .p_0()
                .title(div().text_lg().child(bucket_name.clone()))
                .child(detail_view.clone())
        });
    }

    fn goto_bucket(
        &self,
        bucket_name: &str,
        region: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.main_view
            .update(cx, |main_view, cx| {
                main_view.goto_object_list(bucket_name.to_string(), region.to_string(), window, cx);
            })
            .ok();

        cx.notify();
    }
}

impl BucketListPanel {
    /// 已加载 bucket 的统计合计（搜索过滤不影响它——这是账号级的"总计"）
    fn stat_totals(&self, cx: &App) -> StatTotals {
        let delegate = self.buckets_state.read(cx).delegate();
        let mut t = StatTotals::default();

        for b in &delegate.rows {
            if let Some(s) = delegate.stats.get(&b.name) {
                t.objects += s.object_count;
                t.total += total_storage(s);
                t.standard += s.standard_storage;
                t.infrequent += s.infrequent_access_storage;
                t.archive += s.archive_storage;
                t.cold_archive += s.cold_archive_storage;
                t.deep_cold_archive += s.deep_cold_archive_storage;
            }
        }

        t
    }

    /// 标题下的 6 个统计块：对象总数 + 各存储类型用量。
    /// 颜色是"标准 → 深冷"的语义梯度：蓝(标准) → 青(低频) → 黄(归档) →
    /// 品红(冷归档) → 红(深冷归档)，越靠后越"冷"、检索成本越高；
    /// Objects 是总量不是存储类型，用中性前景色
    fn stat_blocks(&self, cx: &App) -> Div {
        let t = self.stat_totals(cx);
        let has_stats = !self.buckets_state.read(cx).delegate().stats.is_empty();

        // 统计还没拉回来时显示 "-"，别显示误导性的 0
        let count = |v: u64| {
            if has_stats {
                format_count(v)
            } else {
                "-".to_string()
            }
        };
        let size = |v: u64| {
            if has_stats {
                format_file_size(v)
            } else {
                "-".to_string()
            }
        };

        div()
            .w_full()
            .h_flex()
            .gap_2()
            .child(stat_block("Objects", count(t.objects), cx.theme().foreground))
            // Total 和 Objects 一样是"合计"不是存储类型，用中性色；
            // 五个类型块才是语义色梯度
            .child(stat_block("Total", size(t.total), cx.theme().foreground))
            .child(stat_block("Standard", size(t.standard), cx.theme().blue))
            .child(stat_block("Infrequent", size(t.infrequent), cx.theme().cyan))
            .child(stat_block("Archive", size(t.archive), cx.theme().yellow))
            .child(stat_block(
                "Cold Archive",
                size(t.cold_archive),
                cx.theme().magenta,
            ))
            .child(stat_block(
                "Deep Cold Archive",
                size(t.deep_cold_archive),
                cx.theme().red,
            ))
    }
}

impl BucketListPanel {
    /// 勾选/取消勾选一个列：写进设置，并让表格按新的列集合重建
    fn toggle_column(&mut self, key: &str, cx: &mut Context<Self>) {
        let hidden = {
            let settings = cx.global_mut::<AppSettings>();
            let hidden = settings.bucket_hidden_columns.get_or_insert_with(HashSet::new);
            if !hidden.remove(key) {
                hidden.insert(key.to_string());
            }
            let hidden = hidden.clone();
            settings.save();

            hidden
        };

        self.buckets_state.update(cx, |state, cx| {
            state.delegate_mut().set_hidden_columns(&hidden);
            // 列集合变了，让 TableState 重新读一遍 delegate 的列（表头/单元格都按它布局）
            state.refresh(cx);
        });

        cx.notify();
    }

    /// Columns 按钮：勾选控制统计列的显示/隐藏。
    /// 菜单闭包是 Fn（每次打开都会重跑），所以在里面现读设置拿勾选态
    fn columns_button(&self, panel: WeakEntity<Self>) -> impl IntoElement {
        Button::new("bucket-columns-button")
            .icon(IconName::Menu)
            .label("Columns")
            .tooltip("Show / hide columns")
            .dropdown_menu(move |menu, _, cx| {
                let hidden = cx
                    .global::<AppSettings>()
                    .bucket_hidden_columns
                    .clone()
                    .unwrap_or_default();

                TOGGLEABLE_COLUMNS
                    .iter()
                    .fold(menu, |menu, (key, label)| {
                        // 闭包是 Fn，捕获值不能 move 进 on_click，每轮现克隆
                        let panel = panel.clone();
                        let key = *key;

                        menu.item(
                            PopupMenuItem::new(*label)
                                .checked(!hidden.contains(key))
                                .on_click(move |_, _, cx| {
                                    panel
                                        .update(cx, |panel, cx| panel.toggle_column(key, cx))
                                        .ok();
                                }),
                        )
                    })
            })
    }
}

impl Render for BucketListPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .gap_2()
            .child(div().text_3xl().child("Buckets"))
            .child(self.stat_blocks(cx))
            .child(
                div()
                    .mt_2()
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
                            .loading(self.load_state == LoadState::Loading)
                            .icon(IconName::RefreshCw)
                            .on_click(cx.listener(|view, _, _, cx| {
                                // 统计缓存一并清掉，随列表重载重新拉
                                cx.global_mut::<BucketStatsCache>().clear();
                                view.stats_inflight.clear();
                                view.load_buckets(false, cx);
                            })),
                    )
                    .child(
                        Button::new("load-more-buckets-button")
                            .label("Load more")
                            .loading(self.load_state == LoadState::Loading)
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
                    )
                    .child(self.columns_button(cx.weak_entity())),
            )
            .child(
                DataTable::new(&self.buckets_state)
                    .stripe(false)
                    .header_row_height(px(40.0))
                    .with_size(px(40.0))
                    .scrollbar_visible(true, true),
            )
    }
}

struct BucketDetailPanel {
    ossclient: Arc<Client>,
    bucket_name: String,
    load_task: Task<()>,
    load_state: LoadState,
    bucket_detail: Option<BucketDetail>,
}

impl BucketDetailPanel {
    fn new(
        ossclient: Arc<Client>,
        bucket_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let this = Self {
            ossclient,
            bucket_name,
            load_task: Task::ready(()),
            load_state: LoadState::Idle,
            bucket_detail: None,
        };

        cx.on_next_frame(window, |this, _, cx| this.load_detail(cx));

        this
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        if self.load_state == LoadState::Loading {
            return;
        }

        self.load_state = LoadState::Loading;
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
                        view.load_state = LoadState::Loaded;
                    }
                    Err(e) => {
                        window.push_notification((NotificationType::Error, e.to_string()), cx);
                        view.load_state = LoadState::Failed;
                    }
                }

                cx.notify();
            })
            .ok();
        });
    }

    fn render_detail(&self) -> Div {
        if let Some(d) = &self.bucket_detail {
            // 值这一列用 element 形式自己控制字号 —— 默认字号偏大。
            // DescriptionText 只有 From<AnyElement>，div 要 into_any_element() 过一道
            let sm = |text: String| div().text_sm().child(text).into_any_element();

            div().child(
                DescriptionList::new()
                    .columns(1)
                    .children([
                        DescriptionItem::new("Name").value(sm(d.name.to_string())),
                        DescriptionItem::new("Location").value(sm(d.location.to_string())),
                        DescriptionItem::new("Storage").value(sm(d.storage_class.to_string())),
                        DescriptionItem::new("Created at")
                            .value(sm(format_datetime(&d.creation_date))),
                        DescriptionItem::new("Acl").value(sm(
                            d.access_control_list
                                .iter()
                                .map(|acl| acl.to_string())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )),
                    ]),
            )
        } else {
            div().text_center().child("Something went wrong...")
        }
    }
}

impl Render for BucketDetailPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .p_4()
            .child(match self.load_state {
                LoadState::Idle => div(),
                LoadState::Loading => div()
                    .h_flex()
                    .justify_center()
                    .child(ProgressCircle::new("bucket-detail-loading")),
                LoadState::Loaded => self.render_detail(),
                LoadState::Failed => div().child("Failed"),
            })
    }
}
