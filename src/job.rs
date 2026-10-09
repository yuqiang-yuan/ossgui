//! 后台任务队列。
//!
//! 分四段：
//! 1. 模型   —— 纯数据与谓词，不碰 GPUI
//! 2. 队列   —— GPUI 侧，持有全部状态，唯一的状态迁移入口
//! 3. 执行器 —— tokio 侧，只持有 Arc<Client> + 事件 sender + 控制信号 receiver
//! 4. 面板   —— 视图
//!

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use ali_oss_rs::Client;
use gpui_kit::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    RenderOnce, Styled, Subscription, Task, Window,
    assets::IconName,
    base::{IndexPath, Selectable, StyledExt, h_flex},
    component::{
        ActiveTheme, Icon, Sizable,
        button::{Button, ButtonVariants},
        list::{List, ListDelegate, ListState},
        progress::ProgressCircle,
    },
    div,
};
use tokio::sync::{mpsc, watch};

use crate::common::file_name;

/// 上传走分片的下限：小于它用一次 PutObject 完成，中途无法暂停
const MULTIPART_THRESHOLD: u64 = 50 * 1024 * 1024;

/// 分片大小。50 MiB 的文件 = 4 片；10000 片上限 → 单对象最大 160 GiB
const PART_SIZE: u64 = 16 * 1024 * 1024;

/// 一页列出的对象数。正好等于 delete_multiple_objects 的上限，
/// 所以"一页 = 一批 = 一个 checkpoint"
const PAGE_SIZE: u32 = 1000;

/// 同时运行的任务数上限。跟 tokio worker 数无关，这些任务全是 I/O await
const DEFAULT_MAX_CONCURRENCY: usize = 2;

/// 面板上保留的终态任务条数上限，超出从最旧的开始丢
const MAX_FINISHED_JOBS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JobProgress {
    done: u64,
    total: Option<u64>,
}

impl JobProgress {
    pub fn unknown() -> Self {
        Self {
            done: 0,
            total: None,
        }
    }

    pub fn new(done: u64, total: Option<u64>) -> Self {
        Self { done, total }
    }

    pub fn percent(&self) -> f32 {
        match self.total {
            Some(t) if t > 0 => (self.done as f64 / t as f64 * 100.0).min(100.0) as f32,
            _ => 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum JobKind {
    Upload {
        bucket_name: String,
        object_key: String,
        source: PathBuf,
        size: u64,
    },

    Download {
        bucket_name: String,
        object_key: String,
        target: PathBuf,
    },

    /// Delete objects
    Delete {
        bucket_name: String,
        object_keys: Vec<String>,
    },

    /// Delete folder
    DeletePrefix { bucket_name: String, prefix: String },
}

impl JobKind {
    /// 获取在 UI 上显示的文本
    fn get_label(&self) -> &str {
        match self {
            JobKind::Upload { object_key, .. } => file_name(object_key.as_str()),
            JobKind::Download { object_key, .. } => file_name(object_key.as_str()),
            JobKind::Delete { object_keys, .. } => {
                if let Some(s) = object_keys.first() {
                    file_name(s)
                } else {
                    ""
                }
            }
            JobKind::DeletePrefix { prefix, .. } => file_name(prefix.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum JobState {
    Queued,
    Running { progress: JobProgress },
    Completed,
    Failed { message: String },
    Cancelled,
}

impl JobState {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Cancelled
        )
    }
}

struct Job {
    id: u64,
    kind: JobKind,
    client: Arc<Client>,
    state: JobState,
}

impl Job {
    fn can_cancel(&self) -> bool {
        matches!(self.state, JobState::Queued | JobState::Running { .. })
    }
}

#[derive(Debug, Clone)]
enum JobEvent {
    Progress { id: u64, progress: JobProgress },
    Completed { id: u64 },
    Failed { id: u64, message: String },
    Cancelled { id: u64 },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum JobSignal {
    Run,
    Cancel,
}

struct JobCtl {
    signal: watch::Sender<JobSignal>,
    abort: tokio::task::AbortHandle,
}

#[derive(Debug, Clone)]
enum JobError {
    Cancelled,
    Failed(String),
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct JobsSummary {
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub failed: usize,
}

/// 一次派发需要的最小信息。执行器只拿到这个，拿不到整个 Job。
struct JobRun {
    id: u64,
    kind: JobKind,
    client: Arc<Client>,
}

pub struct JobQueue {
    ///  展示顺序：只 push，永不重排（面板按这个顺序渲染）
    jobs: Vec<Job>,

    /// 派发顺序。不变量：这里的 id 集合 == jobs 里状态为 Queued 的集合
    queue_order: VecDeque<u64>,
    running: HashMap<u64, JobCtl>,
    in_flight: usize,
    max_concurrency: usize,
    tx: mpsc::UnboundedSender<JobEvent>,

    /// 常驻的事件消费协程。存进 Self —— Task 一旦被 drop 就取消，队列会静默死掉
    _pump_task: Task<()>,
    next_id: u64,
}

impl JobQueue {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<JobEvent>();
        let pump_task = cx.spawn(async move |this, cx| {
            // tokio 的 mpsc 不需要 tokio reactor，在 GPUI 前台执行器上 await 正常。
            // 同一个 sender 的消息按发送顺序投递，所以终态一定是这个 job 的最后一条。
            while let Some(ev) = rx.recv().await {
                if this.update(cx, |queue, cx| queue.on_event(ev, cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            jobs: gen_test_data(),
            queue_order: VecDeque::new(),
            running: HashMap::new(),
            in_flight: 0,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            tx,
            _pump_task: pump_task,
            next_id: 1,
        }
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    fn job(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub fn summary(&self) -> JobsSummary {
        let mut s = JobsSummary {
            total: self.jobs.len(),
            ..Default::default()
        };

        for job in &self.jobs {
            match &job.state {
                JobState::Queued => s.queued += 1,
                JobState::Running { .. } => s.running += 1,
                JobState::Failed { .. } => s.failed += 1,
                JobState::Completed => {}
                JobState::Cancelled => {}
            }
        }

        s
    }

    fn set_state(&mut self, id: u64, state: JobState) {
        if let Some(job) = self.job_mut(id) {
            job.state = state;
        }
    }

    pub fn enqueue(&mut self, kind: JobKind, client: Arc<Client>, cx: &mut Context<Self>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        self.jobs.push(Job {
            id,
            kind,
            client,
            state: JobState::Queued,
        });
        self.queue_order.push_back(id);
        self.pump(cx);
        cx.notify();
        id
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        let dispatch = select_dispatch(&self.queue_order, self.in_flight, self.max_concurrency);
        if dispatch.is_empty() {
            return;
        }

        for _ in 0..dispatch.len() {
            self.queue_order.pop_front();
        }

        for id in dispatch {
            let Some(job) = self.job(id) else {
                continue;
            };
            debug_assert!(
                job.state == JobState::Queued,
                "queue_order 与 job 状态不同步: {id:?}"
            );

            let run = JobRun {
                id,
                kind: job.kind.clone(),
                client: job.client.clone(),
            };

            let (signal, receiver) = watch::channel(JobSignal::Run);
            let handle = runner::spawn(run, self.tx.clone(), receiver);
            self.running.insert(
                id,
                JobCtl {
                    signal,
                    abort: handle.abort_handle(),
                },
            );
            self.set_state(
                id,
                JobState::Running {
                    progress: JobProgress::unknown(),
                },
            );
            self.in_flight += 1;
        }

        cx.notify();
    }

    /// 释放一个运行槽位。
    /// 所有终态都必须走这里：漏了 running 泄漏 watch sender，
    /// 漏了 in_flight 队列会永久少一个槽位。
    fn release_slot(&mut self, id: u64) {
        if self.running.remove(&id).is_some() {
            self.in_flight = self.in_flight.saturating_sub(1);
        }
    }

    fn on_event(&mut self, ev: JobEvent, cx: &mut Context<Self>) {
        match ev {
            JobEvent::Progress { id, progress } => {
                if let Some(job) = self.job_mut(id)
                    && let JobState::Running { progress: slot } = &mut job.state
                {
                    *slot = progress;
                }
            }
            JobEvent::Completed { id } => {
                self.set_state(id, JobState::Completed);
                self.release_slot(id);
                self.pump(cx);
            }
            JobEvent::Failed { id, message } => {
                self.set_state(id, JobState::Failed { message });
                self.release_slot(id);
                self.pump(cx);
            }
            JobEvent::Cancelled { id } => {
                self.set_state(id, JobState::Cancelled);
                self.release_slot(id);
                self.pump(cx);
            }
        }

        self.trim_finished();
        cx.notify();
    }

    /// 终态任务只保留最新的 MAX_FINISHED_JOBS 条，从最旧的开始丢
    fn trim_finished(&mut self) {
        let finished = self.jobs.iter().filter(|j| j.state.is_terminal()).count();
        if finished <= MAX_FINISHED_JOBS {
            return;
        }
        let mut to_remove = finished - MAX_FINISHED_JOBS;
        self.jobs.retain(|job| {
            if to_remove > 0 && job.state.is_terminal() {
                to_remove -= 1;
                false
            } else {
                true
            }
        });
    }

    fn cancel(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(job) = self.job(id) else {
            return;
        };
        if !job.can_cancel() {
            return;
        }

        match job.state {
            JobState::Queued { .. } => {
                self.queue_order.retain(|q| *q != id);
                self.set_state(id, JobState::Cancelled);
            }
            JobState::Running { .. } => {
                if let Some(ctl) = self.running.get(&id) {
                    let _ = ctl.signal.send_replace(JobSignal::Cancel);
                }
            }
            _ => {}
        }

        cx.notify();
    }
}

/// 本轮要派发的 id：从队首取，直到槽位填满。纯函数，方便单测。
fn select_dispatch(order: &VecDeque<u64>, in_flight: usize, max: usize) -> Vec<u64> {
    order
        .iter()
        .take(max.saturating_sub(in_flight))
        .copied()
        .collect()
}

mod runner {
    use crate::common::tokio_runtime;

    use super::*;

    pub(super) fn spawn(
        run: JobRun,
        tx: mpsc::UnboundedSender<JobEvent>,
        ctl: watch::Receiver<JobSignal>,
    ) -> tokio::task::JoinHandle<()> {
        tokio_runtime().spawn(async move {
            let id = run.id;
            let outcome = match &run.kind {
                JobKind::Upload { .. } => run_upload(&run, &tx, ctl).await,
                JobKind::Download { .. } => {
                    async {
                        println!("download job");
                        Ok(())
                    }
                    .await
                }
                JobKind::Delete { .. } => run_delete(&run, &tx, ctl).await,
                JobKind::DeletePrefix { .. } => {
                    async {
                        println!("delete prefix job");
                        Ok(())
                    }
                    .await
                }
            };

            let _ = tx.send(match outcome {
                Ok(()) => JobEvent::Completed { id },
                Err(JobError::Cancelled) => JobEvent::Cancelled { id },
                Err(JobError::Failed(msg)) => JobEvent::Failed { id, message: msg },
            });
        })
    }

    enum Checkpoint {
        Continue,
        Cancel,
    }

    fn checkpoint(ctl: &watch::Receiver<JobSignal>) -> Checkpoint {
        match *ctl.borrow() {
            JobSignal::Run => Checkpoint::Continue,
            JobSignal::Cancel => Checkpoint::Cancel,
        }
    }

    async fn run_upload(
        run: &JobRun,
        tx: &mpsc::UnboundedSender<JobEvent>,
        mut ctl: watch::Receiver<JobSignal>,
    ) -> Result<(), JobError> {
        let JobKind::Upload {
            bucket_name,
            object_key,
            source,
            size,
        } = &run.kind
        else {
            unreachable!()
        };

        match checkpoint(&ctl) {
            Checkpoint::Continue => {}
            Checkpoint::Cancel => return Err(JobError::Cancelled),
        }

        tokio::select! {
            _ = fake_upload() => {},
            _ = ctl.wait_for(|s| *s == JobSignal::Cancel) => {
                return Err(JobError::Cancelled)
            },
        }

        println!("file {object_key} uploaded successfully");

        Ok(())
    }

    async fn run_delete(
        run: &JobRun,
        tx: &mpsc::UnboundedSender<JobEvent>,
        mut ctl: watch::Receiver<JobSignal>,
    ) -> Result<(), JobError> {
        let JobKind::Delete {
            bucket_name,
            object_keys,
        } = &run.kind
        else {
            unreachable!()
        };

        let total = object_keys.len() as u64;
        let mut done = 0u64;

        for chunk in object_keys.chunks(PAGE_SIZE as usize) {
            match checkpoint(&ctl) {
                Checkpoint::Continue => {}
                Checkpoint::Cancel => return Err(JobError::Cancelled),
            }

            tokio::select! {
                _ = ctl.wait_for(|s| *s == JobSignal::Cancel) => {
                    return Err(JobError::Cancelled)
                },
            }

            done = done + chunk.len() as u64;
            let _ = tx.send(JobEvent::Progress {
                id: run.id,
                progress: JobProgress {
                    done,
                    total: Some(total),
                },
            });
        }
        Ok(())
    }
}

/// 假的上传一片：随机睡 300~1500ms，模拟网络耗时
async fn fake_upload_part(_bytes: u64) {
    tokio::time::sleep(Duration::from_millis(random_ms(300, 1500))).await;
}

async fn fake_upload() {
    tokio::time::sleep(Duration::from_millis(random_ms(300, 1500))).await;
}

/// 无依赖的伪随机：拿系统时间的纳秒位混淆一下。
/// 只给假任务用，别拿它做正经事。
fn random_ms(min: u64, max: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = nanos
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    min + (mixed >> 33) % (max - min).max(1)
}

#[derive(IntoElement)]
struct JobRow {
    id: u64,
    queue: Entity<JobQueue>,
    selected: bool,
}

impl JobRow {
    fn new(id: u64, queue: Entity<JobQueue>) -> Self {
        Self {
            id,
            queue,
            selected: false,
        }
    }
}

impl Selectable for JobRow {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for JobRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let queue = self.queue.read(cx);
        let Some(job) = queue.job(self.id) else {
            return div().into_any_element();
        };

        let label = job.kind.get_label().to_string();

        let icon = match &job.state {
            JobState::Queued => match &job.kind {
                JobKind::Upload { .. } => Icon::new(IconName::CloudUpload)
                    .text_color(cx.theme().primary)
                    .into_any_element(),
                JobKind::Download { .. } => Icon::new(IconName::CloudDownload)
                    .text_color(cx.theme().colors.magenta)
                    .into_any_element(),
                JobKind::Delete { .. } => Icon::new(IconName::TicketX)
                    .text_color(cx.theme().colors.red)
                    .into_any_element(),
                JobKind::DeletePrefix { .. } => Icon::new(IconName::TicketX)
                    .text_color(cx.theme().colors.red)
                    .into_any_element(),
            },
            JobState::Running { progress } => ProgressCircle::new(format!("loading-{}", self.id))
                .loading(progress.total.is_none())
                .value(progress.percent())
                .size_4()
                .into_any_element(),
            JobState::Completed => Icon::new(IconName::Check)
                .text_color(cx.theme().colors.success)
                .into_any_element(),
            JobState::Failed { .. } => Icon::new(IconName::TriangleAlert)
                .text_color(cx.theme().colors.danger)
                .into_any_element(),
            JobState::Cancelled => Icon::new(IconName::CircleSlash2)
                .text_color(cx.theme().secondary_foreground)
                .into_any_element(),
        };

        let row_ui_id = format!("job-row-{}", self.id);

        div()
            .id(row_ui_id.clone())
            .group(row_ui_id.clone())
            .min_w_0()
            .w_full()
            .p_2()
            .rounded_md()
            .hover(|s| s.bg(cx.theme().list_hover))
            .h_flex()
            .items_center()
            .gap_1()
            .child(div().flex_shrink_0().child(icon))
            .child(
                div()
                    .min_w_0()
                    .flex_grow_1()
                    .text_sm()
                    .truncate()
                    .child(label),
            )
            .child(
                div()
                    .size_6()
                    .flex_shrink_0()
                    .invisible()
                    .group_hover(row_ui_id.clone(), |s| s.visible())
                    .child(
                        Button::new(format!("cancel-button-{}", self.id))
                            .small()
                            .icon(IconName::X)
                            .ghost()
                            .danger()
                            .rounded_full()
                            .tooltip("Cancel"),
                    ),
            )
            .into_any_element()
    }
}

struct JobListDelegate {
    queue: Entity<JobQueue>,
    selected_index: Option<IndexPath>,
}

impl ListDelegate for JobListDelegate {
    type Item = JobRow;

    fn items_count(&self, _: usize, cx: &App) -> usize {
        self.queue.read(cx).jobs().len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let q = self.queue.read(cx);
        let job = q.jobs().get(ix.row)?;
        Some(JobRow::new(job.id, self.queue.clone()))
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }
}

pub struct JobPanel {
    queue: Entity<JobQueue>,
    list_state: Entity<ListState<JobListDelegate>>,
    _subs: Vec<Subscription>,
}

impl JobPanel {
    pub fn new(queue: Entity<JobQueue>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&queue, |this, _, cx| {
            this.list_state.update(cx, |_, cx| cx.notify());
            cx.notify();
        });
        let delegate = JobListDelegate {
            queue: queue.clone(),
            selected_index: None,
        };
        let list_state = cx.new(|cx| ListState::new(delegate, window, cx));

        Self {
            queue,
            list_state,
            _subs: vec![sub],
        }
    }

    /// 让 List 重新测量布局。
    /// VirtualList 首次布局时还不知道可用宽度（last_content_size 为空），
    /// 会用一个不受约束的宽度量样例行，导致 content_size.width 偏大。
    /// 补一次布局就能拿到正确宽度。
    pub fn refresh_list(&mut self, cx: &mut Context<Self>) {
        self.list_state.update(cx, |_, cx| cx.notify());
    }
}

impl Render for JobPanel {
    fn render(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::prelude::IntoElement {
        div()
            .size_full()
            .child(List::new(&self.list_state).size_full())
    }
}

fn gen_test_data() -> Vec<Job> {
    vec![
        // ---------------- Upload × 5 state ----------------
        Job {
            id: 1,
            kind: JobKind::Upload {
                bucket_name: "demo-hangzhou".into(),
                object_key: "photos/2026/IMG_0001.jpg".into(),
                source: "/tmp/IMG_0001.jpg".into(),
                size: 44_040_192,
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Queued,
        },
        Job {
            id: 2,
            kind: JobKind::Upload {
                bucket_name: "demo-hangzhou".into(),
                object_key: "photos/2026/IMG_0001.jpg".into(),
                source: "/tmp/IMG_0001.jpg".into(),
                size: 44_040_192,
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Running {
                progress: JobProgress::unknown(),
            },
        },
        Job {
            id: 3,
            kind: JobKind::Upload {
                bucket_name: "demo-hangzhou".into(),
                object_key: "photos/2026/IMG_0001.jpg".into(),
                source: "/tmp/IMG_0001.jpg".into(),
                size: 44_040_192,
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Completed,
        },
        Job {
            id: 4,
            kind: JobKind::Upload {
                bucket_name: "demo-hangzhou".into(),
                object_key: "photos/2026/IMG_0001.jpg".into(),
                source: "/tmp/IMG_0001.jpg".into(),
                size: 44_040_192,
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Failed {
                message: "AccessDenied: Access denied by bucket policy.".into(),
            },
        },
        Job {
            id: 5,
            kind: JobKind::Upload {
                bucket_name: "demo-hangzhou".into(),
                object_key: "photos/2026/IMG_0001.jpg".into(),
                source: "/tmp/IMG_0001.jpg".into(),
                size: 44_040_192,
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Cancelled,
        },
        // ---------------- Download × 5 state ----------------
        Job {
            id: 6,
            kind: JobKind::Download {
                bucket_name: "demo-beijing".into(),
                object_key: "docs/2026/q3-report-final-v7.pdf".into(),
                target: "/tmp/q3-report-final-v7.pdf".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Queued,
        },
        Job {
            id: 7,
            kind: JobKind::Download {
                bucket_name: "demo-beijing".into(),
                object_key: "docs/2026/q3-report-final-v7.pdf".into(),
                target: "/tmp/q3-report-final-v7.pdf".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Running {
                progress: JobProgress::unknown(),
            },
        },
        Job {
            id: 8,
            kind: JobKind::Download {
                bucket_name: "demo-beijing".into(),
                object_key: "docs/2026/q3-report-final-v7.pdf".into(),
                target: "/tmp/q3-report-final-v7.pdf".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Completed,
        },
        Job {
            id: 9,
            kind: JobKind::Download {
                bucket_name: "demo-beijing".into(),
                object_key: "docs/2026/q3-report-final-v7.pdf".into(),
                target: "/tmp/q3-report-final-v7.pdf".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Failed {
                message: "NoSuchKey: The specified key does not exist.".into(),
            },
        },
        Job {
            id: 10,
            kind: JobKind::Download {
                bucket_name: "demo-beijing".into(),
                object_key: "docs/2026/q3-report-final-v7.pdf".into(),
                target: "/tmp/q3-report-final-v7.pdf".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Cancelled,
        },
        // ---------------- Delete × 5 state ----------------
        Job {
            id: 11,
            kind: JobKind::Delete {
                bucket_name: "photos-prod".into(),
                object_keys: vec!["cache/warehouse/session_events_2026_10_09.parquet".into()],
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Queued,
        },
        Job {
            id: 12,
            kind: JobKind::Delete {
                bucket_name: "photos-prod".into(),
                object_keys: vec!["cache/warehouse/session_events_2026_10_09.parquet".into()],
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Running {
                progress: JobProgress { done: 1234, total: Some(2234) },
            },
        },
        Job {
            id: 13,
            kind: JobKind::Delete {
                bucket_name: "photos-prod".into(),
                object_keys: vec!["cache/warehouse/session_events_2026_10_09.parquet".into()],
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Completed,
        },
        Job {
            id: 14,
            kind: JobKind::Delete {
                bucket_name: "photos-prod".into(),
                object_keys: vec!["cache/warehouse/session_events_2026_10_09.parquet".into()],
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Failed {
                message: "AccessDenied: Access denied by bucket policy.".into(),
            },
        },
        Job {
            id: 15,
            kind: JobKind::Delete {
                bucket_name: "photos-prod".into(),
                object_keys: vec!["cache/warehouse/session_events_2026_10_09.parquet".into()],
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Cancelled,
        },
        // ---------------- DeletePrefix × 5 state ----------------
        Job {
            id: 16,
            kind: JobKind::DeletePrefix {
                bucket_name: "backup-cold".into(),
                prefix: "archive/2025/backup/".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Queued,
        },
        Job {
            id: 17,
            kind: JobKind::DeletePrefix {
                bucket_name: "backup-cold".into(),
                prefix: "archive/2025/backup/".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Running {
                progress: JobProgress::unknown(),
            },
        },
        Job {
            id: 18,
            kind: JobKind::DeletePrefix {
                bucket_name: "backup-cold".into(),
                prefix: "archive/2025/backup/".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Completed,
        },
        Job {
            id: 19,
            kind: JobKind::DeletePrefix {
                bucket_name: "backup-cold".into(),
                prefix: "archive/2025/backup/".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Failed {
                message: "NoSuchBucket: The specified bucket does not exist.".into(),
            },
        },
        Job {
            id: 20,
            kind: JobKind::DeletePrefix {
                bucket_name: "backup-cold".into(),
                prefix: "archive/2025/backup/".into(),
            },
            client: Arc::new(Client::from_env()),
            state: JobState::Cancelled,
        },
    ]
}
