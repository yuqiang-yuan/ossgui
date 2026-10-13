use std::fs;

use gpui_kit::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, Task, WeakEntity, Window,
    accesskit::Uuid,
    assets::IconName,
    base::{IndexPath, StyledExt, input::InputState},
    component::{
        ActiveTheme, Icon, WindowExt,
        button::{Button, ButtonVariants},
        input::Input,
        list::{List, ListDelegate, ListItem, ListState},
        menu::DropdownMenu,
        notification::NotificationType,
        select::{SearchableVec, Select, SelectState},
    },
    div,
};
use serde::{Deserialize, Serialize};

use crate::{
    actions::DeleteAccountAction, common::oss_region_map, globals::APP_ID, main_view::MainView,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    id: String,
    pub access_key_id: String,
    pub region: String,
    pub remark: Option<String>,
}

impl Account {
    pub fn new(access_key_id: String, region: String, remark: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            access_key_id,
            region,
            remark,
        }
    }

    pub fn get_id(&self) -> &String {
        &self.id
    }

    pub fn save_secret(
        &self,
        access_key_secret: &str,
        cx: &mut App,
    ) -> Task<Result<(), anyhow::Error>> {
        cx.write_credentials(
            format!("ossgui://accounts/{}", self.id).as_str(),
            &self.access_key_id,
            access_key_secret.as_bytes(),
        )
    }

    pub fn load_secret(
        &self,
        cx: &mut App,
    ) -> Task<Result<Option<(String, Vec<u8>)>, anyhow::Error>> {
        cx.read_credentials(format!("ossgui://accounts/{}", self.id).as_str())
    }

    pub fn delete_secret(&self, cx: &mut App) -> Task<Result<(), anyhow::Error>> {
        cx.delete_credentials(format!("ossgui://accounts/{}", self.id).as_str())
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AccountSettings {
    pub last_used: Option<String>,
    pub accounts: Vec<Account>,
}

impl AccountSettings {
    pub fn load() -> Self {
        let Some(path) = dirs::config_local_dir() else {
            return Self::default();
        };

        let file_path = path.join(APP_ID).join("accounts.json");

        let Ok(s) = fs::read_to_string(&file_path) else {
            return Self::default();
        };

        serde_json::from_str::<Self>(&s).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = dirs::config_local_dir() else {
            return;
        };

        let file_path = path.join(APP_ID).join("accounts.json");

        let Ok(s) = serde_json::to_string(self) else {
            return;
        };

        let _ = fs::write(file_path, &s);
    }
}

pub struct AccountListDelegate {
    account_settings: Entity<AccountSettings>,
    /// 列表行里的按钮要点回面板，跟 BucketTableDelegate 一个路子
    account_panel: WeakEntity<AccountPanel>,
    selected_index: Option<IndexPath>,
}

impl AccountListDelegate {
    fn new(
        account_settings: Entity<AccountSettings>,
        account_panel: WeakEntity<AccountPanel>,
    ) -> Self {
        Self {
            account_settings,
            account_panel,
            selected_index: None,
        }
    }
}

impl ListDelegate for AccountListDelegate {
    type Item = ListItem;

    fn items_count(&self, _: usize, cx: &App) -> usize {
        self.account_settings.read(cx).accounts.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let Some(row) = self.account_settings.read(cx).accounts.get(ix.row) else {
            return None;
        };

        Some(
            ListItem::new(format!("account-{}", row.id)).child(
                div()
                    .size_full()
                    .h_flex()
                    .gap_1()
                    .child(
                        div()
                            .min_w_0()
                            .flex_grow_1()
                            .child(div().child(row.access_key_id.clone()).text_ellipsis())
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().secondary_foreground)
                                    .text_ellipsis()
                                    .child(
                                        row.remark
                                            .as_ref()
                                            .map(|s| s.to_string())
                                            .unwrap_or(" ".to_string()),
                                    ),
                            ),
                    )
                    .child({
                        let panel = self.account_panel.clone();
                        let id = row.get_id().clone();
                        Button::new(format!("use-account-{}", row.id))
                            .tooltip("Use this account")
                            .ghost()
                            .icon(Icon::new(IconName::ArrowRight).text_color(cx.theme().primary))
                            .rounded_full()
                            .on_click(move |_, window, cx| {
                                panel
                                    .update(cx, |panel, cx| panel.use_account(&id, window, cx))
                                    .ok();
                            })
                    })
                    .child({
                        let danger = cx.theme().danger;
                        let account_id = row.get_id().clone();
                        Button::new(format!("more-account-{}", row.id))
                            .tooltip("More actions")
                            .icon(IconName::CircleEllipsis)
                            .ghost()
                            .rounded_full()
                            .dropdown_caret(false)
                            .dropdown_menu(move |menu, _, _| {
                                let id = account_id.clone();
                                menu.menu_element_with_icon(
                                    Icon::new(IconName::Trash).text_color(danger),
                                    Box::new(DeleteAccountAction { id }),
                                    |_, cx| div().text_color(cx.theme().danger).child("Delete"),
                                )
                            })
                    }),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
    }
}

pub struct AccountPanel {
    main_view: WeakEntity<MainView>,
    account_settings: Entity<AccountSettings>,
    accounts_state: Entity<ListState<AccountListDelegate>>,
    aid_state: Entity<InputState>,
    asec_state: Entity<InputState>,
    regions_state: Entity<SelectState<SearchableVec<String>>>,
    remark_state: Entity<InputState>,
}

impl AccountPanel {
    pub fn new(
        main_view: WeakEntity<MainView>,
        account_settings: Entity<AccountSettings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 面板还不存在，但 cx.weak_entity() 已经能给出句柄 ——
        // 委托里的按钮靠它点回面板
        let this_weak = cx.weak_entity();

        let account_list_delegate = AccountListDelegate::new(account_settings.clone(), this_weak);

        let mut regions = oss_region_map()
            .iter()
            .map(|(k, _)| k.to_string())
            .collect::<Vec<_>>();
        regions.sort();

        Self {
            main_view,
            account_settings,
            accounts_state: cx.new(|cx| ListState::new(account_list_delegate, window, cx)),
            aid_state: cx.new(|cx| InputState::new(window, cx)),
            asec_state: cx.new(|cx| InputState::new(window, cx)),
            regions_state: cx.new(|cx| {
                SelectState::new(SearchableVec::new(regions), None, window, cx).searchable(true)
            }),
            remark_state: cx.new(|cx| InputState::new(window, cx)),
        }
    }

    pub fn add_account(
        &mut self,
        account: Account,
        asec: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 1. 此时还能借用 App，先把 Task 拿到手
        let save_secret_task = account.save_secret(&asec, cx);
        let handle = window.window_handle();
        let account_settings = self.account_settings.clone();

        // 2. spawn 只负责 await，用 GPUI 递进来的 AsyncApp
        cx.spawn(async move |_this, cx| match save_secret_task.await {
            Ok(()) => {
                account_settings.update(cx, |settings, cx| {
                    settings.accounts.push(account);
                    settings.save();
                    cx.notify();
                });
            }
            Err(err) => {
                let _ = cx.update(|cx| {
                    cx.update_window(handle, |_, window, cx| {
                        window.push_notification(
                            (
                                NotificationType::Error,
                                format!("Save access key secret failed: {err:#}"),
                            ),
                            cx,
                        );
                    })
                });
            }
        })
        .detach();
    }

    /// 选定账号：把密钥从钥匙串读出来，交给 MainView 建会话。
    ///
    /// 读钥匙串是异步的（Linux 走 D-Bus，macOS 可能弹授权框），拿不到同步结果，
    /// 所以这里读完再回主线程切场景 —— 这是唯一一处会创建 OSS client 的入口。
    fn use_account(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self
            .account_settings
            .read(cx)
            .accounts
            .iter()
            .find(|a| a.get_id() == id)
            .cloned()
        else {
            return;
        };

        let load_task = account.load_secret(cx);
        let window_handle = window.window_handle();
        let main_view = self.main_view.clone();

        cx.spawn(async move |_, cx| {
            // 三种失败收敛成一条文案，后面只处理一次
            let outcome = match load_task.await {
                Ok(Some((_username, secret))) => String::from_utf8(secret).map_err(|_| {
                    "The stored secret is not valid UTF-8. Delete and re-create this account."
                        .to_string()
                }),
                // Ok(None) 是"没存过"，不是错误：首次启动、用户清过钥匙串、
                // 换机器迁移都会走这条路
                Ok(None) => Err(
                    "No secret stored for this account. Delete and re-create it to set one."
                        .to_string(),
                ),
                Err(err) => Err(format!("Failed to read the secret: {err:#}")),
            };

            match outcome {
                Ok(secret) => {
                    let _ = cx.update(|cx| {
                        cx.update_window(window_handle, |_, window, cx| {
                            main_view
                                .update(cx, |main_view, cx| {
                                    main_view.activate_account(account, secret, window, cx)
                                })
                                .ok();
                        })
                    });
                }
                Err(message) => {
                    let _ = cx.update(|cx| {
                        cx.update_window(window_handle, |_, window, cx| {
                            window.push_notification((NotificationType::Error, message), cx);
                        })
                    });
                }
            }
        })
        .detach();
    }

    fn on_delete_account_action(
        &mut self,
        action: &DeleteAccountAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let account_settings = self.account_settings.clone();
        let Some(account) = account_settings
            .read(cx)
            .accounts
            .iter()
            .find(|a| a.id == action.id)
            .cloned()
        else {
            return;
        };

        let delete_secret_task = account.delete_secret(cx);

        let window_handle = window.window_handle();

        // 主操作：从列表移除 + 落盘。同步做完，不用等钥匙串。
        account_settings.update(cx, |settings, cx| {
            settings.accounts.retain(|a| a.get_id() != account.get_id());
            if settings.last_used.as_deref() == Some(account.get_id().as_str()) {
                settings.last_used = None;
            }
            settings.save();
            cx.notify();
        });

        cx.spawn(async move |_, cx|  {
            if let Err(err) = delete_secret_task.await {
                let _ = cx.update(|cx| {
                    cx.update_window(window_handle, |_, window, cx| {
                        window.push_notification(
                            (
                                NotificationType::Error,
                                format!(
                                    "Account removed, but deleting its keychain entry failed: {err:#}"
                                ),
                            ),
                            cx,
                        );
                    })
                });
            }
        }).detach();
    }
}

impl Render for AccountPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .on_action(cx.listener(Self::on_delete_account_action))
            .size_full()
            .py_4()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(div().pb_4().text_lg().child("New account"))
                    .child(div().text_sm().mt_2().child("Access key id"))
                    .child(Input::new(&self.aid_state))
                    .child(div().text_sm().mt_2().child("Access key secret"))
                    .child(Input::new(&self.asec_state))
                    .child(div().text_sm().mt_2().child("Region"))
                    .child(Select::new(&self.regions_state))
                    .child(div().text_sm().mt_2().child("Remark"))
                    .child(Input::new(&self.remark_state))
                    .child(
                        div().mt_2().w_full().h_flex().justify_end().child(
                            Button::new("add-account-button")
                                .primary()
                                .label("Create")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let aid = this.aid_state.read(cx).value();
                                    let asec = this.asec_state.read(cx).value();
                                    let region = this
                                        .regions_state
                                        .read(cx)
                                        .selected_value()
                                        .map(|s| s.to_string())
                                        .unwrap_or_default();

                                    let remark = this.remark_state.read(cx).value();
                                    if aid.trim().is_empty()
                                        || asec.trim().is_empty()
                                        || region.trim().is_empty()
                                    {
                                        window.push_notification(
                                            (
                                                NotificationType::Error,
                                                "Access key, secret and region can not be empty",
                                            ),
                                            cx,
                                        );
                                        return;
                                    }

                                    this.add_account(
                                        Account::new(
                                            aid.to_string(),
                                            region,
                                            Some(remark.to_string()),
                                        ),
                                        asec.to_string(),
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                    ),
            )
            .child(div().mt_4().child("Recent"))
            .child(
                div()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .size_full()
                    .flex_grow_1()
                    .child(List::new(&self.accounts_state).size_full()),
            )
    }
}
