//! Tolerant parser for Claude Code transcripts (SPEC §9.4): unknown record types are kept
//! as `Record::Other` and never fail the parse. Fixtures in `fixtures/` were captured from
//! Claude Code 2.1.307 and 2.1.308 with `api-request*` records removed and the file
//! snapshot stripped.
//!
//! One parser serves the adapter (titles), the mirror and the import (SPEC §9.4): the
//! mirror reads [`Message::is_main`] records, takes the session's working directory from
//! [`first_cwd`] and the end of a turn from [`Message::stop_reason`].
//!
//! Record `uuid`s are shared between the jsonl file and stream-json output (verified on
//! 2.1.307, SPEC §8.1), which is what makes mirror/adapter event ids line up.

use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Record {
    User(Message),
    Assistant(Message),
    /// The session title Claude Code generates after the first prompt (SPEC §8.1, §9).
    AiTitle(AiTitle),
    /// The title a person gave the session (`/rename`, the VS Code tab's rename); it wins over
    /// [`Record::AiTitle`] (SPEC §9).
    CustomTitle(CustomTitle),
    /// What the CLI adds to the conversation: among others a message a person typed while a
    /// turn ran ([`Attachment::queued_prompt`]).
    Attachment(Attachment),
    /// A line of the CLI's own: a slash command's output, where a compaction began
    /// ([`System`]).
    System(System),
    /// Everything else: file-history-snapshot, queue-operation, last-prompt, cost-state,
    /// atis-latch, summary, ... (SPEC §9).
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub uuid: Uuid,
    #[serde(default)]
    pub parent_uuid: Option<Uuid>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Text the CLI injected as a user message (caveats, slash command output): not
    /// something a person typed.
    #[serde(default)]
    pub is_meta: bool,
    /// The summary that replaces the history after `/compact`.
    #[serde(default)]
    pub is_compact_summary: bool,
    /// An assistant record the CLI wrote itself for a failed API call (`<synthetic>`, with
    /// `error`), not for anything else it writes as a reply.
    #[serde(default)]
    pub is_api_error_message: bool,
    /// Who wrote a `user` record that no person typed: `{"kind": "task-notification",
    /// "producer": "session-task", ...}` for a finished background task (2.1.312).
    #[serde(default)]
    pub origin: Option<serde_json::Value>,
    pub message: serde_json::Value,
    /// `toolUseResult` of a `tool_result` record, the tool's own result: for an answered
    /// `AskUserQuestion` `{questions, answers}`, the answers keyed by question text. Only a
    /// result with `answers` is kept: other tools' results (a whole file for `Read`) would
    /// stay in memory with every record of a parsed transcript.
    #[serde(default, deserialize_with = "answers_only")]
    pub tool_use_result: Option<serde_json::Value>,
}

fn answers_only<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(d)?.filter(|v| v.get("answers").is_some()))
}

impl Message {
    /// A record of the session's main chain that shows in its history: not a subagent's
    /// (`isSidechain`), not injected by the CLI (`isMeta`, compact summary). These are the
    /// records the mirror maps to events and the foreign-writer check looks at (SPEC §9.3).
    pub fn is_main(&self) -> bool {
        !self.is_sidechain && !self.is_meta && !self.is_compact_summary
    }

    /// `message.stop_reason` of an assistant record: `end_turn` on the blocks of the turn's
    /// last API message, `tool_use` before a tool call (2.1.308). Every block of a message
    /// is a record of its own and carries the message's `stop_reason` in the transcript;
    /// stream-json leaves it `null`.
    pub fn stop_reason(&self) -> Option<&str> {
        self.message.get("stop_reason")?.as_str()
    }

    /// `origin.kind`: `task-notification` for a finished background task; a prompt a person
    /// typed has no `origin`.
    pub fn origin_kind(&self) -> Option<&str> {
        self.origin.as_ref()?.get("kind")?.as_str()
    }

    /// `message.model` of an assistant record; `<synthetic>` for a reply the CLI wrote
    /// itself (an API error, "Credit balance is too low").
    pub fn model(&self) -> Option<&str> {
        self.message.get("model")?.as_str()
    }

    /// The content blocks; a plain string (a typed prompt) is one `text` block.
    pub fn blocks(&self) -> Vec<serde_json::Value> {
        match self.message.get("content") {
            Some(serde_json::Value::String(s)) => {
                vec![serde_json::json!({"type": "text", "text": s})]
            }
            Some(serde_json::Value::Array(a)) => a.clone(),
            _ => vec![],
        }
    }
}

/// An `attachment` record. A message a person typed while a turn ran, which the CLI gave the
/// running turn, is `{"type": "queued_command", "commandMode": "prompt", "prompt": [blocks] or
/// "text", "origin": {"kind": "human"}}` (no `origin` before 2.1.210, nor on some later ones
/// that carry `delivery_id`; seen up to 2.1.318); a
/// peer's message has `origin.kind: peer` and `isMeta: true`, a finished background task
/// `commandMode: task-notification`. Fields are optional: the record keeps its place in the
/// fork tree whatever it holds.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(default)]
    pub uuid: Option<Uuid>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub attachment: serde_json::Value,
}

impl Attachment {
    /// The content blocks of a message a person typed while a turn ran, a plain string as one
    /// `text` block; `None` for any other attachment and for a subagent's.
    pub fn queued_prompt(&self) -> Option<Vec<serde_json::Value>> {
        let a = &self.attachment;
        let typed = a["type"] == "queued_command"
            && a["commandMode"] == "prompt"
            && matches!(a["origin"]["kind"].as_str(), None | Some("human"))
            && a["isMeta"] != true
            && !self.is_sidechain;
        if !typed {
            return None;
        }
        match &a["prompt"] {
            serde_json::Value::String(s) => {
                Some(vec![serde_json::json!({"type": "text", "text": s})])
            }
            serde_json::Value::Array(a) => Some(a.clone()),
            _ => None,
        }
    }
}

/// How the CLI's `/goal` stands after a `goal_status` attachment (2.1.312): it sets the goal
/// (`sentinel`, not `met`), checks it at each turn's end, and ends it by meeting it, judging it
/// impossible (`failed`) or `/goal clear` (`sentinel`, `met`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalState {
    Set,
    /// A check that did not pass; the CLI goes on.
    Unmet,
    Met,
    Failed,
    Cleared,
}

impl GoalState {
    /// The name the wire uses (`system goal`, SPEC 0.218).
    pub fn as_str(self) -> &'static str {
        match self {
            GoalState::Set => "set",
            GoalState::Unmet => "unmet",
            GoalState::Met => "met",
            GoalState::Failed => "failed",
            GoalState::Cleared => "cleared",
        }
    }
}

/// A `goal_status` attachment: `{"met": false, "sentinel": true, "condition": c}` when set,
/// `{"met": false, "condition": c, "reason": r}` for each unmet check, `{"met": true, ...,
/// "iterations", "durationMs", "tokens"}` when met, `"failed": true` in place of `met` when
/// impossible, `{"met": true, "sentinel": true, ...}` when cleared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRecord {
    pub uuid: Uuid,
    /// The record's own `timestamp`, as written.
    pub timestamp: Option<String>,
    pub condition: String,
    pub state: GoalState,
    pub reason: Option<String>,
    /// `iterations`: how many checks it took.
    pub checks: Option<u32>,
    pub duration_ms: Option<u64>,
    pub tokens: Option<u64>,
}

/// The [`GoalRecord`] of a transcript line; `None` for any other line and for a record without
/// `uuid` or `condition`. Lines that do not mention `goal_status` are not parsed.
pub fn goal_record(line: &str) -> Option<GoalRecord> {
    if !line.contains("\"goal_status\"") {
        return None;
    }
    let Ok(Record::Attachment(a)) = parse_line(line) else {
        return None;
    };
    goal_of(&a)
}

/// The [`GoalRecord`] of a parsed `attachment` record, under the rules of [`goal_record`].
pub fn goal_of(a: &Attachment) -> Option<GoalRecord> {
    let g = &a.attachment;
    if g["type"] != "goal_status" || a.is_sidechain {
        return None;
    }
    let sentinel = g["sentinel"] == true;
    let met = g["met"] == true;
    let state = match (sentinel, met, g["failed"] == true) {
        (true, false, _) => GoalState::Set,
        (true, true, _) => GoalState::Cleared,
        (false, _, true) => GoalState::Failed,
        (false, true, false) => GoalState::Met,
        (false, false, false) => GoalState::Unmet,
    };
    Some(GoalRecord {
        uuid: a.uuid?,
        timestamp: a.timestamp.clone(),
        condition: g["condition"].as_str()?.to_owned(),
        state,
        reason: g["reason"].as_str().map(str::to_owned),
        checks: g["iterations"].as_u64().and_then(|n| u32::try_from(n).ok()),
        duration_ms: g["durationMs"].as_u64(),
        tokens: g["tokens"].as_u64(),
    })
}

/// The last [`GoalRecord`] of a transcript: how the session's goal stood when the CLI last
/// wrote about it. Cheap like [`last_ai_title`].
pub fn last_goal_status(s: &str) -> Option<GoalRecord> {
    s.lines().rev().find_map(goal_record)
}

/// A `system` record (SPEC §9): `local_command` holds a slash command's output as `content`
/// `<local-command-stdout>…</local-command-stdout>` (or `-stderr`); `compact_boundary` is where
/// a compaction began, the summary record (`isCompactSummary`) comes after it; others
/// (`stop_hook_summary`, `turn_duration`, ...) say nothing for the history. Fields are optional:
/// the record keeps its place in the fork tree whatever it holds.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct System {
    #[serde(default)]
    pub uuid: Option<Uuid>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub content: serde_json::Value,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub is_meta: bool,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// `cwd` of the first record of `s` that has one. The folder name under `projects/` cannot be
/// decoded back (SPEC §9), so recorded `cwd`s are the source of a transcript's folder.
pub fn first_cwd(s: &str) -> Option<String> {
    s.lines().filter(|l| l.contains("\"cwd\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        v.get("cwd")?
            .as_str()
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
    })
}

/// `{"type": "ai-title", "aiTitle": "...", "sessionId": "..."}` (2.1.307). The CLI may write
/// it more than once per session; the last one wins.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTitle {
    #[serde(default)]
    pub ai_title: String,
    /// Kept as text: a malformed id must not cost the title.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `{"type": "custom-title", "customTitle": "...", "sessionId": "..."}` (2.1.312); the last
/// one wins.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTitle {
    #[serde(default)]
    pub custom_title: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// The title of a transcript: its last non-empty `custom-title`, else its last non-empty
/// `ai-title`, trimmed. Cheap like [`last_ai_title`].
pub fn last_title(s: &str) -> Option<String> {
    s.lines()
        .rev()
        .filter(|l| l.contains("\"custom-title\""))
        .find_map(|l| match parse_line(l) {
            Ok(Record::CustomTitle(t)) if !t.custom_title.trim().is_empty() => {
                Some(t.custom_title.trim().to_string())
            }
            _ => None,
        })
        .or_else(|| last_ai_title(s))
}

/// The last non-empty `ai-title` of a transcript, trimmed. Lines that do not mention
/// `ai-title` are not parsed, so this is cheap on long transcripts.
pub fn last_ai_title(s: &str) -> Option<String> {
    s.lines()
        .rev()
        .filter(|l| l.contains("\"ai-title\""))
        .find_map(|l| match parse_line(l) {
            Ok(Record::AiTitle(t)) if !t.ai_title.trim().is_empty() => {
                Some(t.ai_title.trim().to_string())
            }
            _ => None,
        })
}

/// Token counts summed over the models of a `modelUsage` object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenSums {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenSums {
    /// The growth since `base`, per count; never negative.
    pub fn since(self, base: TokenSums) -> TokenSums {
        TokenSums {
            input: self.input.saturating_sub(base.input),
            output: self.output.saturating_sub(base.output),
            cache_read: self.cache_read.saturating_sub(base.cache_read),
            cache_write: self.cache_write.saturating_sub(base.cache_write),
        }
    }

    /// The larger of each count of the two.
    pub fn max(self, other: TokenSums) -> TokenSums {
        TokenSums {
            input: self.input.max(other.input),
            output: self.output.max(other.output),
            cache_read: self.cache_read.max(other.cache_read),
            cache_write: self.cache_write.max(other.cache_write),
        }
    }

    /// All four counts added together; saturating.
    pub fn total(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

/// The `inputTokens`, `outputTokens`, `cacheReadInputTokens` and `cacheCreationInputTokens`
/// of a `modelUsage` object (a stream-json `result`'s or a `cost-state`'s: one entry per
/// model, cumulative over the CLI session) summed over its models. A missing or malformed
/// object and missing counts are zeros.
pub fn model_usage_tokens(model_usage: &serde_json::Value) -> TokenSums {
    let mut sums = TokenSums::default();
    for m in model_usage.as_object().into_iter().flat_map(|o| o.values()) {
        let n = |k: &str| m[k].as_u64().unwrap_or(0);
        sums.input = sums.input.saturating_add(n("inputTokens"));
        sums.output = sums.output.saturating_add(n("outputTokens"));
        sums.cache_read = sums.cache_read.saturating_add(n("cacheReadInputTokens"));
        sums.cache_write = sums
            .cache_write
            .saturating_add(n("cacheCreationInputTokens"));
    }
    sums
}

/// The cumulative cost state a CLI process starts from: see [`last_cost_state`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CostState {
    pub total_cost_usd: f64,
    pub tokens: TokenSums,
}

/// `totalCostUSD` and the summed `modelUsage` of the last `cost-state` record of a
/// transcript: the session's cumulative cost and tokens the CLI writes when its process ends
/// and starts from again on `--resume` (checked on 2.1.308, T27; SPEC §8.1). A record with a
/// bad `totalCostUSD` is skipped. Cheap like [`last_ai_title`].
pub fn last_cost_state(s: &str) -> Option<CostState> {
    s.lines()
        .rev()
        .filter(|l| l.contains("\"cost-state\""))
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["type"] != "cost-state" {
                return None;
            }
            let total_cost_usd = v["totalCostUSD"]
                .as_f64()
                .filter(|c| c.is_finite() && *c >= 0.0)?;
            Some(CostState {
                total_cost_usd,
                tokens: model_usage_tokens(&v["modelUsage"]),
            })
        })
}

/// Where a record hangs in the transcript's tree: every record with a `uuid` (messages,
/// attachments, system records) names its `parentUuid`. Two lines of turns from one record
/// are a fork (SPEC §9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub uuid: Uuid,
    #[serde(default)]
    pub parent_uuid: Option<Uuid>,
    #[serde(default)]
    pub is_sidechain: bool,
}

/// The [`Link`] of one line; `None` for records without a `uuid` and for bad lines.
pub fn link(line: &str) -> Option<Link> {
    serde_json::from_str(line).ok()
}

/// One line of a transcript. Lines that are not valid JSON objects are reported as
/// `Err` so callers can count and log them without stopping (SPEC §9.4).
pub fn parse_line(line: &str) -> Result<Record, serde_json::Error> {
    serde_json::from_str(line)
}

/// Parse a whole file, skipping blank lines; returns records and the number of
/// unparseable lines.
pub fn parse_str(s: &str) -> (Vec<Record>, usize) {
    let mut out = Vec::new();
    let mut bad = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Ok(r) => out.push(r),
            Err(_) => bad += 1,
        }
    }
    (out, bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../fixtures/tool-call-2.1.307.jsonl");

    /// SPEC 0.218: every shape of the `goal_status` attachment (2.1.312) maps to its state.
    #[test]
    fn goal_status_by_state() {
        let line = |attachment: serde_json::Value| {
            serde_json::json!({
                "type": "attachment",
                "uuid": "36c72c19-b4ad-4d95-a482-ce4e281952cb",
                "timestamp": "2026-11-22T19:33:57.895Z",
                "attachment": attachment,
            })
            .to_string()
        };
        let state = |a: serde_json::Value| goal_record(&line(a)).unwrap();
        let g = state(
            serde_json::json!({"type": "goal_status", "met": false, "sentinel": true, "condition": "C"}),
        );
        assert_eq!(
            (g.state, g.condition.as_str(), g.reason),
            (GoalState::Set, "C", None)
        );
        assert_eq!(g.uuid.to_string(), "36c72c19-b4ad-4d95-a482-ce4e281952cb");
        assert_eq!(g.timestamp.as_deref(), Some("2026-11-22T19:33:57.895Z"));
        let g = state(
            serde_json::json!({"type": "goal_status", "met": false, "condition": "C", "reason": "R"}),
        );
        assert_eq!(
            (g.state, g.reason.as_deref(), g.checks),
            (GoalState::Unmet, Some("R"), None)
        );
        let g = state(
            serde_json::json!({"type": "goal_status", "met": true, "condition": "C", "reason": "R",
            "iterations": 1, "durationMs": 21808, "tokens": 549}),
        );
        assert_eq!(
            (g.state, g.checks, g.duration_ms, g.tokens),
            (GoalState::Met, Some(1), Some(21808), Some(549))
        );
        let g = state(
            serde_json::json!({"type": "goal_status", "met": true, "sentinel": true, "condition": "C"}),
        );
        assert_eq!(g.state, GoalState::Cleared);
        let g = state(
            serde_json::json!({"type": "goal_status", "met": false, "failed": true, "condition": "C",
            "reason": "R", "iterations": 4, "durationMs": 9, "tokens": 7}),
        );
        assert_eq!((g.state, g.checks), (GoalState::Failed, Some(4)));
        // Not a goal: another attachment, another record, a goal without a condition.
        assert!(goal_record(&line(serde_json::json!({"type": "queued_command"}))).is_none());
        assert!(goal_record(&line(
            serde_json::json!({"type": "goal_status", "met": false})
        ))
        .is_none());
        assert!(goal_record(r#"{"type":"user","uuid":"36c72c19-b4ad-4d95-a482-ce4e281952cb","message":{"content":"goal_status"}}"#).is_none());
    }

    /// SPEC 0.218: the last goal record of a transcript, whatever its state.
    #[test]
    fn the_last_goal_status_of_a_transcript() {
        let at = |u: &str, a: serde_json::Value| {
            serde_json::json!({"type": "attachment", "uuid": u, "attachment": a}).to_string()
        };
        let set = at(
            "00000000-0000-0000-0000-000000000001",
            serde_json::json!({"type": "goal_status", "met": false, "sentinel": true, "condition": "C"}),
        );
        let met = at(
            "00000000-0000-0000-0000-000000000002",
            serde_json::json!({"type": "goal_status", "met": true, "condition": "C"}),
        );
        let t = format!("{set}\n{met}\n{{\"type\":\"user\"}}\n");
        assert_eq!(last_goal_status(&t).unwrap().state, GoalState::Met);
        assert!(last_goal_status("{\"type\":\"user\"}\n").is_none());
    }

    #[test]
    fn fixture_parses_and_keeps_main_chain() {
        let (records, bad) = parse_str(FIXTURE);
        assert_eq!(bad, 0);
        let main: Vec<&Message> = records
            .iter()
            .filter_map(|r| match r {
                Record::User(m) | Record::Assistant(m) if !m.is_sidechain => Some(m),
                _ => None,
            })
            .collect();
        // user prompt, assistant tool_use, user tool_result, assistant text
        assert_eq!(main.len(), 4);
        assert!(records.iter().any(|r| matches!(r, Record::Other)));
    }

    /// SPEC §8.1: the `ai-title` record, written twice in this run.
    #[test]
    fn ai_title_from_fixture() {
        let (records, _) = parse_str(FIXTURE);
        let titles: Vec<&AiTitle> = records
            .iter()
            .filter_map(|r| match r {
                Record::AiTitle(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(titles.len(), 2);
        assert_eq!(
            titles[0].session_id.as_deref(),
            Some("e47a9949-9b46-4e74-9701-525d009cf93e")
        );
        assert_eq!(
            last_ai_title(FIXTURE).as_deref(),
            Some("Hqc-uuid-test echo command")
        );
        let later = format!(
            "{FIXTURE}{}\n",
            r#"{"type":"ai-title","aiTitle":" Renamed "}"#
        );
        assert_eq!(last_ai_title(&later).as_deref(), Some("Renamed"));
        let blank = format!("{FIXTURE}{}\n", r#"{"type":"ai-title","aiTitle":""}"#);
        assert_eq!(
            last_ai_title(&blank).as_deref(),
            Some("Hqc-uuid-test echo command")
        );
        assert_eq!(
            last_ai_title(include_str!("../fixtures/permission-allow-2.1.307.jsonl")),
            None
        );
    }

    /// A person's `custom-title` (2.1.312) is the title over any `ai-title`, also one written
    /// after it; without one the last `ai-title` is.
    #[test]
    fn a_custom_title_wins() {
        assert_eq!(
            last_title(FIXTURE).as_deref(),
            Some("Hqc-uuid-test echo command")
        );
        let renamed = format!(
            "{FIXTURE}{}\n{}\n{}\n",
            r#"{"type":"custom-title","customTitle":" Mine ","sessionId":"x"}"#,
            r#"{"type":"ai-title","aiTitle":"Later","sessionId":"x"}"#,
            r#"{"type":"custom-title","customTitle":"","sessionId":"x"}"#,
        );
        assert_eq!(last_title(&renamed).as_deref(), Some("Mine"));
    }

    /// `system` records parse whatever they hold (SPEC §9.4): a `local_command` with its
    /// output, one whose `content` is no string, one without a `uuid`.
    #[test]
    fn system_records_parse() {
        let out = r#"{"type":"system","subtype":"local_command","content":"<local-command-stdout>ok</local-command-stdout>","isMeta":false,"uuid":"73698000-0e8a-4b4c-9d8e-1f2a3b4c5d6e"}"#;
        let Ok(Record::System(s)) = parse_line(out) else {
            panic!("{out}");
        };
        assert_eq!(s.subtype.as_deref(), Some("local_command"));
        assert_eq!(
            s.content.as_str(),
            Some("<local-command-stdout>ok</local-command-stdout>")
        );
        for odd in [
            r#"{"type":"system","subtype":"stop_hook_summary","content":[1,2]}"#,
            r#"{"type":"system","subtype":"turn_duration","durationMs":1200}"#,
        ] {
            assert!(matches!(parse_line(odd), Ok(Record::System(_))), "{odd}");
        }
    }

    /// T27: the baseline of a resumed process is the last `cost-state`; the transcript before
    /// the resume (up to the first `cost-state`) has the first process's total.
    #[test]
    fn last_cost_state_from_fixture() {
        let state = |total_cost_usd, input, output, cache_read, cache_write| {
            Some(CostState {
                total_cost_usd,
                tokens: TokenSums {
                    input,
                    output,
                    cache_read,
                    cache_write,
                },
            })
        };
        let full = include_str!("../fixtures/cost-resume-2.1.308.jsonl");
        let last = state(0.0043361, 6, 90, 51870, 18859);
        assert_eq!(last_cost_state(full), last);
        let cut = full.find("\"cost-state\"").unwrap();
        let before_resume = &full[..full[cut..].find('\n').unwrap() + cut];
        assert_eq!(
            last_cost_state(before_resume),
            state(0.00398629, 4, 85, 27739, 18330)
        );
        assert_eq!(
            last_cost_state(FIXTURE),
            state(0.00393462, 1199, 120, 60832, 15732)
        );
        let first_turn = &full[..full.find("\"cost-state\"").unwrap()];
        assert_eq!(last_cost_state(first_turn), None, "process still running");
        let broken = format!("{full}{}\n", r#"{"type":"cost-state","totalCostUSD":"x"}"#);
        assert_eq!(last_cost_state(&broken), last);
        let no_usage = r#"{"type":"cost-state","totalCostUSD":1.5}"#;
        assert_eq!(last_cost_state(no_usage), state(1.5, 0, 0, 0, 0));
        let two_models = r#"{"type":"cost-state","totalCostUSD":2.5,"modelUsage":{"claude-opus-5-5":{"inputTokens":10,"outputTokens":20,"cacheReadInputTokens":300,"cacheCreationInputTokens":40,"costUSD":2.0},"claude-haiku-5-5":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4,"costUSD":0.5}}}"#;
        assert_eq!(last_cost_state(two_models), state(2.5, 11, 22, 303, 44));
    }

    /// T0 fixtures: (transcript, stream-json stdout) of the same run.
    const T0_PAIRS: &[(&str, &str, &str)] = &[
        (
            "permission-allow",
            include_str!("../fixtures/permission-allow-2.1.307.jsonl"),
            include_str!("../fixtures/stream-permission-allow-2.1.307.jsonl"),
        ),
        (
            "permission-deny",
            include_str!("../fixtures/permission-deny-2.1.307.jsonl"),
            include_str!("../fixtures/stream-permission-deny-2.1.307.jsonl"),
        ),
        (
            "permission-always",
            include_str!("../fixtures/permission-always-2.1.307.jsonl"),
            include_str!("../fixtures/stream-permission-always-2.1.307.jsonl"),
        ),
        (
            "ask-user-question",
            include_str!("../fixtures/ask-user-question-2.1.307.jsonl"),
            include_str!("../fixtures/stream-ask-user-question-2.1.307.jsonl"),
        ),
        (
            "mcp-timeout",
            include_str!("../fixtures/mcp-timeout-2.1.307.jsonl"),
            include_str!("../fixtures/stream-mcp-timeout-2.1.307.jsonl"),
        ),
    ];

    fn main_chain_uuids(records: &[Record]) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = records
            .iter()
            .filter_map(|r| match r {
                Record::User(m) | Record::Assistant(m) if !m.is_sidechain => Some(m.uuid),
                _ => None,
            })
            .collect();
        ids.sort();
        ids
    }

    /// T15 sets (2.1.308): a session started with `claude -p "<prompt>"` and taken over
    /// through `--resume` with the adapter's flags (`vscode-takeover`), and an adapter session
    /// continued by `claude -p --resume` from outside (`adapter-then-outside`). The stream
    /// is the adapter's run; the transcript holds both writers.
    const T15_SETS: &[(&str, &str, &str)] = &[
        (
            "vscode-takeover",
            include_str!("../fixtures/vscode-takeover-2.1.308.jsonl"),
            include_str!("../fixtures/stream-vscode-takeover-2.1.308.jsonl"),
        ),
        (
            "adapter-then-outside",
            include_str!("../fixtures/adapter-then-outside-2.1.308.jsonl"),
            include_str!("../fixtures/stream-adapter-then-outside-2.1.308.jsonl"),
        ),
    ];

    fn main_records(records: &[Record]) -> Vec<&Message> {
        records
            .iter()
            .filter_map(|r| match r {
                Record::User(m) | Record::Assistant(m) if m.is_main() => Some(m),
                _ => None,
            })
            .collect()
    }

    /// SPEC §9: `--resume` appends to the same file; both writers' records carry the same
    /// `cwd` and `sessionId`; the adapter's uuids are all in the transcript, the other
    /// writer's records are exactly the rest; no `ai-title` in either run.
    #[test]
    fn t15_two_writers_share_one_transcript() {
        for (name, transcript, stream) in T15_SETS {
            let (tr, bad_tr) = parse_str(transcript);
            let (st, bad_st) = parse_str(stream);
            assert_eq!((bad_tr, bad_st), (0, 0), "{name}");
            assert_eq!(
                first_cwd(transcript).as_deref(),
                Some(format!("/tmp/hqc-fixture/{name}").as_str())
            );
            let main = main_records(&tr);
            let sessions: std::collections::BTreeSet<_> =
                main.iter().map(|m| m.session_id).collect();
            assert_eq!(sessions.len(), 1, "{name}: one session id");
            let own: std::collections::HashSet<Uuid> =
                main_records(&st).iter().map(|m| m.uuid).collect();
            let foreign: Vec<&Message> = main
                .iter()
                .copied()
                .filter(|m| !own.contains(&m.uuid))
                .collect();
            assert!(
                own.iter().all(|u| main.iter().any(|m| m.uuid == *u)),
                "{name}"
            );
            match *name {
                // `claude -p` before the adapter: prompt, tool_use, tool_result, thinking, text.
                "vscode-takeover" => {
                    assert_eq!(foreign.len(), 5, "{name}");
                    assert_eq!(own.len(), 2, "{name}: echo of the prompt and the answer");
                    assert!(main[..5].iter().all(|m| !own.contains(&m.uuid)));
                }
                // The outside turn after the adapter's: prompt, thinking, text.
                "adapter-then-outside" => {
                    assert_eq!(foreign.len(), 3, "{name}");
                    assert_eq!(own.len(), 4, "{name}");
                    assert!(main[4..].iter().all(|m| !own.contains(&m.uuid)));
                }
                _ => unreachable!(),
            }
            // The last record of each writer's turn ends it.
            assert_eq!(main.last().unwrap().stop_reason(), Some("end_turn"));
            assert_eq!(last_ai_title(transcript), None, "{name}: ai-title");
        }
    }

    /// T25, 2.1.308 (`resume-compact`): a session, a `--resume` with the adapter's flags and
    /// one prompt (`stream-resume-prompt`), another `--resume` with `/compact`
    /// (`stream-resume-compact`). The prompt echo after `--resume` carries the transcript
    /// record's uuid. Of the `/compact` records the main chain gets only the command
    /// (`<command-name>/compact</command-name>`, not in stream-json) and its output (the echo);
    /// the summary is `isCompactSummary`, the caveat `isMeta`.
    #[test]
    fn t25_resume_prompt_and_compact() {
        let transcript = include_str!("../fixtures/resume-compact-2.1.308.jsonl");
        let (tr, bad) = parse_str(transcript);
        assert_eq!(bad, 0);
        let main = main_records(&tr);
        let uuids = |s: &str| -> Vec<Uuid> {
            main_records(&parse_str(s).0)
                .iter()
                .map(|m| m.uuid)
                .collect()
        };
        let prompt = uuids(include_str!(
            "../fixtures/stream-resume-prompt-2.1.308.jsonl"
        ));
        assert_eq!(prompt.len(), 2, "echo and answer");
        assert!(prompt.iter().all(|u| main.iter().any(|m| m.uuid == *u)));
        let echo = main.iter().find(|m| m.uuid == prompt[0]).unwrap();
        assert_eq!(echo.blocks()[0]["text"], "Reply with exactly: TWO");

        let compact = uuids(include_str!(
            "../fixtures/stream-resume-compact-2.1.308.jsonl"
        ));
        let after = main.iter().position(|m| m.uuid == prompt[1]).unwrap() + 1;
        let unknown: Vec<String> = main[after..]
            .iter()
            .filter(|m| !compact.contains(&m.uuid))
            .map(|m| m.blocks()[0]["text"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(unknown.len(), 1, "{unknown:?}");
        assert!(unknown[0].starts_with("<command-name>/compact</command-name>"));
        assert_eq!(main.len() - after, 2, "the command and its output");
    }

    /// T20, 2.1.308: `--resume --resume-session-at <uuid>` appends to the same file and hangs
    /// its prompt off that earlier record, which so gets a second child: a fork in one file.
    /// The adapter's turn (the stream) is the line the later prompt does not see.
    #[test]
    fn t20_fork_in_one_transcript() {
        let transcript = include_str!("../fixtures/fork-2.1.308.jsonl");
        let stream = include_str!("../fixtures/stream-fork-2.1.308.jsonl");
        let links: Vec<Link> = transcript.lines().filter_map(link).collect();
        assert!(links.iter().all(|l| !l.is_sidechain));
        let mut children: std::collections::HashMap<Uuid, Vec<Uuid>> = Default::default();
        for l in &links {
            if let Some(p) = l.parent_uuid {
                children.entry(p).or_default().push(l.uuid);
            }
        }
        let forks: Vec<_> = children.iter().filter(|(_, c)| c.len() > 1).collect();
        assert_eq!(forks.len(), 1, "one record with two children");
        let (at, _) = forks[0];
        let (tr, bad) = parse_str(transcript);
        assert_eq!(bad, 0);
        let at_rec = main_records(&tr)
            .into_iter()
            .find(|m| m.uuid == *at)
            .unwrap();
        assert_eq!(at_rec.blocks()[0]["text"], "ONE");
        let own: Vec<Uuid> = main_records(&parse_str(stream).0)
            .iter()
            .map(|m| m.uuid)
            .collect();
        let prompts: Vec<&Message> = main_records(&tr)
            .into_iter()
            .filter(|m| matches!(m.message["content"], serde_json::Value::String(_)))
            .collect();
        assert_eq!(prompts.len(), 3);
        // The adapter's prompt and the later one.
        assert!(own.contains(&prompts[1].uuid));
        assert!(!own.contains(&prompts[2].uuid));
        assert_eq!(prompts[2].parent_uuid, Some(*at));
    }

    #[test]
    fn meta_records_are_not_main() {
        let m = |extra: &str| match parse_line(&format!(
            r#"{{"type":"user","uuid":"{}","message":{{"content":"x"}}{extra}}}"#,
            Uuid::nil()
        ))
        .unwrap()
        {
            Record::User(m) => m,
            other => panic!("{other:?}"),
        };
        assert!(m("").is_main());
        assert!(!m(r#","isMeta":true"#).is_main());
        assert!(!m(r#","isSidechain":true"#).is_main());
        assert!(!m(r#","isCompactSummary":true"#).is_main());
        assert_eq!(m("").blocks()[0]["text"], "x");
    }

    /// The shapes of `queued_command` (2.1.318, the `origin` and `isMeta` of older versions):
    /// only a message a person typed is a queued prompt.
    #[test]
    fn queued_prompt_is_a_typed_message() {
        let a = |attachment: &str, extra: &str| {
            let line = format!(
                r#"{{"type":"attachment","uuid":"{}","timestamp":"2026-11-08T20:56:46.218Z","attachment":{attachment}{extra}}}"#,
                Uuid::nil()
            );
            match parse_line(&line).unwrap() {
                Record::Attachment(a) => a,
                other => panic!("{other:?}"),
            }
        };
        let human = r#"{"type":"queued_command","prompt":[{"type":"text","text":"also the docs"},{"type":"image","source":{}}],"commandMode":"prompt","origin":{"kind":"human"},"humanTurn":true}"#;
        let blocks = a(human, "").queued_prompt().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "also the docs");
        assert_eq!(a(human, "").uuid, Some(Uuid::nil()));
        let plain = r#"{"type":"queued_command","prompt":"also the docs","commandMode":"prompt"}"#;
        assert_eq!(
            a(plain, "").queued_prompt().unwrap(),
            [serde_json::json!({"type": "text", "text": "also the docs"})]
        );
        assert_eq!(a(human, r#","isSidechain":true"#).queued_prompt(), None);
        for other in [
            r#"{"type":"queued_command","prompt":"<cross-session-message>hi</cross-session-message>","commandMode":"prompt","origin":{"kind":"peer"},"isMeta":true}"#,
            r#"{"type":"queued_command","prompt":"[Cross-session delivery]","commandMode":"prompt","isMeta":true}"#,
            r#"{"type":"queued_command","prompt":"<task-notification></task-notification>","commandMode":"task-notification","origin":{"kind":"task-notification"}}"#,
            r#"{"type":"deferred_tools_delta","addedNames":[]}"#,
            r#"{"stripped":true}"#,
        ] {
            assert_eq!(a(other, "").queued_prompt(), None, "{other}");
        }
    }

    /// SPEC §8.1, §9.3: stream-json and the transcript share message uuids, and the
    /// `--replay-user-messages` echo carries the uuid of the transcript's prompt record.
    #[test]
    fn t0_stream_and_transcript_share_uuids() {
        for (name, transcript, stream) in T0_PAIRS {
            let (tr, bad_tr) = parse_str(transcript);
            let (st, bad_st) = parse_str(stream);
            assert_eq!((bad_tr, bad_st), (0, 0), "{name}: unparseable lines");
            assert_eq!(main_chain_uuids(&tr), main_chain_uuids(&st), "{name}");

            let echo: Vec<String> = stream
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .filter(|v| v["isReplay"] == true)
                .map(|v| v["uuid"].as_str().unwrap().to_owned())
                .collect();
            let prompt = tr.iter().find_map(|r| match r {
                Record::User(m) if m.message["content"].is_string() => Some(m.uuid.to_string()),
                _ => None,
            });
            assert_eq!(echo, vec![prompt.unwrap()], "{name}: replay echo uuid");
        }
    }
}
