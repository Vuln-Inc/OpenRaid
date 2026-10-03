//! Shared telemetry uses expandable per-agent slots, atomics, and bounded stream previews.
//! The global board is deliberately not stored here: SQLite remains its durable source.
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Instant,
};

const STREAM_PREVIEW_BYTES: usize = 16 * 1024;
const DETAIL_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum AgentStatus {
    #[default]
    Starting,
    Thinking,
    Tool,
    Retry,
    Waiting,
    Voted,
    Finished,
    Error,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Thinking => "thinking",
            Self::Tool => "tool",
            Self::Retry => "retry",
            Self::Waiting => "waiting",
            Self::Voted => "voted",
            Self::Finished => "finished",
            Self::Error => "error",
        }
    }

    fn from_byte(value: u8) -> Self {
        match value {
            1 => Self::Thinking,
            2 => Self::Tool,
            3 => Self::Retry,
            4 => Self::Waiting,
            5 => Self::Voted,
            6 => Self::Finished,
            7 => Self::Error,
            _ => Self::Starting,
        }
    }
}

#[derive(Default)]
struct AgentSlot {
    status: AtomicU8,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    cached_tokens: AtomicU64,
    tools: AtomicU64,
    retries: AtomicU64,
    stream: Mutex<String>,
    detail: Mutex<String>,
}

#[derive(Clone, Debug)]
pub struct AgentSnapshot {
    pub id: String,
    pub status: AgentStatus,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub tools: u64,
    pub retries: u64,
}

#[derive(Clone, Debug)]
pub struct MetricsSnapshot {
    pub agents: Vec<AgentSnapshot>,
    pub elapsed_secs: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub tools: u64,
    pub retries: u64,
    pub active: usize,
    pub voted: usize,
    pub finished: usize,
}

pub struct Metrics {
    slots: RwLock<BTreeMap<String, Arc<AgentSlot>>>,
    started: Instant,
}

impl Metrics {
    pub fn agent_tail(&self, agent_id: &str, max_bytes: usize) -> String {
        self.slot(agent_id)
            .map(|slot| {
                let text = slot.stream.lock().unwrap_or_else(|e| e.into_inner());
                let start = boundary_after(&text, text.len().saturating_sub(max_bytes));
                text[start..].to_owned()
            })
            .unwrap_or_default()
    }

    pub fn new(agent_count: usize) -> Self {
        Self {
            slots: RwLock::new(
                (1..=agent_count)
                    .map(|number| (format!("agent-{number:03}"), Arc::new(AgentSlot::default())))
                    .collect(),
            ),
            started: Instant::now(),
        }
    }

    pub fn add_agent(&self, agent_id: &str) -> bool {
        match self
            .slots
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .entry(agent_id.to_owned())
        {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Arc::new(AgentSlot::default()));
                true
            }
            std::collections::btree_map::Entry::Occupied(_) => false,
        }
    }

    fn slot(&self, agent_id: &str) -> Option<Arc<AgentSlot>> {
        self.slots
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(agent_id)
            .cloned()
    }

    pub fn set_status(&self, agent_id: &str, status: AgentStatus) {
        if let Some(slot) = self.slot(agent_id) {
            slot.status.store(status as u8, Ordering::Relaxed);
        }
    }

    pub fn record_usage(&self, agent_id: &str, input: u64, output: u64, cached: u64) {
        if let Some(slot) = self.slot(agent_id) {
            slot.input_tokens.fetch_add(input, Ordering::Relaxed);
            slot.output_tokens.fetch_add(output, Ordering::Relaxed);
            slot.cached_tokens.fetch_add(cached, Ordering::Relaxed);
        }
    }

    pub fn record_tool(&self, agent_id: &str) {
        if let Some(slot) = self.slot(agent_id) {
            slot.tools.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_retry(&self, agent_id: &str) {
        if let Some(slot) = self.slot(agent_id) {
            slot.retries.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn append_output(&self, agent_id: &str, text: &str) {
        if let Some(slot) = self.slot(agent_id) {
            let mut preview = slot.stream.lock().unwrap_or_else(|e| e.into_inner());
            if text.len() >= STREAM_PREVIEW_BYTES {
                *preview = tail(text, STREAM_PREVIEW_BYTES).to_owned();
            } else {
                let retain = STREAM_PREVIEW_BYTES.saturating_sub(text.len());
                if preview.len() > retain {
                    let trim = boundary_after(&preview, preview.len() - retain);
                    preview.drain(..trim);
                }
                preview.push_str(text);
            }
        }
    }

    pub fn set_detail(&self, agent_id: &str, text: &str) {
        if let Some(slot) = self.slot(agent_id) {
            *slot.detail.lock().unwrap_or_else(|e| e.into_inner()) =
                tail(text, DETAIL_BYTES).to_owned();
        }
    }

    /// Copy only the selected agent's stream, rather than 500 stream buffers per frame.
    pub fn agent_output(&self, agent_id: &str) -> String {
        self.slot(agent_id)
            .map(|slot| {
                slot.stream
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn agent_detail(&self, agent_id: &str) -> String {
        self.slot(agent_id)
            .map(|slot| {
                slot.detail
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            })
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let slots = self.slots.read().unwrap_or_else(|error| error.into_inner());
        let mut result = MetricsSnapshot {
            agents: Vec::with_capacity(slots.len()),
            elapsed_secs: self.started.elapsed().as_secs_f64(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            tools: 0,
            retries: 0,
            active: 0,
            voted: 0,
            finished: 0,
        };
        for (id, slot) in slots.iter() {
            let status = AgentStatus::from_byte(slot.status.load(Ordering::Relaxed));
            let agent = AgentSnapshot {
                id: id.clone(),
                status,
                input_tokens: slot.input_tokens.load(Ordering::Relaxed),
                output_tokens: slot.output_tokens.load(Ordering::Relaxed),
                cached_tokens: slot.cached_tokens.load(Ordering::Relaxed),
                tools: slot.tools.load(Ordering::Relaxed),
                retries: slot.retries.load(Ordering::Relaxed),
            };
            result.input_tokens += agent.input_tokens;
            result.output_tokens += agent.output_tokens;
            result.cached_tokens += agent.cached_tokens;
            result.tools += agent.tools;
            result.retries += agent.retries;
            result.active +=
                usize::from(matches!(status, AgentStatus::Thinking | AgentStatus::Tool));
            result.voted += usize::from(status == AgentStatus::Voted);
            result.finished += usize::from(status == AgentStatus::Finished);
            result.agents.push(agent);
        }
        result
    }
}

fn boundary_after(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

fn tail(text: &str, max_bytes: usize) -> &str {
    &text[boundary_after(text, text.len().saturating_sub(max_bytes))..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn five_hundred_tasks_account_without_losing_updates() {
        let metrics = Arc::new(Metrics::new(500));
        let mut tasks = tokio::task::JoinSet::new();
        for index in 1..=500 {
            let metrics = metrics.clone();
            tasks.spawn(async move {
                let id = format!("agent-{index:03}");
                for _ in 0..100 {
                    metrics.record_usage(&id, 3, 2, 1);
                    metrics.record_tool(&id);
                }
                metrics.set_status(&id, AgentStatus::Voted);
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        let snapshot = metrics.snapshot();
        assert_eq!(
            (
                snapshot.input_tokens,
                snapshot.output_tokens,
                snapshot.tools
            ),
            (150_000, 100_000, 50_000)
        );
        assert_eq!(snapshot.voted, 500);
    }

    #[test]
    fn preview_is_bounded_and_unicode_safe() {
        let metrics = Metrics::new(1);
        metrics.append_output("agent-001", &"界".repeat(20_000));
        metrics.append_output("agent-001", "শেষ");
        let output = metrics.agent_output("agent-001");
        assert!(output.len() <= STREAM_PREVIEW_BYTES);
        assert!(output.ends_with("শেষ"));
    }
}
