use std::{cmp::Ordering, sync::Arc};

use ali_oss_rs::{
    Client,
    bucket::BucketOperations,
    bucket_common::{ListObjectsOptionsBuilder, ListObjectsResult, ObjectSummary},
};
use gpui_kit::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Task, WeakEntity,
    Window,
    assets::IconName,
    base::{StyledExt, input::InputState},
    component::{
        ActiveTheme, Icon, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        input::Input,
        menu::DropdownMenu,
        notification::NotificationType,
        table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    },
    div, px,
};

use crate::{
    actions::{CopyAction, CutAction, DeleteAction, PasteAction},
    common::{AbortOnDrop, LoadState, oss_region_map, tokio_runtime},
    main_view::MainView,
};

pub struct ObjectListPanel {
    ossclient: Arc<Client>,
    main_view: WeakEntity<MainView>,
    bucket_name: String,
    prefix: String,
    search_state: Entity<InputState>,
    load_state: LoadState,
    objects_state: Entity<TableState<ObjectTableDelegate>>,
    load_task: Task<()>,
}

impl ObjectListPanel {
    pub fn new(
        main_view: WeakEntity<MainView>,
        ossclient: Arc<Client>,
        bucket_name: String,
        region: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search")
                .clean_on_escape()
        });

        let this_weak = cx.weak_entity();
        let endpoint = oss_region_map()
            .get(region.as_str())
            .map(|r| r.to_string())
            .unwrap_or(format!("oss-{}.aliyuncs.com", region));

        let this = Self {
            main_view,
            ossclient: Arc::new(ossclient.clone_to(region, endpoint)),
            bucket_name: bucket_name.to_string(),
            prefix: String::new(),
            search_state,
            load_state: LoadState::Idle,
            load_task: Task::ready(()),
            objects_state: cx.new(|cx| {
                TableState::new(ObjectTableDelegate::new(this_weak), window, cx)
                    .row_selectable(true)
                    .col_selectable(false)
                    .cell_selectable(false)
            }),
        };

        cx.on_next_frame(window, |this, window, cx| this.load_objects(window, cx));

        this
    }

    fn load_objects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.load_state, LoadState::Loading) {
            return;
        }

        println!("loading objects with prefix: {}", self.prefix);

        self.load_state = LoadState::Loading;
        cx.notify();

        let client = self.ossclient.clone();
        let handle = tokio_runtime().handle().clone();
        let bucket_name = self.bucket_name.clone();
        let prefix = self.prefix.clone();

        self.load_task = cx.spawn(async move |this, cx| {
            let join = handle.spawn(async move {
                client
                    .list_objects(
                        bucket_name,
                        Some(
                            ListObjectsOptionsBuilder::new()
                                .prefix(prefix)
                                .delimiter('/')
                                .build(),
                        ),
                    )
                    .await
            });

            let _abort_on_drop = AbortOnDrop(join.abort_handle());
            let result = match join.await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
                Err(join_err) => Err(anyhow::anyhow!("oss task failed: {join_err}")),
            };

            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(results) => {
                        this.load_state = LoadState::Loaded;
                        this.objects_state.update(cx, |state, cx| {
                            let ListObjectsResult {
                                name,
                                prefix,
                                max_keys,
                                delimiter,
                                start_after,
                                is_truncated,
                                key_count,
                                continuation_token,
                                next_continuation_token,
                                common_prefixes,
                                contents,
                            } = results;

                            // common_prefixes.iter().for_each(|c| println!("prefix: {prefix}, common prefix: {c}"));
                            // contents.iter().for_each(|f| println!("prefix: {prefix}, object: {}", f.key));

                            let mut rows = common_prefixes
                                .into_iter()
                                .map(OssObjectItem::Folder)
                                .collect::<Vec<_>>();

                            // while request with prefix, there will be an item in the contents which key is the same as prefix. this will be removed before renderring
                            rows.extend(
                                contents
                                    .into_iter()
                                    .filter(|o| !prefix.is_empty() && o.key != prefix)
                                    .map(|s| OssObjectItem::File(s)),
                            );

                            state.delegate_mut().set_rows(rows);
                            state.delegate_mut().set_prefix(prefix);
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        this.load_state = LoadState::Failed;
                        window.push_notification((NotificationType::Error, msg), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Title bar for object list
    fn navbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h_flex()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child({
                let main_view = self.main_view.clone();
                Button::new("home-button")
                    .tooltip("Goto bucket list")
                    .icon(IconName::House)
                    .rounded_none()
                    .border_0()
                    .on_click(move |_, window, cx| {
                        main_view
                            .update(cx, |view, cx| {
                                view.goto_bucket_list(window, cx);
                            })
                            .ok();
                    })
            })
            .child(
                Button::new("back-button")
                    .tooltip("Backward")
                    .icon(IconName::ArrowLeft)
                    .rounded_none()
                    .border_0(),
            )
            .child(
                Button::new("forward-button")
                    .tooltip("Forward")
                    .icon(IconName::ArrowRight)
                    .rounded_none()
                    .border_0(),
            )
            .child(
                Button::new("refresh-button")
                    .tooltip("Refresh")
                    .icon(IconName::RefreshCw)
                    .rounded_none()
                    .border_0(),
            )
            .child(
                div()
                    .ml_2()
                    .h_flex()
                    .flex_grow_1()
                    .items_center()
                    .justify_start()
                    .child({
                        let main_view = self.main_view.clone();
                        Button::new("breadcrumb-home-button")
                            .text()
                            .label("oss://")
                            .on_click(move |_, window, cx| {
                                main_view
                                    .update(cx, |view, cx| {
                                        view.goto_bucket_list(window, cx);
                                    })
                                    .ok();
                            })
                    })
                    .child(
                        Button::new("breadcrumb-bucket-button")
                            .text()
                            .label(self.bucket_name.clone())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.prefix = String::new();
                                this.load_objects(window, cx);
                            })),
                    ).children(self.breadcrumb_items(window, cx)),
            )
    }

    /// Components for prefix in the breadcrumb bars
    fn breadcrumb_items(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<impl IntoElement> {
        if self.prefix.is_empty() {
            return vec![];
        }

        let mut acc = String::new();

        self.prefix
            .split("/")
            .filter(|s| !s.is_empty())
            .map(|p| {
                acc.push_str(p);
                acc.push('/');
                let prefix = acc.clone();
                Button::new(format!("breadcrumb-prefix-{}", acc))
                    .text()
                    .label(format!("/{p}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.prefix = prefix.clone();
                        this.load_objects(window, cx);
                    }))
            })
            .collect::<Vec<_>>()
    }

    fn actions_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .w_full()
            .h_flex()
            .gap_2()
            .items_center()
            .child(
                Input::new(&self.search_state)
                    .w_64()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .child(div().flex_grow_1())
            .child(
                Button::new("upload-button")
                    .icon(Icon::default().path("icons/cloud-upload.svg"))
                    .label("Upload"),
            )
            .child(
                Button::new("download-button")
                    .icon(Icon::default().path("icons/cloud-download.svg"))
                    .label("Download"),
            )
            .child(
                Button::new("new-folder-button")
                    .icon(IconName::Plus)
                    .label("New Folder"),
            )
            .child(
                Button::new("more-button")
                    .label("More")
                    .dropdown_caret(true)
                    .dropdown_menu(|menu, window, cx| {
                        menu.menu_with_icon("Copy", IconName::Copy, Box::new(CopyAction))
                            .menu_with_icon("Cut", IconName::ClipboardX, Box::new(CutAction))
                            .menu_with_icon(
                                "Paste",
                                IconName::ClipboardPaste,
                                Box::new(PasteAction),
                            )
                            .separator()
                            .menu_with_icon("Delete", IconName::Trash, Box::new(DeleteAction))
                    }),
            )
    }

    pub fn paginator_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .w_full()
            .h_flex()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
    }
}

impl Render for ObjectListPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .gap_2()
            .child(self.navbar(window, cx))
            .child(div().px_2().text_2xl().child("Objects"))
            .child(self.actions_bar(window, cx))
            .child(
                div().px_2().flex_grow_1().child(
                    DataTable::new(&self.objects_state)
                        .stripe(false)
                        .scrollbar_visible(true, true),
                ),
            )
            .child(self.paginator_bar(window, cx))
    }
}

enum OssObjectItem {
    Folder(String),
    File(ObjectSummary),
}

struct ObjectTableDelegate {
    rows: Vec<OssObjectItem>,
    prefix: String,
    /// This are rows after applying filter
    filtered_indexes: Vec<usize>,
    loading: bool,
    columns: Vec<Column>,
    search: String,
    current_sort: Option<(usize, ColumnSort)>,
    object_list_panel: WeakEntity<ObjectListPanel>,
}

impl ObjectTableDelegate {
    fn new(object_list_panel: WeakEntity<ObjectListPanel>) -> Self {
        Self {
            object_list_panel,
            rows: Vec::new(),
            prefix: String::new(),
            filtered_indexes: Vec::new(),
            loading: false,
            search: String::new(),
            current_sort: None,
            columns: vec![
                Column::new("ck", "")
                    .width(px(60.0))
                    .movable(false)
                    .resizable(false),
                Column::new("icon", "")
                    .width(px(24.0))
                    .movable(false)
                    .resizable(false),
                Column::new("name", "Name")
                    .width(px(200.0))
                    .movable(false)
                    .sortable(),
            ],
        }
    }

    fn set_rows(&mut self, rows: Vec<OssObjectItem>) {
        self.rows = rows;
        self.loading = false;
        self.recompute();
    }

    fn set_prefix(&mut self, prefix: String) {
        self.prefix = prefix;
    }

    // pub fn extend_rows(&mut self, rows: Vec<OssObjectItem>) {
    //     self.rows.extend(rows);
    //     self.loading = false;
    //     self.recompute();
    // }

    fn row(&self, ix: usize) -> Option<&OssObjectItem> {
        self.filtered_indexes
            .get(ix)
            .map(|ix| self.rows.get(*ix))
            .flatten()
    }

    fn recompute(&mut self) {
        let needle = self.search.trim().to_ascii_lowercase();

        self.filtered_indexes = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                needle.is_empty()
                    || (match o {
                        OssObjectItem::Folder(s) => s,
                        OssObjectItem::File(f) => &f.key,
                    })
                    .to_ascii_lowercase()
                    .contains(&needle)
            })
            .map(|(ix, _)| ix)
            .collect();

        let Some((col_ix, sort)) = self.current_sort else {
            return; // 没有排序 → 自然序，完事
        };

        let desc = matches!(sort, ColumnSort::Descending);
        self.filtered_indexes.sort_by(|&a, &b| {
            match col_ix {
                2 => {
                    let item_a = &self.rows[a];
                    let item_b = &self.rows[b];
                    let name_a = match item_a {
                        OssObjectItem::Folder(s) => s,
                        OssObjectItem::File(f) => &f.key,
                    };

                    let name_b = match item_b {
                        OssObjectItem::Folder(s) => s,
                        OssObjectItem::File(f) => &f.key,
                    };

                    if matches!(item_a, OssObjectItem::Folder(_))
                        && matches!(item_b, OssObjectItem::Folder(_))
                    {
                        let o = name_a.cmp(name_b);

                        if desc {
                            return o.reverse();
                        } else {
                            return o;
                        }
                    }

                    // folder always shown first
                    if matches!(item_a, OssObjectItem::Folder(_))
                        && matches!(item_b, OssObjectItem::File(_))
                    {
                        return Ordering::Less;
                    }

                    if matches!(item_a, OssObjectItem::File(_))
                        && matches!(item_b, OssObjectItem::Folder(_))
                    {
                        return Ordering::Greater;
                    }

                    if matches!(item_a, OssObjectItem::File(_))
                        && matches!(item_b, OssObjectItem::File(_))
                    {
                        let o = name_a.cmp(name_b);

                        if desc {
                            return o.reverse();
                        } else {
                            return o;
                        }
                    }

                    return Ordering::Equal;
                }
                _ => Ordering::Equal,
            }
        });
    }

    fn apply_filter(&mut self, needle: &str) {
        self.search = needle.to_string();
        self.recompute();
    }
}

impl TableDelegate for ObjectTableDelegate {
    fn columns_count(&self, cx: &gpui_kit::App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, cx: &gpui_kit::App) -> usize {
        self.filtered_indexes.len()
    }

    fn column(&self, col_ix: usize, cx: &gpui_kit::App) -> Column {
        self.columns[col_ix].clone()
    }

    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<gpui_kit::component::table::TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.row(row_ix) else {
            return div();
        };

        let key = match row {
            OssObjectItem::Folder(s) => &s,
            OssObjectItem::File(f) => &f.key,
        };

        let name = &key[self.prefix.len()..];
        let is_folder = matches!(row, OssObjectItem::Folder(_));

        match col_ix {
            0 => div()
                .size_full()
                .h_flex()
                .items_center()
                .justify_center()
                .child(Checkbox::new(format!("ck-{}", key))),
            1 => div()
                .size_full()
                .h_flex()
                .items_center()
                .justify_center()
                .child(
                    (match row {
                        OssObjectItem::Folder(_) => div().child(IconName::Folder),
                        OssObjectItem::File(_) => div().child(IconName::File),
                    })
                    .size_4(),
                ),
            2 => div().size_full().h_flex().child({
                let object_list_panel = self.object_list_panel.clone();
                let key_cloned = key.clone();
                Button::new(format!("goto-folder-button-{}", key))
                    .label(name)
                    .text()
                    .on_click(move |_, window, cx| {
                        if is_folder {
                            let key_cloned = key_cloned.clone();
                            object_list_panel
                                .update(cx, move |panel, cx| {
                                    panel.prefix = key_cloned;
                                    panel.load_objects(window, cx);
                                })
                                .ok();
                        }
                    })
            }),
            _ => div(),
        }
    }
}
