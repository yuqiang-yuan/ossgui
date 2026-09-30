use std::sync::Arc;

use ali_oss_rs::Client;
use gpui_kit::{Context, IntoElement, Render, WeakEntity, Window, div};

use crate::main_view::MainView;

pub struct ObjectListPanel {
    ossclient: Arc<Client>,
    main_view: WeakEntity<MainView>,
}

impl ObjectListPanel {
    pub fn new(main_view: WeakEntity<MainView>, ossclient: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            main_view,
            ossclient
        }
    }
}

impl Render for ObjectListPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
