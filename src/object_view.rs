use std::sync::Arc;

use ali_oss_rs::{Client, bucket::BucketOperations, bucket_common::ObjectSummary};
use gpui_kit::{App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Task, WeakEntity, Window, assets::IconName, base::{Checkbox, StyledExt, input::InputState}, component::{ActiveTheme, Icon, Sizable, WindowExt, button::{Button, ButtonVariants}, input::Input, menu::DropdownMenu, notification::NotificationType, table::{Column, ColumnSort, DataTable, TableDelegate, TableState}}, div, px};

use crate::{actions::{CopyAction, CutAction, DeleteAction, PasteAction}, common::{AbortOnDrop, LoadState, oss_region_map, tokio_runtime}, main_view::MainView};

pub struct ObjectListPanel {
    ossclient: Arc<Client>,
    main_view: WeakEntity<MainView>,
    bucket_name: String,
    search_state: Entity<InputState>,
    load_state: LoadState,
    objects_state: Entity<TableState<ObjectTableDelegate>>,
    load_task: Task<()>,
}

impl ObjectListPanel {
    pub fn new(main_view: WeakEntity<MainView>, ossclient: Arc<Client>, bucket_name: String, region: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search")
                .clean_on_escape()
        });

        let this_weak = cx.weak_entity();
        let endpoint = oss_region_map().get(region.as_str()).map(|r| r.to_string()).unwrap_or(format!("oss-{}.aliyuncs.com", region));

        let mut this = Self {
            main_view,
            ossclient: Arc::new(ossclient.clone_to(region, endpoint)),
            bucket_name: bucket_name.to_string(),
            search_state,
            load_state: LoadState::Idle,
            load_task: Task::ready(()),
            objects_state: cx.new(|cx| TableState::new(ObjectTableDelegate::new(this_weak), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false))
        };

        cx.on_next_frame(window, |this, window, cx| this.load_objects(window, cx));

        this
    }

    fn load_objects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.load_state, LoadState::Loading) {
            return;
        }

        self.load_state = LoadState::Loading;
        cx.notify();

        let client = self.ossclient.clone();
        let handle = tokio_runtime().handle().clone();
        let bucket_name = self.bucket_name.clone();

        self.load_task = cx.spawn(async move |this, cx| {
            let join = handle.spawn(async move {
                client.list_objects(bucket_name, None).await
            });

            let _abort_on_drop = AbortOnDrop(join.abort_handle());
            let result = match join.await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
                Err(join_err) => Err(anyhow::anyhow!("oss task failed: {join_err}")),
            };

            this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(results) => {
                        view.objects_state.update(cx, |state, cx| {
                            state.delegate_mut().set_rows(results.contents);
                            cx.notify();
                        });
                    },
                    Err(e) => {
                        let msg = e.to_string();
                        view.load_state = LoadState::Failed;
                        window.push_notification((NotificationType::Error, msg), cx);
                    }
                }
                cx.notify();
            }).ok();
        });
    }

    /// Title bar for object list
    fn navbar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h_flex()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                {
                    let main_view = self.main_view.clone();
                    Button::new("home-button")
                        .tooltip("Goto bucket list")
                        .icon(IconName::House)
                        .rounded_none()
                        .border_0()
                        .on_click(move |_, window, cx| {
                            main_view.update(cx, |view, cx| {
                                view.goto_bucket_list(window, cx);
                            }).ok();
                        })
                }
            )
            .child(
                Button::new("back-button")
                    .tooltip("Backward")
                    .icon(IconName::ArrowLeft)
                    .rounded_none()
                    .border_0()
            )
            .child(
                Button::new("forward-button")
                    .tooltip("Forward")
                    .icon(IconName::ArrowRight)
                    .rounded_none()
                    .border_0()
            )
            .child(
                Button::new("refresh-button")
                    .tooltip("Refresh")
                    .icon(IconName::RefreshCw)
                    .rounded_none()
                    .border_0()
            )
            .child(
                div()
                    .ml_2()
                    .h_flex()
                    .flex_grow_1()
                    .items_center()
                    .justify_start()
                    .child(
                        Button::new("home-part-button")
                            .text()
                            .label("oss://")
                    )
            )
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
                    .label("Upload")
            )
            .child(
                Button::new("download-button")
                    .icon(Icon::default().path("icons/cloud-download.svg"))
                    .label("Download")
            )
            .child(
                Button::new("new-folder-button")
                    .icon(IconName::Plus)
                    .label("New Folder")
            )
            .child(
                Button::new("more-button")
                    .label("More")
                    .dropdown_caret(true)
                    .dropdown_menu(|menu, window, cx| {
                        menu.menu_with_icon("Copy", IconName::Copy, Box::new(CopyAction))
                            .menu_with_icon("Cut", IconName::ClipboardX, Box::new(CutAction))
                            .menu_with_icon("Paste", IconName::ClipboardPaste, Box::new(PasteAction))
                            .separator()
                            .menu_with_icon("Delete", IconName::Trash, Box::new(DeleteAction))
                    })
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
                div()
                    .px_2()
                    .flex_grow_1()
                    .child(
                        DataTable::new(&self.objects_state)
                            .stripe(false)
                            .scrollbar_visible(true, true),
                )
            )
            .child(self.paginator_bar(window, cx))
    }
}

pub struct ObjectTableDelegate {
    rows: Vec<ObjectSummary>,
    /// This are rows after applying filter
    filtered_indexes: Vec<usize>,
    loading: bool,
    columns: Vec<Column>,
    search: String,
    current_sort: Option<(usize, ColumnSort)>,
    object_list_panel: WeakEntity<ObjectListPanel>,
}

impl ObjectTableDelegate {
    pub fn new(object_list_panel: WeakEntity<ObjectListPanel>) -> Self {
        Self {
            object_list_panel,
            rows: Vec::new(),
            filtered_indexes: Vec::new(),
            loading: false,
            search: String::new(),
            current_sort: None,
            columns: vec![
                Column::new("ck", "")
                    .width(px(60.0))
                    .movable(false),
                Column::new("name", "Name")
                    .width(px(140.0))
                    .movable(false)
                    .sortable()
            ],
        }
    }

    /// 由 MainView 调用；内部不 notify，通知由调用方统一发
    pub fn set_rows(&mut self, rows: Vec<ObjectSummary>) {
        self.rows = rows;
        self.loading = false;
        // self.recompute();
    }

    pub fn row(&self, ix: usize) -> Option<&ObjectSummary> {
        self.filtered_indexes
            .get(ix)
            .map(|ix| self.rows.get(*ix))
            .flatten()
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
            return div().into_any_element();
        };

        match col_ix {
            0 => Checkbox::new(format!("ck-{}", row.key)).into_any_element(),
            1 => div().child(row.key.clone()).into_any_element(),
            _ => div().into_any_element()
        }
    }
}
