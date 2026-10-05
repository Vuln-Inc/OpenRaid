//! Shared telemetry uses expandable per-agent slots, bounded previews, and disk-spooled activity.
//! The global board is deliberately not stored here: SQLite remains its durable source.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Take, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const STREAM_PREVIEW_BYTES: usize = 16 * 1024;
const DETAIL_BYTES: usize = 1024;
static NEXT_ACTIVITY_LOG: AtomicU64 = AtomicU64::new(0);

/// Activity is spooled lazily so idle agents consume no descriptors or disk space.
/// The fallback is bounded and explicitly identified when temporary storage fails.
#[derive(Default)]
struct ActivityLog {
    file: Option<File>,
    path: Option<PathBuf>,
    fallback: String,
    unavailable: bool,
    response_boundary_needed: bool,
}

impl ActivityLog {
    fn append(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.file.is_none() && !self.unavailable {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let number = NEXT_ACTIVITY_LOG.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "openraid-activity-{}-{stamp}-{number}.log",
                std::process::id()
            ));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    self.file = Some(file);
                    self.path = Some(path);
                }
                Err(_) => self.unavailable = true,
            }
        }
        if let Some(file) = self.file.as_mut() {
            if file
                .seek(SeekFrom::End(0))
                .and_then(|_| file.write_all(text.as_bytes()))
                .is_ok()
            {
                return;
            }
            self.unavailable = true;
        }
        append_preview(&mut self.fallback, text, STREAM_PREVIEW_BYTES);
    }

    fn snapshot(&self) -> ActivitySnapshot {
        // Freeze the complete byte length while holding the writer lock. A separate
        // handle then reads outside that lock without sharing the append cursor.
        let reader = self.path.as_ref().map(|path| {
            File::open(path).and_then(|file| {
                let size = file.metadata()?.len();
                Ok(file.take(size))
            })
        });
        let mut suffix = String::new();
        let reader = match reader {
            Some(Ok(reader)) => Some(reader),
            Some(Err(_)) => {
                suffix.push_str("\n[activity log could not be fully read]\n");
                None
            }
            None => None,
        };
        if self.unavailable {
            suffix.push_str(
                "\n[activity storage unavailable; only recent fallback activity retained]\n",
            );
            suffix.push_str(&self.fallback);
        }
        ActivitySnapshot { reader, suffix }
    }

    fn tail_snapshot(&self, max_bytes: usize) -> ActivitySnapshot {
        let mut snapshot = self.snapshot();
        if let Some(reader) = snapshot.reader.as_mut() {
            let length = reader.limit();
            let start = length.saturating_sub(max_bytes as u64);
            if reader.get_mut().seek(SeekFrom::Start(start)).is_ok() {
                reader.set_limit(length - start);
            } else {
                snapshot.reader = None;
                snapshot
                    .suffix
                    .push_str("\n[activity log could not be read]\n");
            }
        }
        snapshot.suffix = tail(&snapshot.suffix, max_bytes).to_owned();
        snapshot
    }
}

struct ActivitySnapshot {
    reader: Option<Take<File>>,
    suffix: String,
}

impl ActivitySnapshot {
    fn read_tail(mut self, max_bytes: usize) -> String {
        let mut bytes = Vec::new();
        if let Some(reader) = self.reader.as_mut() {
            if reader.read_to_end(&mut bytes).is_err() {
                self.suffix.push_str("\n[activity log could not be read]\n");
            }
        }
        // The range can begin in the middle of a UTF-8 code point.
        let start = bytes
            .iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(bytes.len());
        let mut text = String::from_utf8_lossy(&bytes[start..]).into_owned();
        text.push_str(&self.suffix);
        tail(&text, max_bytes).to_owned()
    }

    fn read(mut self) -> String {
        let mut text = String::new();
        if let Some(reader) = self.reader.as_mut() {
            if reader.read_to_string(&mut text).is_err() {
                text.push_str("\n[activity log could not be fully read]\n");
            }
        }
        text.push_str(&self.suffix);
        text
    }
}

impl Drop for ActivityLog {
    fn drop(&mut self) {
        // Close before unlinking: Windows does not permit removal of an open file.
        self.file.take();
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

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
    activity: Mutex<ActivityLog>,
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
    changes: tokio::sync::watch::Sender<u64>,
}

impl Metrics {
    /// Coalesced change notifications; subscribers read the latest snapshot.
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn changed(&self) {
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

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
            changes: tokio::sync::watch::channel(0).0,
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
                self.changed();
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
            self.changed();
        }
    }

    pub fn record_usage(&self, agent_id: &str, input: u64, output: u64, cached: u64) {
        if let Some(slot) = self.slot(agent_id) {
            slot.input_tokens.fetch_add(input, Ordering::Relaxed);
            slot.output_tokens.fetch_add(output, Ordering::Relaxed);
            slot.cached_tokens.fetch_add(cached, Ordering::Relaxed);
            self.changed();
        }
    }

    pub fn record_tool(&self, agent_id: &str) {
        if let Some(slot) = self.slot(agent_id) {
            slot.tools.fetch_add(1, Ordering::Relaxed);
            self.changed();
        }
    }

    pub fn record_retry(&self, agent_id: &str) {
        if let Some(slot) = self.slot(agent_id) {
            slot.retries.fetch_add(1, Ordering::Relaxed);
            self.changed();
        }
    }

    pub fn append_output(&self, agent_id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(slot) = self.slot(agent_id) {
            let mut activity = slot.activity.lock().unwrap_or_else(|e| e.into_inner());
            // Separate the next response from tool/status payload once, never
            // between streaming chunks or in the bounded generation preview.
            if activity.response_boundary_needed {
                activity.append("\n\n[response]\n");
                activity.response_boundary_needed = false;
            }
            activity.append(text);
            drop(activity);
            let mut preview = slot.stream.lock().unwrap_or_else(|e| e.into_inner());
            append_preview(&mut preview, text, STREAM_PREVIEW_BYTES);
            drop(preview);
            self.changed();
        }
    }

    pub fn set_detail(&self, agent_id: &str, text: &str) {
        if let Some(slot) = self.slot(agent_id) {
            let mut detail = slot.detail.lock().unwrap_or_else(|e| e.into_inner());
            let preview = tail(text, DETAIL_BYTES);
            if *detail != preview {
                let mut activity = slot.activity.lock().unwrap_or_else(|e| e.into_inner());
                activity.append(&format!("\n\n[status] {text}\n"));
                activity.response_boundary_needed = true;
                *detail = preview.to_owned();
                self.changed();
            }
        }
    }

    /// Append tool/process activity without disturbing the dashboard's generation preview.
    pub fn append_activity(&self, agent_id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(slot) = self.slot(agent_id) {
            let mut activity = slot.activity.lock().unwrap_or_else(|e| e.into_inner());
            activity.append(text);
            activity.response_boundary_needed = true;
            drop(activity);
            self.changed();
        }
    }

    pub fn record_tool_start(&self, agent_id: &str, name: &str, arguments: &str) {
        self.append_activity(agent_id, &format!("\n\n[tool] {name}\n{arguments}\n"));
    }

    pub fn record_tool_result(&self, agent_id: &str, name: &str, result: &str) {
        self.append_activity(agent_id, &format!("\n\n[result] {name}\n{result}\n"));
    }

    /// Load only the inspected agent's complete session activity from its spool.
    pub fn agent_activity(&self, agent_id: &str) -> String {
        self.slot(agent_id)
            .map(|slot| {
                let snapshot = slot
                    .activity
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .snapshot();
                snapshot.read()
            })
            .unwrap_or_default()
    }

    /// Read a bounded live transcript, including tools and command/PTY output.
    /// The complete transcript remains available through `agent_activity`.
    pub fn agent_activity_tail(&self, agent_id: &str, max_bytes: usize) -> String {
        self.slot(agent_id)
            .map(|slot| {
                let snapshot = slot
                    .activity
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .tail_snapshot(max_bytes);
                snapshot.read_tail(max_bytes)
            })
            .unwrap_or_default()
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

    /// Show the committed roster while retaining cumulative session telemetry.
    /// Removed workers can keep draining and recording activity without remaining
    /// in the live roster or contributing to its status counts.
    pub fn snapshot_for_members(&self, members: &[String]) -> MetricsSnapshot {
        let mut snapshot = self.snapshot();
        snapshot.agents.retain(|agent| members.contains(&agent.id));
        snapshot.active = snapshot
            .agents
            .iter()
            .filter(|agent| matches!(agent.status, AgentStatus::Thinking | AgentStatus::Tool))
            .count();
        snapshot.voted = snapshot
            .agents
            .iter()
            .filter(|agent| agent.status == AgentStatus::Voted)
            .count();
        snapshot.finished = snapshot
            .agents
            .iter()
            .filter(|agent| agent.status == AgentStatus::Finished)
            .count();
        snapshot
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

fn append_preview(preview: &mut String, text: &str, max_bytes: usize) {
    if text.len() >= max_bytes {
        *preview = tail(text, max_bytes).to_owned();
    } else {
        let retain = max_bytes.saturating_sub(text.len());
        if preview.len() > retain {
            let trim = boundary_after(preview, preview.len() - retain);
            preview.drain(..trim);
        }
        preview.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn change_notifications_coalesce_and_include_tool_activity() {
        let metrics = Metrics::new(1);
        metrics.append_output("agent-001", "before subscription");
        let mut changes = metrics.subscribe_changes();
        assert!(!changes.has_changed().unwrap());
        metrics.set_status("agent-001", AgentStatus::Tool);
        metrics.record_tool_start("agent-001", "run_command", "{}");
        metrics.append_activity("agent-001", "[stdout] hello\n");
        changes.changed().await.unwrap();
        assert!(metrics
            .agent_activity("agent-001")
            .contains("[stdout] hello"));
        assert_eq!(metrics.snapshot().agents[0].status, AgentStatus::Tool);
        assert!(!changes.has_changed().unwrap());
        metrics.append_output("missing", "ignored");
        assert!(!changes.has_changed().unwrap());
    }

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

    #[test]
    fn activity_preserves_early_generation_tools_and_process_output() {
        let metrics = Metrics::new(2);
        metrics.append_output("agent-001", "early generation\n");
        metrics.record_tool_start("agent-001", "run_command", "{\"command\":\"build\"}");
        metrics.append_activity("agent-001", "[stdout] building\n");
        metrics.record_tool_result("agent-001", "run_command", "{\"exit_code\":0}");
        metrics.append_output("agent-001", &"界".repeat(20_000));
        let activity = metrics.agent_activity("agent-001");
        assert!(activity.starts_with("early generation\n"));
        assert!(activity.contains("[tool] run_command"));
        assert!(activity.contains("[stdout] building"));
        assert!(activity.contains("[result] run_command"));
        assert!(activity.ends_with(&"界".repeat(20_000)));
        assert!(metrics.agent_output("agent-001").len() <= STREAM_PREVIEW_BYTES);
        assert!(metrics.agent_activity("agent-002").is_empty());
        assert!(metrics.agent_activity("missing-agent").is_empty());
    }

    #[test]
    fn activity_tail_is_bounded_unicode_safe_and_includes_process_output() {
        let metrics = Metrics::new(1);
        metrics.append_output("agent-001", &"界".repeat(20_000));
        metrics.append_activity("agent-001", "\n[stdout] hello\n");
        let activity = metrics.agent_activity_tail("agent-001", 100);
        assert!(activity.len() <= 100);
        assert!(activity.ends_with("[stdout] hello\n"));
        assert!(!activity.contains('\u{fffd}'));
        assert!(metrics.agent_activity_tail("agent-001", 0).is_empty());
        assert!(metrics.agent_activity_tail("missing", 100).is_empty());
    }

    #[test]
    fn activity_spool_is_lazy_and_removed_when_metrics_drop() {
        let metrics = Metrics::new(1);
        let slot = metrics.slot("agent-001").unwrap();
        assert!(slot.activity.lock().unwrap().path.is_none());
        metrics.append_output("agent-001", "hello");
        let path = slot.activity.lock().unwrap().path.clone().unwrap();
        assert!(path.is_file());
        assert_eq!(metrics.agent_activity("agent-001"), "hello");
        let snapshot = slot.activity.lock().unwrap().snapshot();
        metrics.append_output("agent-001", " world");
        assert_eq!(snapshot.read(), "hello");
        assert_eq!(metrics.agent_activity("agent-001"), "hello world");
        drop(slot);
        drop(metrics);
        assert!(!path.exists());
    }
}
