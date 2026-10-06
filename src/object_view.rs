use std::sync::Arc;

use ali_oss_rs::Client;
use gpui_kit::{AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, WeakEntity, Window, assets::IconName, base::{StyledExt, input::InputState}, component::{ActiveTheme, Icon, Sizable, button::{Button, ButtonVariants}, input::Input, menu::{ContextMenuExt, DropdownMenu, PopupMenuItem}}, div};

use crate::{actions::{CopyAction, CutAction, DeleteAction, PasteAction}, main_view::MainView};

pub struct ObjectListPanel {
    ossclient: Arc<Client>,
    main_view: WeakEntity<MainView>,
    bucket_name: String,
    search_state: Entity<InputState>,
}

impl ObjectListPanel {
    pub fn new(main_view: WeakEntity<MainView>, ossclient: Arc<Client>, bucket_name: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search")
                .clean_on_escape()
        });

        let mut this = Self {
            main_view,
            ossclient,
            bucket_name: bucket_name.to_string(),
            search_state,
        };

        cx.on_next_frame(window, |this, window, cx| this.load_objects(window, cx));

        this
    }

    fn load_objects(&mut self, window: &mut Window, cx: &mut Context<Self>) {

    }

    /// Title bar for object list
    fn titlebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h_flex()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("home-button")
                    .icon(IconName::House)
                    .rounded_none()
                    .border_0()
            )
            .child(
                Button::new("back-button")
                    .icon(IconName::ArrowLeft)
                    .rounded_none()
                    .border_0()
            )
            .child(
                Button::new("forward-button")
                    .icon(IconName::ArrowRight)
                    .rounded_none()
                    .border_0()
            )
            .child(
                Button::new("refresh-button")
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
}

impl Render for ObjectListPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .gap_2()
            .child(self.titlebar(window, cx))
            .child(div().px_2().text_2xl().child("Objects"))
            .child(self.actions_bar(window, cx))
    }
}
