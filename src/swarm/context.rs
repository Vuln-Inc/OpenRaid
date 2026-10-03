//! Bounded model context inspired by OpenCode's completed-turn compaction.
//!
//! The durable global board is independent of this window. Summaries never
//! replace board records or authorize skipping unread board pages.

use std::{collections::HashSet, io::Write, sync::Arc};

use anyhow::{bail, Result};
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct ContextConfig {
    pub max_tokens: usize,
    pub reserve_output_tokens: usize,
    pub retain_recent_tokens: usize,
    pub summary_max_tokens: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_tokens: 32_768,
            reserve_output_tokens: 4_096,
            retain_recent_tokens: 8_192,
            summary_max_tokens: 2_048,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompactionPlan {
    /// Send this as a user message to the provider with tools disabled.
    pub prompt: String,
    pub removed_tokens: usize,
    pub retained_tokens: usize,
    prefix_len: usize,
    revision: u64,
}

#[derive(Debug)]
pub struct ContextWindow {
    system: Arc<str>,
    config: ContextConfig,
    history: Vec<Value>,
    sizes: Vec<usize>,
    history_tokens: usize,
    system_tokens: usize,
    revision: u64,
    compactions: u64,
    model_revision: u64,
}

/// Serialize a context without cloning it while awaiting shared HTTP admission.
/// Only admitted requests allocate their wire body, independent of swarm size.
pub struct ContextMessages<'a> {
    system: &'a str,
    history: &'a [Value],
}

impl Serialize for ContextMessages<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct SystemMessage<'a> {
            role: &'static str,
            content: &'a str,
        }

        let mut sequence = serializer.serialize_seq(Some(self.history.len() + 1))?;
        sequence.serialize_element(&SystemMessage {
            role: "system",
            content: self.system,
        })?;
        for message in self.history {
            sequence.serialize_element(message)?;
        }
        sequence.end()
    }
}

impl ContextWindow {
    pub fn select_model(&mut self, revision: u64, config: ContextConfig) {
        if self.model_revision != revision {
            self.reconfigure(config);
            self.model_revision = revision;
        }
    }
    pub fn reconfigure(&mut self, config: ContextConfig) {
        self.config = config;
        // Signed/encrypted reasoning belongs to its original model/account.
        // Keep textual history and complete tool groups across a model switch.
        for message in &mut self.history {
            if let Some(object) = message.as_object_mut() {
                object.retain(|key, _| !key.starts_with("_openraid_"));
            }
        }
        self.sizes = self.history.iter().map(estimate_message_tokens).collect();
        self.history_tokens = self.sizes.iter().sum();
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn new(system: Arc<str>, config: ContextConfig) -> Self {
        let system_tokens = estimate_text_tokens(&system).saturating_add(8);
        Self {
            system,
            config,
            history: Vec::new(),
            sizes: Vec::new(),
            history_tokens: 0,
            system_tokens,
            revision: 0,
            compactions: 0,
            model_revision: 0,
        }
    }

    pub fn push(&mut self, message: Value) {
        let size = estimate_message_tokens(&message);
        self.history_tokens = self.history_tokens.saturating_add(size);
        self.history.push(message);
        self.sizes.push(size);
        self.revision = self.revision.wrapping_add(1);
    }

    /// The first message is immutable across turns, enabling provider prefix caching.
    pub fn messages(&self) -> Vec<Value> {
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        messages.push(json!({"role": "system", "content": self.system.as_ref()}));
        messages.extend(self.history.iter().cloned());
        messages
    }

    pub fn borrowed_messages(&self) -> ContextMessages<'_> {
        ContextMessages {
            system: &self.system,
            history: &self.history,
        }
    }

    pub fn estimated_tokens(&self) -> usize {
        self.system_tokens.saturating_add(self.history_tokens)
    }

    pub fn system_tokens(&self) -> usize {
        self.system_tokens
    }

    pub fn available_tokens(&self) -> usize {
        self.config
            .max_tokens
            .saturating_sub(self.config.reserve_output_tokens)
    }

    /// A size blocker, not an operation timeout. Callers must surface it and
    /// preserve/checkpoint all history instead of issuing unchanged oversized
    /// requests or silently removing an indivisible tool/board entry.
    pub fn has_oversized_indivisible_prefix(&self) -> bool {
        self.estimated_tokens() > self.available_tokens() && self.forced_compaction_plan().is_none()
    }

    pub fn compactions(&self) -> u64 {
        self.compactions
    }

    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// Select a completed prefix without cutting any assistant/tool-result group.
    /// Work is linear in the current bounded context, never the durable session.
    pub fn compaction_plan(&self) -> Option<CompactionPlan> {
        self.plan(false)
    }

    /// Recover from a provider's actual context overflow even when the local
    /// byte heuristic had not yet reached its proactive trigger.
    pub fn forced_compaction_plan(&self) -> Option<CompactionPlan> {
        self.plan(true)
    }

    fn plan(&self, forced: bool) -> Option<CompactionPlan> {
        let available = self
            .config
            .max_tokens
            .saturating_sub(self.config.reserve_output_tokens);
        // Start before the absolute ceiling to leave room for the next tool result.
        let trigger = available.saturating_mul(85) / 100;
        if (!forced && self.estimated_tokens() <= trigger) || self.history.len() < 3 {
            return None;
        }

        let instruction = compaction_instruction(self.config.summary_max_tokens);
        // Backlog catch-up may need several staged summaries. Never send the
        // entire unbounded backlog in one summary request. The margin covers
        // the summarizer's system message and provider message framing.
        let prefix_budget = available
            .saturating_sub(estimate_text_tokens(&instruction))
            .saturating_sub(128);

        let recent_budget = self.config.retain_recent_tokens.min(
            available
                .saturating_sub(self.system_tokens)
                .saturating_sub(self.config.summary_max_tokens)
                / 2,
        );
        let mut desired_cut = self.history.len().saturating_sub(2);
        let mut recent_tokens = self.sizes[desired_cut..]
            .iter()
            .copied()
            .fold(0usize, usize::saturating_add);
        while !forced && desired_cut > 0 {
            let older = desired_cut - 1;
            let expanded = recent_tokens.saturating_add(self.sizes[older]);
            if expanded > recent_budget {
                break;
            }
            recent_tokens = expanded;
            desired_cut = older;
        }

        let mut pending = HashSet::new();
        let mut safe_cut = 0usize;
        let mut prefix_tokens = 0usize;
        let mut safe_tokens = 0usize;
        for (index, message) in self.history.iter().take(desired_cut).enumerate() {
            prefix_tokens = prefix_tokens.saturating_add(self.sizes[index]);
            if prefix_tokens > prefix_budget {
                break;
            }
            if message.get("role").and_then(Value::as_str) == Some("assistant") {
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        if let Some(id) = call.get("id").and_then(Value::as_str) {
                            pending.insert(id.to_owned());
                        }
                    }
                }
            }
            if message.get("role").and_then(Value::as_str) == Some("tool") {
                if let Some(id) = message.get("tool_call_id").and_then(Value::as_str) {
                    pending.remove(id);
                }
            }
            if pending.is_empty() {
                safe_cut = index + 1;
                safe_tokens = prefix_tokens;
            }
        }
        if safe_cut == 0 || (!forced && safe_tokens <= self.config.summary_max_tokens) {
            return None;
        }

        let conversation = serde_json::to_string(&self.history[..safe_cut]).ok()?;
        let instruction =
            compaction_instruction(self.config.summary_max_tokens.min(safe_tokens / 2).max(1));
        let prompt = format!("{instruction}{conversation}");
        Some(CompactionPlan {
            prompt,
            removed_tokens: safe_tokens,
            retained_tokens: self.history_tokens.saturating_sub(safe_tokens),
            prefix_len: safe_cut,
            revision: self.revision,
        })
    }

    /// A failed or stale summary leaves the entire original context intact.
    pub fn apply_summary(&mut self, plan: &CompactionPlan, summary: &str) -> Result<()> {
        if plan.revision != self.revision || plan.prefix_len > self.history.len() {
            bail!("context changed after compaction was planned");
        }
        let summary = summary.trim();
        if summary.is_empty() {
            bail!("provider returned an empty compaction summary");
        }
        let message = json!({
            "role": "user",
            "content": format!("[Continuation summary of older completed turns]\n{summary}\n[The durable global board is unchanged; continue reading it by cursor.]"),
        });
        let size = estimate_message_tokens(&message);
        if size >= plan.removed_tokens {
            bail!("compaction summary is not smaller than the original prefix");
        }
        self.history.splice(..plan.prefix_len, [message]);
        self.sizes.splice(..plan.prefix_len, [size]);
        self.history_tokens = self
            .history_tokens
            .saturating_sub(plan.removed_tokens)
            .saturating_add(size);
        self.revision = self.revision.wrapping_add(1);
        self.compactions = self.compactions.saturating_add(1);
        Ok(())
    }
}

fn compaction_instruction(summary_max_tokens: usize) -> String {
    format!(
        "Summarize this completed conversation prefix for another instance of the same agent. \
         Return only a concise continuation summary, within {summary_max_tokens} tokens. Preserve \
         the owner's objective and corrections, hard constraints, negotiated decisions, changed \
         paths, verification evidence, unresolved failures, next actions, and the exact global-board \
         read cursor if present. Preserve identifiers and tool outcomes needed to continue. Existing \
         continuation summaries are part of the history and must be merged. The original board \
         remains fully available through board_read; this summary does not replace, filter, or \
         authorize skipping any board message. Do not invent outcomes.\n\nCompleted conversation prefix:\n"
    )
}

/// A cautious byte-based heuristic, including JSON tool arguments and framing.
/// Actual provider usage remains the authority for accounting; no tokenizer copy
/// or vocabulary allocation is required in each of hundreds of workers.
pub fn estimate_text_tokens(text: &str) -> usize {
    let mut counter = TokenCounter::default();
    counter.count(text.as_bytes());
    counter.tokens()
}

pub fn estimate_message_tokens(message: &Value) -> usize {
    let mut counter = TokenCounter::default();
    // Count the wire representation directly, without allocating a second
    // potentially large copy of each tool result while 500 workers push turns.
    let _ = serde_json::to_writer(&mut counter, message);
    counter.tokens().saturating_add(8)
}

#[derive(Default)]
struct TokenCounter {
    ascii: usize,
    non_ascii: usize,
}

impl TokenCounter {
    fn count(&mut self, bytes: &[u8]) {
        let ascii = bytes.iter().filter(|byte| byte.is_ascii()).count();
        self.ascii = self.ascii.saturating_add(ascii);
        // Rare Unicode can tokenize into individual UTF-8 bytes. Budget it
        // accordingly rather than underestimating multilingual tool output.
        self.non_ascii = self.non_ascii.saturating_add(bytes.len() - ascii);
    }

    fn tokens(&self) -> usize {
        (self.ascii.saturating_add(2) / 3).saturating_add(self.non_ascii)
    }
}

impl Write for TokenCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.count(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn swarm_system_prompt(objective: &str, workspace: &str, agents: usize) -> Arc<str> {
    Arc::from(format!(
        "You are an equal participant in an openraid swarm initially configured for {agents} concurrent agents. \
         Membership can change during work; durable join/removal notices on the global board \
         are authoritative for the current roster. \
         Your identity is supplied separately. You have zero pre-assigned roles, specialties, \
         responsibilities, ownership, or preferred contributions. Work in {workspace}.\n\n\
         OWNER OBJECTIVE\n{objective}\n\n\
         COORDINATION\n\
         Read the shared global board immediately and frequently. Read every unread page in \
         sequence until caught up, keeping the returned cursor. Converse on the board before \
         decisions or edits, negotiate organically, and dynamically self-partition. All agents \
         see the same unpartitioned board. Never filter by topic, segment channels, drop messages, \
         create file claims, establish ownership systems, or lock files for agent coordination. \
         Only board cursor/offset/pagination is allowed. Consult other agents to avoid duplicated \
         work and resolve blockers collaboratively. Owner messages are explicitly marked and \
         must be recognized as steering the common objective. Board coordination is advisory for \
         workspace and MCP tools: new messages arriving after a read do not block execution. \
         Keep making progress while checking updates regularly; only positive completion votes \
         require a fully current board cursor.\n\n\
         EXECUTION\n\
         Build actual working changes using native tools, inspect the workspace, and verify \
         meaningful functionality. Do not settle for plans. Do not introduce duration-based \
         operation or request timeouts. Work cooperatively and keep evidence on the board. \
         Native workspace tools expose reads, search, patches, and commands. No role is fixed. \
         While the shared objective is unfinished, seek useful remaining work, help peers, \
         or post a concrete blocker and what would resolve it; do not silently idle.\n\n\
         COMPLETION\n\
         Do not quit autonomously. Vote done only after posting evidence that the shared objective \
         is complete and after checking the whole board. Retract your vote when new work or owner \
         corrections appear. The harness hard-gates termination through swarm consensus and a \
         grace period. Continue coordinating until the harness stops the swarm."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordination_prompt_keeps_execution_advisory_and_completion_fresh() {
        let prompt = swarm_system_prompt("verify changes", "workspace", 8);
        assert!(prompt.contains(
            "workspace and MCP tools: new messages arriving after a read do not block execution"
        ));
        assert!(
            prompt.contains("only positive completion votes require a fully current board cursor")
        );
        assert!(prompt.contains("Read the shared global board immediately and frequently"));
    }

    fn window() -> ContextWindow {
        ContextWindow::new(
            Arc::from("immutable system"),
            ContextConfig {
                max_tokens: 2_500,
                reserve_output_tokens: 100,
                retain_recent_tokens: 80,
                summary_max_tokens: 40,
            },
        )
    }

    fn text(role: &str, n: usize) -> Value {
        json!({"role": role, "content": "x".repeat(n)})
    }

    #[test]
    fn compaction_preserves_parallel_tool_groups_and_stable_system() {
        let mut context = window();
        context.push(text("user", 500));
        context.push(json!({"role":"assistant","tool_calls":[
            {"id":"a","type":"function","function":{"name":"read_file","arguments":"{}"}},
            {"id":"b","type":"function","function":{"name":"board_read","arguments":"{}"}}
        ]}));
        context.push(json!({"role":"tool","tool_call_id":"a","content":"x".repeat(2_200)}));
        context.push(json!({"role":"tool","tool_call_id":"b","content":"x".repeat(2_200)}));
        context.push(text("user", 500));
        context.push(text("assistant", 500));
        let original = context.messages();
        assert_eq!(
            serde_json::to_value(context.borrowed_messages()).unwrap(),
            json!(original)
        );
        let plan = context.compaction_plan().expect("over budget");
        assert_eq!(plan.prefix_len, 4);
        context
            .apply_summary(&plan, "read two files, board cursor 19")
            .unwrap();
        let compacted = context.messages();
        assert_eq!(original[0], compacted[0]);
        assert_eq!(&compacted[2..], &original[5..]);
        assert!(context.estimated_tokens() < 700);
        assert_eq!(context.compactions(), 1);
    }

    #[test]
    fn incomplete_tool_group_is_not_split() {
        let mut context = window();
        context.push(text("user", 500));
        context.push(json!({"role":"assistant","tool_calls":[{"id":"a"},{"id":"b"}]}));
        context.push(json!({"role":"tool","tool_call_id":"a","content":"x".repeat(6_000)}));
        context.push(text("user", 150));
        context.push(text("assistant", 150));
        let plan = context.compaction_plan().unwrap();
        assert_eq!(plan.prefix_len, 1);
        context.apply_summary(&plan, "objective retained").unwrap();
        assert!(context
            .messages()
            .iter()
            .any(|message| message["tool_calls"][1]["id"] == "b"));
    }

    #[test]
    fn stale_and_nonshrinking_summaries_do_not_erase_history() {
        let mut context = window();
        for _ in 0..5 {
            context.push(text("user", 1_600));
        }
        let plan = context.compaction_plan().unwrap();
        let before = context.messages();
        assert!(context.apply_summary(&plan, "").is_err());
        assert!(context.apply_summary(&plan, &"x".repeat(10_000)).is_err());
        assert_eq!(before, context.messages());
        context.push(text("user", 10));
        assert!(context.apply_summary(&plan, "valid but stale").is_err());
        assert_eq!(context.len(), 6);
    }

    #[test]
    fn small_context_does_not_request_compaction() {
        let mut context = window();
        context.push(text("user", 20));
        context.push(text("assistant", 20));
        assert!(context.compaction_plan().is_none());
    }

    #[test]
    fn unicode_and_json_arguments_are_included_without_tokenizer_state() {
        assert!(estimate_text_tokens("🦀") >= 4);
        let small = json!({"role":"assistant","tool_calls":[{"function":{"arguments":"{}"}}]});
        let large =
            json!({"role":"assistant","tool_calls":[{"function":{"arguments":"α".repeat(1_000)}}]});
        assert!(estimate_message_tokens(&large) > estimate_message_tokens(&small) + 1_000);
    }

    #[test]
    fn staged_plans_bound_summary_requests_during_large_backlog_catchup() {
        let mut context = window();
        for _ in 0..40 {
            context.push(text("user", 1_000));
        }
        let mut count = 0;
        while let Some(plan) = context.compaction_plan() {
            assert!(estimate_text_tokens(&plan.prompt) < 2_400);
            context
                .apply_summary(&plan, "verified prior messages; cursor retained")
                .unwrap();
            count += 1;
            assert!(count < 40);
        }
        assert!(count > 1);
        assert!(context.estimated_tokens() < 2_500);
    }

    #[test]
    fn actual_overflow_can_force_compaction_before_heuristic_trigger() {
        let mut context = window();
        context.push(text("user", 1_000));
        context.push(text("assistant", 20));
        context.push(text("user", 20));
        assert!(context.compaction_plan().is_none());
        let plan = context.forced_compaction_plan().expect("completed prefix");
        context.apply_summary(&plan, "retained objective").unwrap();
    }

    #[test]
    fn forced_recovery_does_not_preserve_entire_small_history_under_default_budget() {
        let mut context =
            ContextWindow::new(Arc::from("stable objective"), ContextConfig::default());
        context.push(text("user", 1_000));
        context.push(text("assistant", 40));
        context.push(text("user", 40));
        assert!(context.compaction_plan().is_none());
        let before = context.estimated_tokens();
        let plan = context
            .forced_compaction_plan()
            .expect("force despite large tail budget");
        assert!(plan.removed_tokens < ContextConfig::default().summary_max_tokens);
        context
            .apply_summary(&plan, "prior objective retained")
            .unwrap();
        assert!(context.estimated_tokens() < before);
    }

    #[test]
    fn oversized_indivisible_histories_are_visible_and_never_discarded() {
        let mut context = window();
        context.push(text("user", 20_000));
        context.push(text("assistant", 20));
        context.push(text("user", 20));
        let original = context.messages();
        assert!(context.has_oversized_indivisible_prefix());
        assert!(context.forced_compaction_plan().is_none());
        assert_eq!(context.messages(), original);

        let context = ContextWindow::new(Arc::from("x".repeat(20_000)), window().config);
        assert!(context.system_tokens() > context.available_tokens());
        assert!(context.has_oversized_indivisible_prefix());
    }
}
