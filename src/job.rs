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
    time::{Duration, Instant},
};

use ali_oss_rs::Client;
use gpui_kit::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, Subscription, Task, Window,
    assets::IconName,
    base::{IndexPath, h_flex},
    component::{
        ActiveTheme, Icon, Sizable,
        button::{Button, ButtonVariants},
        list::{List, ListDelegate, ListItem, ListState},
        progress::ProgressCircle,
    },
    div,
    prelude::FluentBuilder,
};
use tokio::sync::{mpsc, watch};

use crate::common::file_name;

/// 上传走分片的下限：小于它用一次 PutObject 完成
const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024;

/// 分片上传的目标片数。按文件大小算出的片大小会尽量让片数落在这个量级：
/// 片数太少进度条走不动，太多则请求数和 complete 请求体都会膨胀。
const TARGET_PARTS: u64 = 50;

/// 单片大小下限。OSS 要求除最后一片外每片 ≥ 100 KB，这里取 5 MiB 留足余量。
const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// 单片大小上限。OSS 的上限是 5 GB，这里取 512 MiB：
/// 再大单片的失败代价就太高了。
const MAX_PART_SIZE: u64 = 512 * 1024 * 1024;

/// 按文件大小算分片大小。
///
/// 固定片大小是在"支持大文件"和"进度够细"之间二选一；按大小算就能两个都要：
/// 小文件切得细一点（进度条能走），大文件把片撑大（请求数不爆炸）。
///
/// | 文件大小 | 片大小 | 片数 |
/// | --- | --- | --- |
/// | 100 MiB | 5 MiB | 20 |
/// | 1 GiB | 21 MiB | 49 |
/// | 10 GiB | 205 MiB | 50 |
/// | 100 GiB | 512 MiB | 200 |
/// | 1 TiB | 512 MiB | 2048 |
///
/// 片大小向上对齐到 1 MiB（边界整齐、日志好读）。因为只会往上取整，
/// 片数只会比目标更少，不会更多。单对象上限约 10000 × 512 MiB ≈ 4.9 TiB。
fn part_size_for(total: u64) -> u64 {
    const ALIGN: u64 = 1024 * 1024;

    let ideal = total.div_ceil(TARGET_PARTS);
    let clamped = ideal.clamp(MIN_PART_SIZE, MAX_PART_SIZE);
    clamped.div_ceil(ALIGN).max(1) * ALIGN
}

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
        /// 相对用户选中位置的路径，用来在面板上显示。
        /// 选文件夹上传时是 `vacation/japan/2023/09/11111.jpg`，直接选文件时就是文件名。
        /// 已经做过 `\` → `/` 归一化（就是 object_key 去掉目标 prefix 的那一段）。
        relative: String,
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
    /// 这个任务在传数据吗？传的是哪个方向？
    /// 删除/复制/移动是元数据操作，不计入传输速率。
    fn transfer_direction(&self) -> Option<TransferDirection> {
        match self {
            JobKind::Upload { .. } => Some(TransferDirection::Up),
            JobKind::Download { .. } => Some(TransferDirection::Down),
            JobKind::Delete { .. } | JobKind::DeletePrefix { .. } => None,
        }
    }

    /// 获取在 UI 上显示的文本
    fn get_label(&self) -> &str {
        match self {
            // 选文件夹上传时显示相对路径（vacation/japan/2023/09/11111.jpg），
            // 直接选文件时 relative 就是文件名
            JobKind::Upload { relative, .. } => relative,
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
    Progress {
        id: u64,
        /// 面板上显示的进度。单位取决于任务类型：
        /// 小文件上传是字节，分片上传是**片数**。
        progress: JobProgress,
        /// 这次事件**新增**的字节数（不是累计值）。
        /// 和 progress 分开是因为分片上传的 progress 单位是片数，
        /// 换算成字节只有执行器知道。
        delta_bytes: u64,
    },
    Completed { id: u64 },
    Failed { id: u64, message: String },
    Cancelled { id: u64 },
}

/// 传输方向，用于速率统计
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferDirection {
    Up,
    Down,
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

/// 速率统计的滑动窗口长度。窗口越长数字越稳，但停下来后衰减得越慢。
const SPEED_WINDOW: Duration = Duration::from_secs(3);

/// 整体传输速率（字节/秒）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferSpeed {
    pub up: u64,
    pub down: u64,
}

impl TransferSpeed {
    pub fn is_idle(&self) -> bool {
        self.up == 0 && self.down == 0
    }
}

/// 一个方向的滑动窗口：只保留 [`SPEED_WINDOW`] 内的采样
#[derive(Default)]
struct SpeedWindow {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedWindow {
    fn add(&mut self, bytes: u64, now: Instant) {
        if bytes > 0 {
            self.samples.push_back((now, bytes));
        }
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        while let Some((at, _)) = self.samples.front() {
            if now.saturating_duration_since(*at) > SPEED_WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    fn bytes(&self) -> u64 {
        self.samples.iter().map(|(_, bytes)| bytes).sum()
    }
}

/// 上/下行速率表。
///
/// 采样是**增量**的：每次进度事件把"这次新增了多少字节"塞进来，
/// 窗口内求和再除以窗口长度就是速率。没有新采样时速率会随时间自然衰减到 0。
struct SpeedMeter {
    up: SpeedWindow,
    down: SpeedWindow,
    /// 最近一次算出来的速率，UI 直接读这个
    current: TransferSpeed,
}

impl SpeedMeter {
    fn new() -> Self {
        Self {
            up: SpeedWindow::default(),
            down: SpeedWindow::default(),
            current: TransferSpeed::default(),
        }
    }

    fn record(&mut self, direction: TransferDirection, bytes: u64) {
        let now = Instant::now();
        match direction {
            TransferDirection::Up => self.up.add(bytes, now),
            TransferDirection::Down => self.down.add(bytes, now),
        }
        self.recompute();
    }

    /// 没有新数据时也要定期调用，让过期采样被淘汰、速率衰减
    fn tick(&mut self) {
        let now = Instant::now();
        self.up.prune(now);
        self.down.prune(now);
        self.recompute();
    }

    fn recompute(&mut self) {
        let secs = SPEED_WINDOW.as_secs_f64();
        self.current = TransferSpeed {
            up: (self.up.bytes() as f64 / secs) as u64,
            down: (self.down.bytes() as f64 / secs) as u64,
        };
    }

    fn speed(&self) -> TransferSpeed {
        self.current
    }
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

    /// 上/下行速率
    speed: SpeedMeter,

    /// 常驻的事件消费协程。存进 Self —— Task 一旦被 drop 就取消，队列会静默死掉
    _pump_task: Task<()>,
    /// 速率的定时重算（没有事件时也要让速率衰减）
    _speed_task: Task<()>,
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

        // 速率是滑动窗口，没有新采样时会随时间衰减 —— 不定期重算的话，
        // 传输停下来之后状态栏会一直挂着最后一个数字
        let speed_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(1))
                    .await;
                if this.update(cx, |queue, cx| queue.tick_speed(cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            jobs: vec![],
            queue_order: VecDeque::new(),
            running: HashMap::new(),
            in_flight: 0,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            tx,
            speed: SpeedMeter::new(),
            _pump_task: pump_task,
            _speed_task: speed_task,
            next_id: 1,
        }
    }

    /// 当前整体传输速率
    pub fn speed(&self) -> TransferSpeed {
        self.speed.speed()
    }

    /// 定时重算速率。值没变就不通知，避免每秒都白重绘一次。
    fn tick_speed(&mut self, cx: &mut Context<Self>) {
        let before = self.speed.speed();
        self.speed.tick();
        if self.speed.speed() != before {
            cx.notify();
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

    fn cancel_job(&mut self, id: u64) {
        let _ = self.tx.send(JobEvent::Cancelled { id });
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

    /// 批量入队：先全部 push，最后统一派发一次。
    ///
    /// 选中一个大目录时可能有几千个文件，逐个走 `enqueue` 会跑几千遍
    /// `pump` + `notify`（虽然 notify 只是置脏位，但语义上没必要）。
    pub fn enqueue_many(
        &mut self,
        jobs: impl IntoIterator<Item = (JobKind, Arc<Client>)>,
        cx: &mut Context<Self>,
    ) {
        for (kind, client) in jobs {
            let id = self.next_id;
            self.next_id += 1;

            self.jobs.push(Job {
                id,
                kind,
                client,
                state: JobState::Queued,
            });
            self.queue_order.push_back(id);
        }

        self.pump(cx);
        cx.notify();
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
            JobEvent::Progress {
                id,
                progress,
                delta_bytes,
            } => {
                if let Some(job) = self.job_mut(id)
                    && let JobState::Running { progress: slot } = &mut job.state
                {
                    *slot = progress;
                }

                if delta_bytes > 0
                    && let Some(direction) = self.job(id).and_then(|j| j.kind.transfer_direction())
                {
                    self.speed.record(direction, delta_bytes);
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
    use std::{
        sync::atomic::{AtomicU32, AtomicU64, Ordering},
        time::Instant,
    };

    use ali_oss_rs::{
        multipart::MultipartUploadsOperations,
        multipart_common::{CompleteMultipartUploadRequest, UploadPartRequest},
        object::ObjectOperations,
        object_common::PutObjectOptionsBuilder,
    };

    use crate::common::tokio_runtime;

    use super::*;

    /// 进度上报的最小间隔：跨过 1‰ 或距上次超过这么久，才真的发一次事件
    const PROGRESS_MIN_INTERVAL_MS: u64 = 100;

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
        ctl: watch::Receiver<JobSignal>,
    ) -> Result<(), JobError> {
        let JobKind::Upload { size, .. } = &run.kind else {
            unreachable!()
        };

        // 两条路的进度单位不一样（小文件是字节，分片是片数），
        // 所以各自的"起始进度"由各自上报
        if *size >= MULTIPART_THRESHOLD {
            run_upload_multipart(run, tx, ctl).await
        } else {
            run_upload_single(run, tx, ctl).await
        }
    }

    /// 小文件：一次 PutObject 传完，进度单位是**字节**。
    async fn run_upload_single(
        run: &JobRun,
        tx: &mpsc::UnboundedSender<JobEvent>,
        mut ctl: watch::Receiver<JobSignal>,
    ) -> Result<(), JobError> {
        let JobKind::Upload {
            bucket_name,
            object_key,
            source,
            size,
            ..
        } = &run.kind
        else {
            unreachable!()
        };

        let throttle = Arc::new(ProgressThrottle::new(tx.clone(), run.id));

        // 先报一次 0/总量，让面板的进度条从"不定量"切成"确定态"
        throttle.report_immediate(0, Some(*size), 0);

        let options = PutObjectOptionsBuilder::new()
            .progress({
                let throttle = throttle.clone();
                // 小文件上传的进度单位就是字节，所以第三个参数直接给 done
                move |done, total| throttle.report(done, total, done)
            })
            .build();

        // 取消要立刻生效：把请求本身放进 select!，未完成的 future 被 drop = 断连，
        // 不用等整个文件传完
        let result = tokio::select! {
            r = run.client.put_object_from_file(bucket_name, object_key, source, Some(options)) => r,
            _ = ctl.wait_for(|s| *s == JobSignal::Cancel) => return Err(JobError::Cancelled),
        };

        // 这一行不能省：select! 的结果就是上传的结果
        result.map_err(|e| JobError::Failed(e.to_string()))?;

        // 限流可能吞掉最后一个 chunk，收尾补一次，保证进度条走满
        throttle.report_immediate(*size, Some(*size), *size);

        Ok(())
    }

    /// 大文件：分片上传，片大小由 [`part_size_for`] 按文件大小算出来。
    ///
    /// 进度单位是**片数**（已完成片 / 总片数）—— 分片内部拿不到进度
    /// （`upload_part_from_file` 没有进度回调），一片就是一个原子单位。
    async fn run_upload_multipart(
        run: &JobRun,
        tx: &mpsc::UnboundedSender<JobEvent>,
        mut ctl: watch::Receiver<JobSignal>,
    ) -> Result<(), JobError> {
        let JobKind::Upload {
            bucket_name,
            object_key,
            source,
            size,
            ..
        } = &run.kind
        else {
            unreachable!()
        };

        let total = *size;
        let part_size = part_size_for(total);
        let total_parts = total.div_ceil(part_size);

        let throttle = ProgressThrottle::new(tx.clone(), run.id);
        throttle.report_immediate(0, Some(total_parts), 0);

        let upload_id = run
            .client
            .initiate_multipart_uploads(bucket_name, object_key, None)
            .await
            .map_err(|e| JobError::Failed(e.to_string()))?
            .upload_id;

        let mut parts: Vec<(u32, String)> = Vec::new();
        let mut offset = 0u64;
        let mut part_number = 1u32;
        let mut completed_parts = 0u64;

        let result: Result<(), JobError> = loop {
            if offset >= total {
                break Ok(());
            }

            // 片与片之间是取消的生效点（片内则由下面 select! 的另一个分支负责）
            match checkpoint(&ctl) {
                Checkpoint::Continue => {}
                Checkpoint::Cancel => break Err(JobError::Cancelled),
            }

            let end = (offset + part_size).min(total);

            let part = tokio::select! {
                r = run.client.upload_part_from_file(
                        bucket_name,
                        object_key,
                        source,
                        offset..end,
                        UploadPartRequest::new(part_number, upload_id.as_str()),
                    ) => match r {
                        Ok(part) => part,
                        Err(e) => break Err(JobError::Failed(e.to_string())),
                    },
                _ = ctl.wait_for(|s| *s == JobSignal::Cancel) => {
                    break Err(JobError::Cancelled)
                }
            };

            parts.push((part_number, part.etag));
            offset = end;
            part_number += 1;
            completed_parts += 1;

            throttle.report(completed_parts, Some(total_parts), offset);
        };

        // 传片成功就收尾（complete），否则直接把错误带出来
        let completed = match result {
            Ok(()) => run
                .client
                .complete_multipart_uploads(
                    bucket_name,
                    object_key,
                    CompleteMultipartUploadRequest {
                        upload_id: upload_id.clone(),
                        parts,
                    },
                    None,
                )
                .await
                .map_err(|e| JobError::Failed(e.to_string())),
            Err(e) => Err(e),
        };

        match completed {
            Ok(_) => {
                throttle.report_immediate(total_parts, Some(total_parts), total);
                Ok(())
            }
            Err(e) => {
                // 取消、传片失败、complete 失败，都要清掉服务端已上传的分片，
                // 否则碎片会一直留在 bucket 里计费
                let _ = run
                    .client
                    .abort_multipart_uploads(bucket_name, object_key, &upload_id)
                    .await;
                Err(e)
            }
        }
    }

    /// 进度上报节流。
    ///
    /// ali-oss-rs 的进度回调**每个 chunk 都会调一次**（8~64KB 一个 chunk，
    /// 1GB 文件上万次）。全部转发给队列会让 UI 做大量无谓的重绘，所以在
    /// 生产侧按"跨过 1‰ 或距上次超过 100ms"合并。
    ///
    /// 用原子量是因为回调签名是 `Fn`（不是 `FnMut`）且要求 `Send + Sync`。
    struct ProgressThrottle {
        tx: mpsc::UnboundedSender<JobEvent>,
        id: u64,
        start: Instant,
        last_permille: AtomicU32,
        last_sent_ms: AtomicU64,
        /// 上次**实际发出**时的累计字节数。只在发送时推进，
        /// 这样被限流吞掉的那些增量会在下一次发送时一起带上。
        last_bytes: AtomicU64,
    }

    impl ProgressThrottle {
        fn new(tx: mpsc::UnboundedSender<JobEvent>, id: u64) -> Self {
            Self {
                tx,
                id,
                start: Instant::now(),
                last_permille: AtomicU32::new(0),
                last_sent_ms: AtomicU64::new(0),
                last_bytes: AtomicU64::new(0),
            }
        }

        /// `bytes` 是这个任务**累计**已传输的字节数（不是本次增量）
        fn report(&self, done: u64, total: Option<u64>, bytes: u64) {
            // 总量未知时恒为 0，只能靠时间那条路触发
            let permille = total
                .filter(|t| *t > 0)
                .map(|t| (done.saturating_mul(1000) / t).min(1000) as u32)
                .unwrap_or(0);
            let now_ms = self.start.elapsed().as_millis() as u64;

            let crossed = permille > self.last_permille.load(Ordering::Relaxed);
            let stale = now_ms.saturating_sub(self.last_sent_ms.load(Ordering::Relaxed))
                >= PROGRESS_MIN_INTERVAL_MS;

            if !crossed && !stale {
                return;
            }

            self.last_permille.store(permille, Ordering::Relaxed);
            self.last_sent_ms.store(now_ms, Ordering::Relaxed);
            self.send(done, total, bytes);
        }

        /// 不限流，立即发一次（起始和收尾用，保证进度条两端不会缺格）
        fn report_immediate(&self, done: u64, total: Option<u64>, bytes: u64) {
            self.send(done, total, bytes);
        }

        fn send(&self, done: u64, total: Option<u64>, bytes: u64) {
            // swap 同时读旧值写新值：delta 就是"距上次实际发送新增了多少"
            let previous = self.last_bytes.swap(bytes, Ordering::Relaxed);

            let _ = self.tx.send(JobEvent::Progress {
                id: self.id,
                progress: JobProgress { done, total },
                delta_bytes: bytes.saturating_sub(previous),
            });
        }
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
                // 删除是元数据操作，不产生传输字节
                delta_bytes: 0,
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

struct JobListDelegate {
    queue: Entity<JobQueue>,
    selected_index: Option<IndexPath>,
}

impl ListDelegate for JobListDelegate {
    type Item = ListItem;

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
            JobState::Running { progress } => ProgressCircle::new(format!("loading-{}", job.id))
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
                .text_color(cx.theme().secondary_foreground.opacity(0.7))
                .into_any_element(),
        };

        let row_ui_id = format!("job-row-{}", job.id);
        let job_id = job.id;
        Some(
            ListItem::new(row_ui_id.clone())
                .group(row_ui_id.clone())
                .rounded_md()
                .p_2()
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(div().size_4().flex_shrink_0().child(icon))
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
                                .when(job.can_cancel(), |d| {
                                    d.child(
                                        Button::new(format!("cancel-button-{}", job.id))
                                            .small()
                                            .icon(IconName::X)
                                            .ghost()
                                            .danger()
                                            .rounded_full()
                                            .tooltip("Cancel")
                                            .on_click(cx.listener(move |state, _, _, cx| {
                                                state.delegate_mut().queue.update(cx, |q, _| {
                                                    q.cancel_job(job_id);
                                                });
                                            })),
                                    )
                                }),
                        ),
                ),
        )
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
        let list_state = cx.new(|cx| ListState::new(delegate, window, cx).selectable(false));

        Self {
            queue,
            list_state,
            _subs: vec![sub],
        }
    }

    // 让 List 重新测量布局。
    // VirtualList 首次布局时还不知道可用宽度（last_content_size 为空），
    // 会用一个不受约束的宽度量样例行，导致 content_size.width 偏大。
    // 补一次布局就能拿到正确宽度。
    // pub fn refresh_list(&mut self, cx: &mut Context<Self>) {
    //     self.list_state.update(cx, |_, cx| cx.notify());
    // }
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
                relative: "photos/2026/IMG_0001.jpg".into(),
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
                relative: "photos/2026/IMG_0001.jpg".into(),
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
                relative: "photos/2026/IMG_0001.jpg".into(),
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
                relative: "photos/2026/IMG_0001.jpg".into(),
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
                relative: "photos/2026/IMG_0001.jpg".into(),
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
                progress: JobProgress {
                    done: 1234,
                    total: Some(2234),
                },
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
