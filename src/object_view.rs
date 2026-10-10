use std::{cmp::Ordering, collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use ali_oss_rs::{
    Client,
    bucket::BucketOperations,
    bucket_common::{ListObjectsOptionsBuilder, ListObjectsResult, ObjectSummary},
    object::ObjectOperations,
    object_common::ObjectMetadata,
    presign_common::PresignGetOptionsBuilder,
};
use gpui_kit::{
    App, AppContext, Context, Div, Entity, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, Render, Styled, StyledImage, Subscription, Task, TextAlign, WeakEntity,
    Window,
    assets::IconName,
    base::{
        Disableable, IndexPath, Placement, StyledExt,
        input::{InputEvent, InputState},
    },
    component::{
        ActiveTheme, Icon, Sizable, WindowExt,
        button::{Button, ButtonVariant, ButtonVariants},
        checkbox::Checkbox,
        description_list::{DescriptionItem, DescriptionList},
        input::Input,
        menu::DropdownMenu,
        notification::NotificationType,
        progress::ProgressCircle,
        select::{Select, SelectEvent, SelectState},
        table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    },
    div, img, px,
};

use crate::{
    actions::{
        CopyAction, CutAction, DeleteAction, OpenFilesForUploadAction, OpenFolderForUploadAction,
        PasteAction,
    },
    common::{
        AbortOnDrop, LoadState, format_datetime, format_file_size, oss_region_map, tokio_runtime,
    },
    job::{JobKind, JobQueue},
    main_view::MainView,
};

pub struct ObjectListPanel {
    ossclient: Arc<Client>,
    main_view: WeakEntity<MainView>,
    bucket_name: String,
    prefix: String,
    page_size: usize,
    next_continuation_token: Option<String>,
    is_truncated: bool,
    search_state: Entity<InputState>,
    load_state: LoadState,
    objects_state: Entity<TableState<ObjectTableDelegate>>,
    load_task: Task<()>,
    page_size_state: Entity<SelectState<Vec<&'static str>>>,

    create_folder_task: Task<()>,

    /// 已经排上了一次后台刷新（去抖窗口内不再排）
    refresh_pending: bool,
    /// 刷新请求撞上了正在飞的加载 → 这次加载完再补一次
    refresh_again: bool,

    _subs: Vec<Subscription>,
}

/// 一次上传最多入队多少个文件。选中一个大目录时防止把队列和面板撑爆。
const MAX_UPLOAD_FILES: usize = 5000;

/// 任务结束后的刷新去抖窗口。
/// 传一个文件夹可能有几千个任务陆续完成，不能每个都去拉一次列表。
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(500);

/// 扫描结果：一个待上传的文件
struct UploadCandidate {
    source: PathBuf,
    /// 相对选中根的路径（已把 `\` 换成 `/`）。既用来拼 object key，
    /// 也用来在面板上显示。选中目录本身的名字会保留：
    /// `/home/me/pics/vacation/day1/a.jpg` → `vacation/day1/a.jpg`
    relative: String,
    size: u64,
}

/// 把用户选中的路径展开成待上传文件列表。
///
/// - 文件：原样收下
/// - 目录：递归展开（用显式栈，避免深目录把调用栈压爆）
/// - 符号链接和特殊文件：跳过。`DirEntry::file_type()` 不跟随符号链接，
///   所以不会传出目录树、也不会有环
///
/// 纯本地 I/O，**必须在后台执行器上调用**。
/// 返回 `(文件列表, 是否因为超过 MAX_UPLOAD_FILES 被截断)`。
fn scan_upload_paths(paths: Vec<PathBuf>) -> (Vec<UploadCandidate>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;

    for path in paths {
        if truncated {
            break;
        }

        // 用户显式选中的路径：跟随符号链接是对的
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };

        if !meta.is_dir() {
            out.push(UploadCandidate {
                relative: path
                    .file_name()
                    .map(|name| name.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default(),
                source: path,
                size: meta.len(),
            });
            continue;
        }

        let root_name = path.file_name().map(PathBuf::from).unwrap_or_default();
        let mut stack = vec![path.clone()];

        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue; // 没权限的目录直接跳过，不中断整次扫描
            };

            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let child = entry.path();

                if file_type.is_dir() {
                    stack.push(child);
                } else if file_type.is_file() {
                    let Ok(size) = entry.metadata().map(|m| m.len()) else {
                        continue; // 遍历途中被删了
                    };
                    let Ok(rel) = child.strip_prefix(&path) else {
                        continue;
                    };
                    // 先把借用用掉，下面才能把 child 移进 UploadCandidate。
                    // Windows 上 PathBuf 的分隔符是 '\'，这里统一成 OSS 的 '/'，
                    // 同时这个字符串也要拿去显示
                    let relative = root_name
                        .join(rel)
                        .to_string_lossy()
                        .replace('\\', "/");

                    out.push(UploadCandidate {
                        source: child,
                        relative,
                        size,
                    });

                    if out.len() >= MAX_UPLOAD_FILES {
                        truncated = true;
                        break;
                    }
                }
            }

            if truncated {
                break;
            }
        }
    }

    (out, truncated)
}

impl ObjectListPanel {
    pub fn new(
        main_view: WeakEntity<MainView>,
        ossclient: Arc<Client>,
        job_queue: Entity<JobQueue>,
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

        let search_sub =
            cx.subscribe(
                &search_state,
                |this, state, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let s = state.read(cx).value();
                        this.objects_state.update(cx, |state, _| {
                            state.delegate_mut().apply_filter(&s);
                        });
                    }
                    _ => {}
                },
            );

        let page_size_state = cx.new(|cx| {
            SelectState::new(
                vec!["100", "200", "500", "1000"],
                Some(IndexPath::new(0usize)),
                window,
                cx,
            )
        });

        let page_size_sub = cx.subscribe(
            &page_size_state,
            |this, _, event: &SelectEvent<Vec<&'static str>>, cx| match event {
                SelectEvent::Confirm(item) => {
                    if let Some(s) = item {
                        this.next_continuation_token = None;
                        this.page_size = usize::from_str_radix(s, 10).unwrap_or(100usize);
                        this.load_objects(cx);
                    }
                }
            },
        );

        // 任务结束时判断要不要刷新当前目录。
        // 订阅挂在面板上：面板销毁（切场景）时自动断掉
        let job_sub = cx.subscribe(&job_queue, |this, _queue, kind: &JobKind, cx| {
            if kind.affects_listing(&this.bucket_name, &this.prefix) {
                this.schedule_refresh(cx);
            }
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
            page_size: 100usize,
            next_continuation_token: None,
            is_truncated: false,
            search_state,
            load_state: LoadState::Idle,
            load_task: Task::ready(()),
            page_size_state,
            objects_state: cx.new(|cx| {
                TableState::new(ObjectTableDelegate::new(this_weak), window, cx)
                    .row_selectable(true)
                    .col_selectable(false)
                    .cell_selectable(false)
            }),
            create_folder_task: Task::ready(()),
            refresh_pending: false,
            refresh_again: false,
            _subs: vec![search_sub, page_size_sub, job_sub],
        };

        cx.on_next_frame(window, |this, _, cx| this.load_objects(cx));

        this
    }

    /// 用户主动重新拉取（翻页、切目录、改 page size）：显示加载态
    fn load_objects(&mut self, cx: &mut Context<Self>) {
        self.load_objects_inner(false, cx);
    }

    /// 任务结束后台刷新：不显示加载态，旧行保留到新数据到达
    fn refresh_objects(&mut self, cx: &mut Context<Self>) {
        self.load_objects_inner(true, cx);
    }

    /// 排一次后台刷新，去抖窗口内的多次请求合并成一次
    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_pending {
            return;
        }
        self.refresh_pending = true;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DEBOUNCE).await;

            this.update(cx, |this, cx| {
                this.refresh_pending = false;
                // 判定和拉取都发生在"现在"：这 500ms 里用户可能已经翻页或换目录了，
                // 换目录的情况由 affects_listing 在下次事件时重新判断
                this.refresh_objects(cx);
            })
            .ok();
        })
        .detach();
    }

    fn load_objects_inner(&mut self, silent: bool, cx: &mut Context<Self>) {
        if self.load_state == LoadState::Loading {
            // 加载中来的刷新请求不能丢，否则列表会一直停在旧数据
            if silent {
                self.refresh_again = true;
            }
            return;
        }

        self.load_state = LoadState::Loading;

        // 静默刷新不动 loading 标志 —— 否则表格每 500ms 闪一次骨架屏
        if !silent {
            self.objects_state
                .update(cx, |state, _| state.delegate_mut().loading = true);
        }
        cx.notify();

        let client = self.ossclient.clone();
        let handle = tokio_runtime().handle().clone();
        let bucket_name = self.bucket_name.clone();
        let prefix = self.prefix.clone();
        let next_continuation_token = self.next_continuation_token.clone();
        let max_keys = self.page_size;

        self.load_task = cx.spawn(async move |this, cx| {
            let join = handle.spawn(async move {
                let mut options = ListObjectsOptionsBuilder::new()
                    .prefix(prefix)
                    .delimiter('/')
                    .max_keys(max_keys as u32);

                if let Some(t) = next_continuation_token {
                    options = options.continuation_token(t);
                }

                client.list_objects(bucket_name, Some(options.build())).await
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
                                is_truncated,
                                key_count,
                                continuation_token,
                                next_continuation_token,
                                common_prefixes,
                                contents,
                                ..
                            } = results;

                            println!("list objects result: name: {name}, max keys: {max_keys}, key count: {key_count}, continuation token: {:?}, next continuation token: {:?}", continuation_token, next_continuation_token);

                            this.is_truncated = is_truncated;
                            this.next_continuation_token = next_continuation_token;
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
                                    .filter(|o| o.key != prefix)
                                    .map(|s| OssObjectItem::File(s)),
                            );

                            if silent {
                                // 后台刷新：勾选按 key 保留（被删掉的自然消失）
                                state.delegate_mut().refresh_rows(rows);
                            } else {
                                // 用户主动换页/换目录：清空勾选是对的
                                state.delegate_mut().set_rows(rows);
                            }
                            state.delegate_mut().set_prefix(prefix);
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        this.load_state = LoadState::Failed;
                        // 静默刷新失败就安静地算了 —— 用户没主动做什么，弹错误太突兀
                        if !silent {
                            window.push_notification(
                                (NotificationType::Error, e.to_string()),
                                cx,
                            );
                        }
                    }
                }
                this.objects_state.update(cx, |state, _| state.delegate_mut().loading = false);
                cx.notify();

                // 这次加载期间又攒了刷新请求 → 让当前任务先收尾，再补一次
                if this.refresh_again {
                    this.refresh_again = false;
                    cx.spawn(async move |this, cx| {
                        this.update(cx, |this, cx| this.refresh_objects(cx)).ok();
                    })
                    .detach();
                }
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
            // .child({
            //     let main_view = self.main_view.clone();
            //     Button::new("home-button")
            //         .tooltip("Goto bucket list")
            //         .icon(IconName::House)
            //         .rounded_none()
            //         .border_0()
            //         .on_click(move |_, window, cx| {
            //             main_view
            //                 .update(cx, |view, cx| {
            //                     view.goto_bucket_list(window, cx);
            //                 })
            //                 .ok();
            //         })
            // })
            // .child(
            //     Button::new("back-button")
            //         .tooltip("Backward")
            //         .icon(IconName::ArrowLeft)
            //         .rounded_none()
            //         .border_0(),
            // )
            // .child(
            //     Button::new("forward-button")
            //         .tooltip("Forward")
            //         .icon(IconName::ArrowRight)
            //         .rounded_none()
            //         .border_0(),
            // )
            .child(
                Button::new("refresh-button")
                    .tooltip("Refresh")
                    .icon(IconName::RefreshCw)
                    .rounded_none()
                    .border_0().on_click(cx.listener(|this, _, _, cx| {
                        this.load_objects(cx);
                    })),
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prefix = String::new();
                                this.next_continuation_token = None;
                                this.load_objects(cx);
                            })),
                    )
                    .children(self.breadcrumb_items(window, cx)),
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.prefix = prefix.clone();
                        this.load_objects(cx);
                    }))
            })
            .collect::<Vec<_>>()
    }

    fn actions_bar(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .icon(IconName::CloudUpload)
                    .label("Upload")
                    .dropdown_caret(true)
                    .dropdown_menu(|menu, _, _| {
                        menu.menu("Files", Box::new(OpenFilesForUploadAction))
                            .menu("Folders", Box::new(OpenFolderForUploadAction))
                    }),
            )
            .child(
                Button::new("download-button")
                    .icon(IconName::CloudDownload)
                    .label("Download"),
            )
            .child(
                Button::new("new-folder-button")
                    .icon(IconName::Plus)
                    .label("New Folder")
                    .on_click(cx.listener(|_, _, window, cx| {
                        let weak_this = cx.weak_entity();
                        let new_folder_panel =
                            cx.new(|cx| NewFolderPanel::new(weak_this, window, cx));

                        window.open_dialog(cx, move |dialog, _, _| {
                            let panel_for_ok = new_folder_panel.clone();
                            dialog
                                .title("New folder")
                                .child(new_folder_panel.clone())
                                .footer(
                                    div()
                                        .size_full()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("new-folder-ok-button")
                                                .primary()
                                                .label("Create")
                                                .on_click(move |_, window, cx| {
                                                    panel_for_ok.update(cx, |this, cx| {
                                                        this.on_confirmed(window, cx);
                                                    });
                                                }),
                                        )
                                        .child(
                                            Button::new("new-folder-cancel-button")
                                                .label("Cancel")
                                                .on_click(|_, window, cx| window.close_dialog(cx)),
                                        ),
                                )
                                .overlay_closable(false)
                        });
                    })),
            )
            .child(
                Button::new("more-button")
                    .label("More")
                    .dropdown_caret(true)
                    .dropdown_menu(|menu, _, _| {
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

    fn paginator_bar(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .w_full()
            .h_flex()
            .gap_1()
            .items_center()
            .justify_end()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(div().child(""))
            .child(div().text_sm().child("Max keys"))
            .child(div().w_24().child(Select::new(&self.page_size_state)))
            .child(
                Button::new("next-page-button")
                    .tooltip("Next page")
                    .icon(IconName::ChevronRight)
                    .disabled(!self.is_truncated)
                    // 用表格自己的 loading 标志，而不是 load_state：
                    // 后台静默刷新也会把 load_state 置成 Loading，但那是用户看不见的，
                    // 让这个按钮一直转圈会显得像卡住了
                    .loading(self.objects_state.read(cx).delegate().loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_objects(cx);
                    })),
            )
    }

    fn show_object_detail(
        &mut self,
        object_key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.ossclient.clone();
        let bucket_name = self.bucket_name.clone();
        let meta_panel =
            cx.new(|cx| ObjectMetaPanel::new(client, bucket_name, object_key.clone(), window, cx));

        window.open_sheet_at(Placement::Right, cx, move |sheet, _, _| {
            sheet.p_0().title("Object detail").child(meta_panel.clone())
        });
    }

    fn create_folder(&mut self, folder_name: String, cx: &mut Context<Self>) {
        let client = self.ossclient.clone();
        let bucket_name = self.bucket_name.clone();
        let folder_object_key = format!("{}{}/", self.prefix, folder_name);
        let handle = tokio_runtime().handle().clone();

        self.create_folder_task = cx.spawn(async move |this, cx| {
            let join = handle
                .spawn(async move { client.create_folder(bucket_name, folder_object_key).await });
            let _abort_on_drop = AbortOnDrop(join.abort_handle());

            let result = match join.await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            };

            this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.load_objects(cx),
                Err(e) => {
                    let msg = e.to_string();
                    window.push_notification((NotificationType::Error, msg), cx);
                }
            })
            .ok();
        });
    }

    /// 打开上传用的文件/文件夹选择器。
    ///
    /// Linux 的 xdg-portal 实现会**忽略 `files`**：`directories: true` 就是文件夹
    /// 选择器，两者只能二选一，所以 `folder_only` 只在 Linux 上有意义。
    /// 其他平台两个都可以为 true，用户在一个对话框里既能选文件也能选目录。
    fn select_files_for_upload(
        &mut self,
        folder_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "linux")]
        let (files, directories) = (!folder_only, folder_only);
        #[cfg(not(target_os = "linux"))]
        let (files, directories) = {
            let _ = folder_only;
            (true, true)
        };

        let picked = cx.prompt_for_paths(PathPromptOptions {
            files,
            directories,
            multiple: true,
            prompt: Some("Upload".into()),
        });

        let main_view = self.main_view.clone();
        let bucket_name = self.bucket_name.clone();
        let prefix = self.prefix.clone();

        cx.spawn_in(window, async move |_, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) if !paths.is_empty() => paths,
                Ok(Err(err)) => {
                    // Linux 上多半是 xdg-desktop-portal 没跑
                    cx.update(|window, cx| {
                        window.push_notification(
                            (
                                NotificationType::Error,
                                format!("Couldn't open the picker: {err}"),
                            ),
                            cx,
                        );
                    })
                    .ok();
                    return;
                }
                _ => return, // 用户取消
            };

            // 展开目录是本地 I/O，放后台执行器，UI 线程不碰
            let (candidates, truncated) = cx
                .background_spawn(async move { scan_upload_paths(paths) })
                .await;

            if candidates.is_empty() {
                cx.update(|window, cx| {
                    window.push_notification(
                        (NotificationType::Warning, "No files found in the selection"),
                        cx,
                    );
                })
                .ok();
                return;
            }

            let count = candidates.len();
            let kinds: Vec<JobKind> = candidates
                .into_iter()
                .map(|candidate| JobKind::Upload {
                    bucket_name: bucket_name.clone(),
                    object_key: format!("{prefix}{}", candidate.relative),
                    source: candidate.source,
                    size: candidate.size,
                    relative: candidate.relative,
                })
                .collect();

            cx.update(|window, cx| {
                main_view
                    .update(cx, move |main_view, cx| {
                        main_view.enqueue_jobs(kinds, window, cx)
                    })
                    .ok();
            })
            .ok();

            cx.update(|window, cx| {
                let msg = if truncated {
                    format!("Added the first {count} files (limit reached)")
                } else {
                    format!("Added {count} files")
                };
                window.push_notification((NotificationType::Success, msg), cx);
            })
            .ok();
        })
        .detach();
    }

    /// 删除当前勾选的对象。
    ///
    /// 勾选里可能混着文件夹（`common_prefixes` 里的虚拟目录），
    /// 它们各自变成一个 `DeletePrefix` 任务（连同内容一起删），
    /// 文件则合成一个 `Delete` 任务（批量删，每 1000 个一批）。
    fn on_delete_action(&mut self, _: &DeleteAction, window: &mut Window, cx: &mut Context<Self>) {
        let (files, folders) = {
            let delegate = self.objects_state.read(cx).delegate();
            let mut files: Vec<String> = Vec::new();
            let mut folders: Vec<String> = Vec::new();

            for &ix in &delegate.selected_indexes {
                let Some(row) = delegate.rows.get(ix) else {
                    continue;
                };
                match row {
                    OssObjectItem::File(_) => files.push(row.get_key().clone()),
                    OssObjectItem::Folder(prefix) => folders.push(prefix.clone()),
                }
            }

            (files, folders)
        };

        if files.is_empty() && folders.is_empty() {
            return;
        }

        let title = match (files.len(), folders.len()) {
            (1, 0) => "Delete 1 object?".to_string(),
            (n, 0) => format!("Delete {n} objects?"),
            (0, 1) => "Delete 1 folder?".to_string(),
            (0, n) => format!("Delete {n} folders?"),
            (f, d) => format!("Delete {f} objects and {d} folders?"),
        };
        let description = if folders.is_empty() {
            "This can't be undone."
        } else {
            "Folders are deleted with everything inside them. This can't be undone."
        };

        let bucket_name = self.bucket_name.clone();
        let main_view = self.main_view.clone();
        let panel = cx.weak_entity();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let bucket_name = bucket_name.clone();
            let main_view = main_view.clone();
            let panel = panel.clone();
            let files = files.clone();
            let folders = folders.clone();

            alert
                .confirm()
                .title(title.clone())
                .description(description)
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    // 一个对象一个任务：每个文件、每个文件夹在面板上各占一行，
                    // 各自有独立的进度和取消。
                    // （代价是每个对象一次 HTTP 请求，批量删除那套 1000 个一批的
                    //   能力还在 run_delete 里留着，以后想换回来只改这里。）
                    let mut kinds: Vec<JobKind> = Vec::new();

                    for key in &files {
                        kinds.push(JobKind::Delete {
                            bucket_name: bucket_name.clone(),
                            object_keys: vec![key.clone()],
                        });
                    }
                    for prefix in &folders {
                        kinds.push(JobKind::DeletePrefix {
                            bucket_name: bucket_name.clone(),
                            prefix: prefix.clone(),
                        });
                    }

                    main_view
                        .update(cx, move |main_view, cx| {
                            main_view.enqueue_jobs(kinds, window, cx)
                        })
                        .ok();

                    // 已经交给队列了，清掉勾选
                    panel
                        .update(cx, |panel, cx| {
                            panel.objects_state.update(cx, |state, cx| {
                                state.delegate_mut().unselect_all();
                                cx.notify();
                            });
                        })
                        .ok();

                    true // 关闭对话框
                })
        });
    }

    fn on_open_files_for_upload_action(
        &mut self,
        _: &OpenFilesForUploadAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_files_for_upload(false, window, cx);
    }

    fn on_open_folders_for_upload_action(
        &mut self,
        _: &OpenFolderForUploadAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_files_for_upload(true, window, cx);
    }
}

impl Render for ObjectListPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .on_action(cx.listener(Self::on_open_files_for_upload_action))
            .on_action(cx.listener(Self::on_open_folders_for_upload_action))
            .on_action(cx.listener(Self::on_delete_action))
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

/// Panel for new folder dialog
struct NewFolderPanel {
    object_list_panel: WeakEntity<ObjectListPanel>,
    input_state: Entity<InputState>,
}

impl NewFolderPanel {
    fn new(
        object_list_panel: WeakEntity<ObjectListPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            object_list_panel,
            input_state: cx.new(|cx| InputState::new(window, cx)),
        }
    }

    fn on_confirmed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.input_state.read(cx).value().trim().to_string();
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            window.push_notification(
                (
                    NotificationType::Error,
                    "Folder name must not be empty and must not contain / or \\",
                ),
                cx,
            );
            return;
        }

        println!("new folder name: {name}");

        self.object_list_panel
            .update(cx, |panel, cx| {
                panel.create_folder(name, cx);
            })
            .ok();

        window.close_dialog(cx);
    }
}

impl Render for NewFolderPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .gap_1()
            .child(div().text_sm().child("Folder name"))
            .child(Input::new(&self.input_state))
    }
}

enum OssObjectItem {
    Folder(String),
    File(ObjectSummary),
}

impl OssObjectItem {
    fn get_key(&self) -> &String {
        match self {
            Self::Folder(f) => f,
            Self::File(object_summary) => &object_summary.key,
        }
    }

    fn is_folder(&self) -> bool {
        matches!(self, Self::Folder(_))
    }

    fn is_file(&self) -> bool {
        matches!(self, Self::File(_))
    }

    fn get_size(&self) -> u64 {
        match self {
            Self::File(f) => f.size,
            Self::Folder(_) => 0u64,
        }
    }
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
    selected_indexes: HashSet<usize>,
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
            selected_indexes: HashSet::new(),
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
                    .width(px(300.0))
                    .movable(false)
                    .sortable(),
                Column::new("size", "Size")
                    .width(px(100.0))
                    .text_right()
                    .movable(false)
                    .sortable(),
                Column::new("storage_class", "Storage class")
                    .width(px(120.0))
                    .movable(false),
            ],
        }
    }

    fn set_rows(&mut self, rows: Vec<OssObjectItem>) {
        self.selected_indexes.clear();
        self.rows = rows;
        self.loading = false;
        self.recompute();
    }

    /// 后台刷新专用：**保留勾选**。
    ///
    /// `selected_indexes` 存的是 `rows` 的下标，换一批 rows 就全错位了，
    /// 所以先把勾选的 key 记下来，替换完再按 key 找回新下标。
    /// 被删掉的对象在新 rows 里找不到，自然从勾选里消失 —— 正是想要的。
    ///
    /// （换页用的 `set_rows` 清空勾选是对的，两者不要混。）
    fn refresh_rows(&mut self, rows: Vec<OssObjectItem>) {
        let selected_keys: HashSet<String> = self
            .selected_indexes
            .iter()
            .filter_map(|&ix| self.rows.get(ix))
            .map(|row| row.get_key().clone())
            .collect();

        self.rows = rows;

        self.selected_indexes = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| selected_keys.contains(row.get_key()))
            .map(|(ix, _)| ix)
            .collect();

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
                needle.is_empty() || o.get_key().to_ascii_lowercase().contains(&needle)
            })
            .map(|(ix, _)| ix)
            .collect();

        if self.current_sort.is_none() {
            return;
        }

        let Some((col_ix, sort)) = self.current_sort else {
            return; // 没有排序 → 自然序，完事
        };

        if sort == ColumnSort::Default {
            return;
        }

        let desc = sort == ColumnSort::Descending;
        self.filtered_indexes.sort_by(|&a, &b| {
            match col_ix {
                2 => {
                    let item_a = &self.rows[a];
                    let item_b = &self.rows[b];
                    let key_a = item_a.get_key();

                    let key_b = item_b.get_key();

                    if item_a.is_folder() && item_b.is_folder() {
                        let o = key_a.cmp(key_b);

                        if desc {
                            return o.reverse();
                        } else {
                            return o;
                        }
                    }

                    // folder always shown first
                    if item_a.is_folder() && item_b.is_file() {
                        return Ordering::Less;
                    }

                    if item_a.is_file() && item_b.is_folder() {
                        return Ordering::Greater;
                    }

                    if item_a.is_file() && item_b.is_file() {
                        let o = key_a.cmp(key_b);

                        if desc {
                            return o.reverse();
                        } else {
                            return o;
                        }
                    }

                    return Ordering::Equal;
                }
                3 => {
                    let item_a = &self.rows[a];
                    let item_b = &self.rows[b];
                    if item_a.is_folder() && item_b.is_folder() {
                        return Ordering::Equal;
                    }

                    if item_a.is_folder() && item_b.is_file() {
                        return Ordering::Less;
                    }

                    if item_a.is_file() && item_b.is_folder() {
                        return Ordering::Greater;
                    }

                    let o = item_a.get_size().cmp(&item_b.get_size());
                    if desc { o.reverse() } else { o }
                }
                _ => Ordering::Equal,
            }
        });
    }

    fn apply_filter(&mut self, needle: &str) {
        self.search = needle.to_string();
        self.recompute();
    }

    fn select_row(&mut self, original_row_index: usize) {
        self.selected_indexes.insert(original_row_index);
    }

    fn unselect_row(&mut self, original_row_index: usize) {
        self.selected_indexes.remove(&original_row_index);
    }

    fn select_all(&mut self) {
        self.selected_indexes = self.filtered_indexes.iter().copied().collect();
    }

    fn unselect_all(&mut self) {
        self.selected_indexes.clear();
    }
}

impl TableDelegate for ObjectTableDelegate {
    fn columns_count(&self, _: &gpui_kit::App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &gpui_kit::App) -> usize {
        self.filtered_indexes.len()
    }

    fn column(&self, col_ix: usize, _: &gpui_kit::App) -> Column {
        self.columns[col_ix].clone()
    }

    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.column(col_ix, cx);
        if col_ix == 0 {
            div()
                .size_full()
                .h_flex()
                .items_center()
                .justify_center()
                .child(
                    Checkbox::new("select-all-checkbox")
                        .tooltip("Toggle select")
                        .checked(
                            self.filtered_indexes.len() > 0
                                && self.selected_indexes.len() == self.filtered_indexes.len(),
                        )
                        .on_change(cx.listener(|this, val, _, _| {
                            if *val {
                                this.delegate_mut().select_all();
                            } else {
                                this.delegate_mut().unselect_all();
                            }
                        })),
                )
        } else {
            div()
                .size_full()
                .text_align(col.align)
                .child(col.name.clone())
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.row(row_ix) else {
            return div();
        };

        let original_row_index = self.filtered_indexes[row_ix];

        let key = row.get_key();

        let name = &key[self.prefix.len()..];
        let is_folder = row.is_folder();
        let file_size = row.get_size();

        match col_ix {
            0 => div()
                .size_full()
                .h_flex()
                .items_center()
                .justify_center()
                .child(
                    Checkbox::new(format!("ck-{}", key))
                        .checked(self.selected_indexes.contains(&original_row_index))
                        .on_change(cx.listener(move |this, val, _, _| {
                            if *val {
                                this.delegate_mut().select_row(original_row_index);
                            } else {
                                this.delegate_mut().unselect_row(original_row_index);
                            }
                        })),
                ),
            1 => div()
                .size_full()
                .h_flex()
                .items_center()
                .justify_center()
                .child(
                    (if row.is_folder() {
                        div()
                            .text_color(cx.theme().primary.opacity(0.85))
                            .child(IconName::Folder)
                    } else {
                        div()
                            .text_color(cx.theme().foreground.opacity(0.85))
                            .child(IconName::File)
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
                                    panel.next_continuation_token = None;
                                    panel.load_objects(cx);
                                })
                                .ok();
                        } else {
                            let key_cloned = key_cloned.clone();
                            object_list_panel
                                .update(cx, move |panel, cx| {
                                    panel.show_object_detail(key_cloned, window, cx);
                                })
                                .ok();
                        }
                    })
            }),
            3 => div()
                .size_full()
                .text_align(TextAlign::Right)
                .child(if is_folder {
                    "-".to_string()
                } else {
                    format_file_size(file_size)
                }),
            4 => div().child(if let OssObjectItem::File(f) = row {
                f.storage_class.to_string()
            } else {
                "-".to_string()
            }),
            _ => div(),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<'_, TableState<Self>>,
    ) {
        self.current_sort = Some((col_ix, sort));
        self.recompute();
    }
}

struct ObjectMetaPanel {
    ossclient: Arc<Client>,
    bucket_name: String,
    object_key: String,
    load_task: Task<()>,
    load_state: LoadState,
    object_meta: Option<ObjectMetadata>,
    presigned_url: Option<String>,
}

impl ObjectMetaPanel {
    fn new(
        ossclient: Arc<Client>,
        bucket_name: String,
        object_key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let this = Self {
            ossclient,
            bucket_name,
            object_key,
            load_task: Task::ready(()),
            load_state: LoadState::Idle,
            object_meta: None,
            presigned_url: None,
        };

        cx.on_next_frame(window, |this, _, cx| {
            this.load_object_metadata(cx);
        });

        this
    }

    fn load_object_metadata(&mut self, cx: &mut Context<Self>) {
        if self.load_state == LoadState::Loading {
            return;
        }

        self.load_state = LoadState::Loading;
        cx.notify();

        let client = self.ossclient.clone();
        let bucket_name = self.bucket_name.clone();
        let object_key = self.object_key.clone();
        let handle = tokio_runtime().handle().clone();

        self.load_task = cx.spawn(async move |this, cx| {
            let bucket_name_clone = bucket_name.clone();
            let object_key_clone = object_key.clone();
            let client_clone = client.clone();

            let join = handle
                .spawn(async move { client.head_object(&bucket_name, &object_key, None).await });
            let _abort_on_drop = AbortOnDrop(join.abort_handle());

            let result = match join.await {
                Ok(Ok(meta)) => Ok(meta),
                Ok(Err(e)) => Err(anyhow::anyhow!("{e}")),
                Err(join_err) => Err(anyhow::anyhow!("oss task failed: {join_err}")),
            };

            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(meta) => {
                        // println!("{:?}", meta);
                        this.presigned_url = Some(
                            client_clone.presign_url(
                                bucket_name_clone,
                                object_key_clone,
                                PresignGetOptionsBuilder::default()
                                    .expires_seconds(120)
                                    .build(),
                            ),
                        );
                        this.object_meta = Some(meta);
                        this.load_state = LoadState::Loaded;
                    }
                    Err(e) => {
                        this.load_state = LoadState::Failed;
                        window.push_notification((NotificationType::Error, e.to_string()), cx);
                    }
                }

                cx.notify();
            })
            .ok();
        });
    }

    fn render_detail(&self) -> Div {
        if let Some(meta) = &self.object_meta {
            div().child(
                DescriptionList::horizontal()
                    .columns(1)
                    .item("Key", self.object_key.as_str(), 1)
                    .item("Size", format_file_size(meta.content_length), 1)
                    .item("ETag", meta.etag.as_str(), 1)
                    .item(
                        "Last modified",
                        meta.last_modified
                            .as_ref()
                            .map(|s| format_datetime(s.as_str()))
                            .unwrap_or("".to_string()),
                        1,
                    )
                    .item(
                        "Last accessed",
                        meta.last_access_time
                            .as_ref()
                            .map(|s| format_datetime(s.as_str()))
                            .unwrap_or("".to_string()),
                        1,
                    )
                    .children(
                        meta.metadata.iter().map(|(k, v)| {
                            DescriptionItem::new(k.as_str()).value(v.as_str()).span(1)
                        }),
                    )
                    .children(
                        meta.raw_headers.iter().map(|(k, v)| {
                            DescriptionItem::new(k.as_str()).value(v.as_str()).span(1)
                        }),
                    ),
            )
        } else {
            div().text_center().child("Something went wrong...")
        }
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> Div {
        div()
            .w_full()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(if self.load_state == LoadState::Loaded {
                if let Some(mime_type) = self
                    .object_meta
                    .as_ref()
                    .map(|m| m.raw_headers.get("content-type"))
                    .flatten()
                    && mime_type.starts_with("image/")
                    && let Some(url) = self.presigned_url.clone()
                {
                    let hint_text_color = cx.theme().secondary_foreground;
                    let error_text_color = cx.theme().red;

                    div().w_full().h_56().flex().overflow_hidden().child(
                        img(url)
                            .id(format!("{}/{}", self.bucket_name, self.object_key))
                            .size_full()
                            .object_fit(gpui_kit::ObjectFit::Contain)
                            .with_loading(move || {
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .text_color(hint_text_color)
                                    .child("Loading...")
                                    .into_any_element()
                            })
                            .with_fallback(move || {
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .text_color(error_text_color)
                                    .child("Failed to load image")
                                    .into_any_element()
                            }),
                    )
                } else {
                    div()
                        .w_full()
                        .h_56()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().secondary_foreground)
                        .child("Preview not supported for this object")
                }
            } else {
                div()
            })
    }
}

impl Render for ObjectMetaPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().secondary_foreground)
                    .child("Preview"),
            )
            .child(self.render_preview(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().secondary_foreground)
                    .mt_4()
                    .child("Metadata"),
            )
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
