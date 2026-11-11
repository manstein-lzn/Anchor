//! Serialize progress writes before handing a known card to final delivery.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anchor_channel_transport::{
    ChannelEvent,
    ledger::Ledger,
    progress::{self, ProgressLine},
};
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

use crate::{GatewayConfig, api::Api, rich::markdown_card};

const UPDATE_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_LIMIT: Duration = Duration::from_secs(2);
const ACTIVITY_LIMIT: Duration = Duration::from_secs(300);
const MAX_UPDATES: usize = 60;

#[derive(Clone, Copy)]
pub(crate) enum End {
    Sent,
    Suppressed,
    Superseded,
    Failed,
    Stopped,
    Expired,
}

impl End {
    fn content(self) -> &'static str {
        match self {
            Self::Sent => "本轮活动已结束，最终回复已另行发送。",
            Self::Suppressed => "本轮活动已结束，本条消息没有外发回复。",
            Self::Superseded => "本条消息的活动已结束，正在处理后续消息。",
            Self::Failed => "本轮活动已停止，回复尚未确认送达。",
            Self::Stopped => "连接正在关闭，本轮活动已停止。",
            Self::Expired => "本轮进度观察已结束，回复尚未确认送达。",
        }
    }
}

struct Activity {
    end: watch::Sender<Option<End>>,
    card: Arc<ReplyCard>,
    task: JoinHandle<()>,
    expires: Instant,
}

struct CardState {
    id: Option<String>,
    paused: bool,
    reusable: bool,
}

pub(crate) struct ReplyCard {
    state: Mutex<CardState>,
    cancel: watch::Sender<bool>,
}

impl ReplyCard {
    pub async fn take_for_reply(&self) -> Option<String> {
        self.cancel.send_replace(true);
        let mut state = self.state.lock().await;
        state.paused = true;
        // Join the last bounded status write. Taking the ID prevents all later
        // status/terminal writes, including shutdown, from using the final card.
        if state.reusable {
            state.id.take()
        } else {
            None
        }
    }

    pub async fn return_for_status(&self, id: String) {
        let mut state = self.state.lock().await;
        state.id = Some(id);
        state.reusable = false;
    }
}

/// Finished entries are short-lived tombstones: worker retries must not create
/// another card. Nothing is written to SQLite; restart can lose a status card.
pub(crate) struct Activities {
    config: Arc<GatewayConfig>,
    ledger: Arc<Ledger>,
    api: Option<Arc<Api>>,
    entries: BTreeMap<String, Activity>,
}

impl Activities {
    pub fn new(config: Arc<GatewayConfig>, ledger: Arc<Ledger>) -> Self {
        // A separate token cache prevents a slow status request from holding
        // the final reply's token mutex. Status setup can never fail the worker.
        let api = config
            .progress_url
            .as_ref()
            .and_then(|_| Api::new(&config).ok())
            .map(Arc::new);
        Self {
            config,
            ledger,
            api,
            entries: BTreeMap::new(),
        }
    }

    pub fn start(&mut self, event: &ChannelEvent, api_key: &str) {
        self.entries.retain(|_, activity| {
            !activity.task.is_finished() || activity.expires > Instant::now()
        });
        let (Some(api), Some(url)) = (&self.api, &self.config.progress_url) else {
            return;
        };
        if self.entries.contains_key(&event.event_id) {
            return;
        }
        let (cancel, cancelled) = watch::channel(false);
        let card = Arc::new(ReplyCard {
            state: Mutex::new(CardState {
                id: None,
                paused: false,
                reusable: true,
            }),
            cancel,
        });
        let (end, ended) = watch::channel(None);
        let (sender, receiver) = mpsc::unbounded_channel();
        let subscription = progress::subscribe(
            url.clone(),
            Some(api_key.to_owned()),
            event.event_id.clone(),
            event.conversation_scope(),
            sender,
            cancelled,
        );
        let task = tokio::spawn(activity(
            api.clone(),
            self.ledger.clone(),
            event.clone(),
            receiver,
            ended,
            card.clone(),
            subscription,
        ));
        self.entries.insert(
            event.event_id.clone(),
            Activity {
                end,
                card,
                task,
                expires: Instant::now() + self.config.recovery_window,
            },
        );
    }

    pub fn reply_card(&self, event_id: &str) -> Option<Arc<ReplyCard>> {
        self.entries
            .get(event_id)
            .map(|activity| activity.card.clone())
    }

    pub fn finish(&self, event_id: &str, end: End) {
        if let Some(activity) = self.entries.get(event_id) {
            activity.card.cancel.send_replace(true);
            activity.end.send_replace(Some(end));
        }
    }

    pub async fn shutdown(self) {
        for activity in self.entries.values() {
            activity.card.cancel.send_replace(true);
            if activity.end.borrow().is_none() {
                activity.end.send_replace(Some(End::Stopped));
            }
        }
        for activity in self.entries.into_values() {
            let _ = activity.task.await;
        }
    }
}

async fn activity(
    api: Arc<Api>,
    ledger: Arc<Ledger>,
    event: ChannelEvent,
    mut lines: mpsc::UnboundedReceiver<ProgressLine>,
    mut end: watch::Receiver<Option<End>>,
    card: Arc<ReplyCard>,
    subscription: JoinHandle<()>,
) {
    let mut shown = String::new();
    let mut pending = "正在准备".to_owned();
    let mut next_update = Instant::now() + Duration::from_millis(250);
    let mut updates = 0;
    let mut observing = true;
    let mut updates_enabled = true;
    let deadline = Instant::now() + ACTIVITY_LIMIT;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let finished = loop {
        if let Some(outcome) = *end.borrow() {
            break outcome;
        }
        tokio::select! {
            biased;
            result = end.changed() => {
                if result.is_err() { break End::Stopped; }
            },
            _ = tokio::time::sleep_until(deadline) => break End::Expired,
            line = lines.recv(), if observing => match line {
                Some(line) if line.event_id == event.event_id && line.scope == event.conversation_scope() => {
                    if line.settled {
                        // Goose/Host settlement is not a platform delivery receipt.
                        // Stop observing tools, but keep this card for the worker's outcome.
                        observing = false;
                        card.cancel.send_replace(true);
                        pending = "本轮执行已结束，正在发送回复。".to_owned();
                    } else {
                        pending = render(&line);
                    }
                },
                Some(_) => {},
                None => {
                    let state = card.state.lock().await;
                    if state.id.is_none() && !state.paused { break End::Failed; }
                    observing = false;
                    pending = "实时进度暂不可用，正在等待回复投递结果。".to_owned();
                },
            },
            _ = tick.tick() => {
                let check_id = event.event_id.clone();
                if ledger.blocking(move |ledger| ledger.is_latest_event(&check_id)).await != Ok(true) {
                    break End::Superseded;
                }
                if let Some(outcome) = *end.borrow() { break outcome; }
                if !updates_enabled || Instant::now() < next_update || pending == shown { continue; }
                let mut state = card.state.lock().await;
                if state.paused { continue; }
                let Ok(content) = markdown_card(&format!("**工作进度**\n\n{pending}")) else { break End::Failed; };
                // Final delivery takes this same lock before taking the ID.
                // An uncertain update makes the card unsafe for a final answer:
                // an externally delayed status request could overwrite it.
                let result = tokio::time::timeout(REQUEST_LIMIT, async {
                    let token = api.token().await?;
                    if let Some(id) = &state.id {
                        api.update_card(id, &content, &token).await?;
                        Ok::<_, crate::GatewayError>(id.clone())
                    } else {
                        api.reply(&event.event_id, &content, &token).await
                    }
                }).await;
                match result {
                    Ok(Ok(id)) => { state.id = Some(id); shown = pending.clone(); },
                    _ => {
                        state.reusable = false;
                        if state.id.is_none() { break End::Failed; }
                        observing = false;
                        updates_enabled = false;
                        card.cancel.send_replace(true);
                    },
                }
                updates += 1;
                if observing && updates >= MAX_UPDATES {
                    observing = false;
                    card.cancel.send_replace(true);
                    pending = "本轮进度观察已结束，正在等待回复投递结果。".to_owned();
                }
                next_update = Instant::now() + UPDATE_INTERVAL;
            },
        }
    };
    card.cancel.send_replace(true);
    let _ = subscription.await;
    let mut state = card.state.lock().await;
    if let Some(id) = &state.id {
        let outcome = end.borrow().unwrap_or(finished);
        if let Ok(content) = markdown_card(&format!("**工作进度**\n\n{}", outcome.content())) {
            let updated = tokio::time::timeout(REQUEST_LIMIT, async {
                let token = api.token().await?;
                api.update_card(id, &content, &token).await?;
                Ok::<_, crate::GatewayError>(())
            })
            .await;
            state.reusable &= matches!(updated, Ok(Ok(())));
        }
    }
}

fn render(line: &ProgressLine) -> String {
    let label = match line.category.as_deref() {
        Some("preparing") => "正在准备",
        Some("read_file") => "文件",
        Some("command") => "工作区",
        Some("history") => "历史记录",
        Some("image") => "图片",
        Some("plugin") => "业务工具",
        Some("final_result") => "回复",
        _ => "活动",
    };
    let step = line
        .step
        .map(|step| format!("第 {step} 步 · "))
        .unwrap_or_default();
    // This is the Host's already-filtered projection, never Goose titles,
    // arguments or thoughts. Render it as bounded, single-line literal text.
    let mut content = String::new();
    for ch in line.content.chars().filter(|ch| !ch.is_control()).take(200) {
        match ch {
            '<' => content.push_str("&lt;"),
            '>' => content.push_str("&gt;"),
            '&' => content.push_str("&amp;"),
            '\\' | '*' | '_' | '[' | ']' | '`' | '~' | '|' => {
                content.push('\\');
                content.push(ch);
            }
            _ => content.push(ch),
        }
    }
    format!("{step}{label}: {content}")
}
