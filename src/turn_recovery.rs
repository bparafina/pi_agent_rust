//! Turn recovery: unexpected-stop classification + auto-continue
//! (bd-cv653.3.15).
//!
//! A model that stops mid-task — token budget exhausted mid-code-block, or a
//! "I will now edit the files" promise with no follow-through — used to end
//! the turn silently, leaving the user to nudge by hand. At each turn end the
//! agent now runs a deterministic, zero-cost heuristic classifier over the
//! final assistant message; actionable classes inject one synthetic
//! continue-nudge user message (visible in the transcript, persisted with the
//! normal message flow) and let the turn loop run again. A hard cap of
//! [`MAX_AUTO_CONTINUATIONS`] per run prevents loops; transport errors and
//! provider failover stay entirely out of scope here (RetrySettings owns
//! them).

use serde::{Deserialize, Serialize};

use crate::model::StopReason;

/// Schema/marker tag carried in nudge messages and logs.
pub const TURN_RECOVERY_SCHEMA: &str = "pi.turn_recovery.v1";

/// Maximum auto-continuations per agent run — then the user decides.
pub const MAX_AUTO_CONTINUATIONS: u8 = 2;

/// How aggressively to auto-continue unexpected stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnRecoveryMode {
    /// Never auto-continue.
    Off,
    /// Auto-continue only provable interruptions: token-budget truncation
    /// and structurally unclosed output.
    #[default]
    Conservative,
    /// Also auto-continue semantic premature stops (announced-but-unstarted
    /// work).
    Aggressive,
}

/// What the heuristic classifier concluded about a finished turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryClass {
    CleanStop,
    /// The provider hit max_tokens (`StopReason::Length`).
    BudgetTruncated,
    /// Output ends inside an unclosed code fence or on a dangling list
    /// bullet — structurally cut off even though the stop looked clean.
    UnclosedStructure,
    /// The message announces imminent work ("I will now ...") and then
    /// stops without doing it.
    SemanticPrematureStop,
}

impl RecoveryClass {
    /// Human-readable reason used in the transcript marker.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::CleanStop => "clean stop",
            Self::BudgetTruncated => "response truncated by token budget",
            Self::UnclosedStructure => "response ended inside unfinished output",
            Self::SemanticPrematureStop => "announced work was not started",
        }
    }

    const fn actionable_in(self, mode: TurnRecoveryMode) -> bool {
        match self {
            Self::CleanStop => false,
            Self::BudgetTruncated | Self::UnclosedStructure => {
                !matches!(mode, TurnRecoveryMode::Off)
            }
            Self::SemanticPrematureStop => matches!(mode, TurnRecoveryMode::Aggressive),
        }
    }
}

/// Classify a finished turn from its stop reason and final text.
///
/// Only `Stop` and `Length` stops are examined: errors, aborts, refusals,
/// tool-use and pause-turn stops all have dedicated handling elsewhere.
#[must_use]
pub fn classify(stop_reason: StopReason, text: &str) -> RecoveryClass {
    match stop_reason {
        StopReason::Length => return RecoveryClass::BudgetTruncated,
        StopReason::Stop => {}
        _ => return RecoveryClass::CleanStop,
    }

    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return RecoveryClass::CleanStop;
    }

    if has_unclosed_fence(trimmed) || ends_on_dangling_bullet(trimmed) {
        return RecoveryClass::UnclosedStructure;
    }
    if ends_on_unfulfilled_promise(trimmed) {
        return RecoveryClass::SemanticPrematureStop;
    }
    RecoveryClass::CleanStop
}

/// Track top-level fenced code blocks, including the delimiter's type and
/// length. A fence may contain shorter fences or fences of the other type
/// as literal code; counting delimiter-looking lines misclassifies both
/// completed examples and genuinely truncated output.
fn has_unclosed_fence(text: &str) -> bool {
    let mut open_fence: Option<(u8, usize)> = None;
    for line in text.lines() {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        // Four spaces (or a leading tab) introduce indented code, not a
        // top-level fence. Only ASCII spaces were counted, so this slice
        // always starts on a UTF-8 boundary.
        if indent > 3 {
            continue;
        }
        let line = &line[indent..];
        let Some(marker @ (b'`' | b'~')) = line.as_bytes().first().copied() else {
            continue;
        };
        let length = line.bytes().take_while(|byte| *byte == marker).count();
        if length < 3 {
            continue;
        }
        let suffix = &line[length..];
        match open_fence {
            Some((open_marker, open_length))
                if marker == open_marker
                    && length >= open_length
                    && suffix.bytes().all(|byte| matches!(byte, b' ' | b'\t')) =>
            {
                open_fence = None;
            }
            // Backticks in a backtick fence's info string invalidate the
            // opener. Tilde fences do not have that restriction.
            None if marker != b'`' || !suffix.contains('`') => {
                open_fence = Some((marker, length));
            }
            _ => {}
        }
    }
    open_fence.is_some()
}

/// The final line is an empty list item. A bare ordered marker is only
/// actionable after another item in the same list: a complete numeric
/// answer such as "42." must not spend another model call or the recovery
/// budget. Prefer a false negative over inventing work from ambiguous text.
fn ends_on_dangling_bullet(text: &str) -> bool {
    let mut lines = text.lines().rev();
    let Some(last) = lines.next() else {
        return false;
    };
    let indent = last.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 || last[indent..].starts_with('\t') {
        return false;
    }
    let last = last.trim();
    if matches!(last, "-" | "*" | "+") {
        return true;
    }
    let Some((delimiter, remainder)) = ordered_bullet(last) else {
        return false;
    };
    if !remainder.is_empty() {
        return false;
    }
    let Some(previous) = lines.find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let previous_indent = previous.bytes().take_while(|byte| *byte == b' ').count();
    if previous_indent != indent || previous[previous_indent..].starts_with('\t') {
        return false;
    }
    ordered_bullet(previous.trim()).is_some_and(|(previous_delimiter, content)| {
        previous_delimiter == delimiter && !content.trim().is_empty()
    })
}

/// Recognize the one-to-nine-digit ordered-list markers supported by
/// Markdown. Nonempty item content must be separated by a space or tab.
fn ordered_bullet(line: &str) -> Option<(u8, &str)> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=9).contains(&digits) {
        return None;
    }
    let delimiter @ (b'.' | b')') = line.as_bytes().get(digits).copied()? else {
        return None;
    };
    let remainder = &line[digits + 1..];
    if remainder
        .as_bytes()
        .first()
        .copied()
        .is_some_and(|byte| !matches!(byte, b' ' | b'\t'))
    {
        return None;
    }
    Some((delimiter, remainder))
}

/// The message's closing sentence announces imminent action and nothing
/// follows it.
fn ends_on_unfulfilled_promise(text: &str) -> bool {
    let tail: String = text
        .chars()
        .rev()
        .take(240)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let tail_lower = tail.to_lowercase();
    let Some(position) = PROMISE_PHRASES
        .iter()
        .filter_map(|phrase| tail_lower.rfind(phrase))
        .max()
    else {
        return false;
    };
    // Nothing but one closing sentence may follow the announcement: if
    // another sentence terminator appears well before the end, the promise
    // was presumably followed through in text.
    let after = &tail_lower[position..]; // ubs:ignore position from rfind on this same string, always a valid boundary
    let interior = after.trim_end_matches(['.', ':', '!', '…', ' ', '\n']);
    !interior.contains(". ") && !interior.contains(":\n\n")
}

const PROMISE_PHRASES: &[&str] = &[
    "i will now",
    "i'll now",
    "let me now",
    "i am going to",
    "i'm going to",
    "next, i will",
    "next, i'll",
    "now i will",
    "now i'll",
    "proceeding to",
];

/// A recovery decision: the class plus the nudge to inject.
#[derive(Debug, Clone)]
pub struct RecoveryAction {
    pub class: RecoveryClass,
    pub nudge_text: String,
}

/// Per-run auto-continuation state (mode gating + hard cap).
#[derive(Debug)]
pub struct TurnRecoveryState {
    mode: TurnRecoveryMode,
    continuations: u8,
}

impl TurnRecoveryState {
    #[must_use]
    pub const fn new(mode: TurnRecoveryMode) -> Self {
        Self {
            mode,
            continuations: 0,
        }
    }

    /// Number of auto-continuations issued so far this run.
    #[must_use]
    pub const fn continuations(&self) -> u8 {
        self.continuations
    }

    /// Classify a finished turn and decide whether to auto-continue.
    ///
    /// Consumes one continuation from the cap when it returns `Some`.
    pub fn evaluate(&mut self, stop_reason: StopReason, text: &str) -> Option<RecoveryAction> {
        if matches!(self.mode, TurnRecoveryMode::Off) {
            return None;
        }
        let class = classify(stop_reason, text);
        if !class.actionable_in(self.mode) {
            return None;
        }
        // After a continuation the model may legitimately open or close
        // structures spanning message boundaries (finishing an earlier code
        // fence looks "unclosed" in isolation), so the text-shape heuristics
        // only apply to the first stop; re-continuation needs the provider's
        // own truncation signal.
        if self.continuations > 0 && !matches!(class, RecoveryClass::BudgetTruncated) {
            return None;
        }
        if self.continuations >= MAX_AUTO_CONTINUATIONS {
            tracing::info!(
                schema = TURN_RECOVERY_SCHEMA,
                class = ?class,
                cap = MAX_AUTO_CONTINUATIONS,
                "auto-continuation cap reached; leaving the stop to the user"
            );
            return None;
        }
        self.continuations += 1;
        tracing::info!(
            schema = TURN_RECOVERY_SCHEMA,
            class = ?class,
            continuation = self.continuations,
            "auto-continuing unexpected stop"
        );
        Some(RecoveryAction {
            class,
            nudge_text: format!(
                "[auto-continue {}/{}: {}] Continue from exactly where you stopped. \
                 Do not repeat content you already produced; finish the remaining work.",
                self.continuations,
                MAX_AUTO_CONTINUATIONS,
                class.reason()
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// Iteration-budget rollover (bd-s9oeu)
// ---------------------------------------------------------------------------
//
// The per-prompt tool-iteration cap used to be a hard wall: at 80% the agent
// was told to stop and write a handoff document, at 100% the turn ended with
// an error and a human had to say "keep going". Every roll-over cost the
// first turns of the next session re-reading that document. With rollover
// the cap becomes a checkpoint: the turn ends cleanly with a marker, the
// session compacts if it needs to (the same path a fresh prompt takes), and a
// generated continue message resumes the same task with a fresh budget — up
// to a hard per-prompt ceiling so a looping agent still stops.

/// Schema/marker tag carried in rollover messages and logs.
pub const ITERATION_ROLLOVER_SCHEMA: &str = "pi.iteration_rollover.v1";

/// Default ceiling on automatic rollovers per prompt (settings
/// `iterationRolloverMax`). 20 × the 50-iteration default budget is 1,000
/// tool iterations before a human has to weigh in.
pub const ITERATION_ROLLOVER_MAX_DEFAULT: u32 = 20;

/// What happens when a prompt exhausts its tool-iteration budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IterationRolloverMode {
    /// Legacy: end the turn with a `Maximum tool iterations exceeded` error
    /// and leave the next move to the user.
    Stop,
    /// Checkpoint and resume: the turn ends cleanly with a budget marker and
    /// the owning session re-prompts with a fresh budget (compacting first if
    /// the context calls for it), until [`ITERATION_ROLLOVER_MAX_DEFAULT`] or
    /// the configured ceiling is reached.
    #[default]
    Continue,
}

/// The one-shot steering text injected at 80% of the budget. In `Continue`
/// mode it asks for a safe checkpoint, not a handoff document: the run will
/// resume on its own.
#[must_use]
pub fn budget_warning_text(mode: IterationRolloverMode, current: usize, max: usize) -> String {
    match mode {
        IterationRolloverMode::Stop => format!(
            "[runtime] Tool-iteration budget at >=80% (used {current} of {max}). \
             Per the iteration-aware-handoff protocol in your spec, begin graceful \
             handoff now: commit current work, post a one-line status note, and \
             write an incomplete-handoff envelope with what's done / what remains \
             / next-agent starting position. Do NOT compress remaining work into \
             the last few iterations."
        ),
        IterationRolloverMode::Continue => format!(
            "[runtime] Tool-iteration budget at >=80% (used {current} of {max}). \
             The run rolls over automatically with a fresh budget when the cap is \
             reached; nothing is lost and no handoff document is needed. Reach a \
             safe checkpoint now — commit or record in-progress state where the \
             task tracks it — then keep working. Do NOT compress remaining work \
             into the last few iterations."
        ),
    }
}

/// The text block the agent appends to the assistant message that hit the
/// cap in `Continue` mode (the counterpart of the `--max-time` marker).
#[must_use]
pub fn budget_checkpoint_marker(max: usize) -> String {
    format!(
        "[iteration budget reached] {max} tool iterations used; checkpoint — the run \
         resumes with a fresh budget"
    )
}

/// The generated user message that resumes the task after a rollover.
/// Visible in the transcript and persisted like any user message, so a
/// replayed session shows exactly where each budget boundary fell.
#[must_use]
pub fn rollover_nudge_text(rollover: u32, max_rollovers: u32, budget: usize) -> String {
    format!(
        "[iteration rollover {rollover}/{max_rollovers}] The tool-iteration budget was reset \
         to {budget}. Continue the task from exactly where you stopped: re-issue any tool \
         calls that were pending, do not repeat work already done, and do not write \
         handoff documents — progress lives in the transcript and wherever the task \
         tracks it."
    )
}

/// The marker left when the rollover ceiling itself is reached: the next
/// move is the user's.
#[must_use]
pub fn rollover_ceiling_text(max_rollovers: u32, budget: usize) -> String {
    format!(
        "[iteration rollover ceiling] {max_rollovers} automatic rollovers of {budget} tool \
         iterations each have been used on this prompt; stopping for the user to decide."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_stop_is_budget_truncated() {
        assert_eq!(
            classify(StopReason::Length, "half a sentence"),
            RecoveryClass::BudgetTruncated
        );
    }

    #[test]
    fn clean_prose_is_clean() {
        assert_eq!(
            classify(StopReason::Stop, "All done. The tests pass."),
            RecoveryClass::CleanStop
        );
    }

    #[test]
    fn non_stop_reasons_never_classify() {
        for reason in [
            StopReason::ToolUse,
            StopReason::Error,
            StopReason::Aborted,
            StopReason::PauseTurn,
        ] {
            assert_eq!(
                classify(reason, "```\nunclosed"),
                RecoveryClass::CleanStop,
                "{reason:?}"
            );
        }
    }

    #[test]
    fn unclosed_fence_detected() {
        let text = "Here is the fix:\n```rust\nfn main() {\n    let x = 1;";
        assert_eq!(
            classify(StopReason::Stop, text),
            RecoveryClass::UnclosedStructure
        );
        let closed = "Here is the fix:\n```rust\nfn main() {}\n```\nDone.";
        assert_eq!(classify(StopReason::Stop, closed), RecoveryClass::CleanStop);
    }

    #[test]
    fn completed_fences_can_contain_literal_delimiters() {
        for text in [
            "````markdown\n```rust\nfn main() {}\n```\n````",
            "~~~text\n``` is literal code\n~~~",
            "```text\n~~~ is literal code\n```",
            "~~~rust\nlet x = 1;\n~~~~",
            "  ```text\npayload\n   ```\t",
            "~~~example with `backticks`\ncontent\n~~~",
            "```rust\nfn main() {}\n```\n~~~text\ndone\n~~~",
        ] {
            assert_eq!(
                classify(StopReason::Stop, text),
                RecoveryClass::CleanStop,
                "completed fence: {text:?}"
            );
        }
    }

    #[test]
    fn fence_closers_must_match_the_opener() {
        for text in [
            "~~~rust\nfn main() {",
            "````text\ncontent\n```",
            "~~~text\ncontent\n```",
            "```text\ncontent\n~~~",
            "```text\ncontent\n```not a closing fence",
            "```text\ncontent\n    ```",
            "~~~text\ncontent\n~~",
        ] {
            assert_eq!(
                classify(StopReason::Stop, text),
                RecoveryClass::UnclosedStructure,
                "unfinished fence: {text:?}"
            );
        }
    }

    #[test]
    fn literal_backticks_do_not_open_a_fence() {
        for text in [
            "    ```rust\n    literal indented code",
            "\t```rust\n\tliteral indented code",
            "```invalid`info\nordinary prose",
            "Inline ```code``` is complete.",
            "Unicode prose: 日本語, café, 🦀.",
        ] {
            assert_eq!(
                classify(StopReason::Stop, text),
                RecoveryClass::CleanStop,
                "not a fence: {text:?}"
            );
        }
    }

    #[test]
    fn dangling_bullet_detected() {
        let text = "Plan:\n1. read the file\n2.";
        assert_eq!(
            classify(StopReason::Stop, text),
            RecoveryClass::UnclosedStructure
        );
        let fine = "Plan:\n1. read the file\n2. edit it";
        assert_eq!(classify(StopReason::Stop, fine), RecoveryClass::CleanStop);
    }

    #[test]
    fn ordered_dangling_bullets_require_list_context() {
        for text in [
            "42.",
            "The answer is:\n42.",
            "1.",
            "Plan:\n1.read the file\n2.",
            "Plan:\n1. read the file\n2)",
            "Plan:\n1. read the file\n  2.",
            "Plan:\n1. read the file\n1234567890.",
            "    1. literal code\n    2.",
            "    -",
            "\t-",
        ] {
            assert_eq!(
                classify(StopReason::Stop, text),
                RecoveryClass::CleanStop,
                "ambiguous or literal marker: {text:?}"
            );
        }
        for text in [
            "1. read the file\n2.",
            "1) read the file\n2)",
            "1. read the file\n\n2.   \n",
            "  1. read the file\n  2.",
            "1.\tread the file\n2.",
            "Plan:\n-",
            "Plan:\n*",
            "Plan:\n+",
        ] {
            assert_eq!(
                classify(StopReason::Stop, text),
                RecoveryClass::UnclosedStructure,
                "unfinished list: {text:?}"
            );
        }
    }

    #[test]
    fn completed_markdown_does_not_spend_recovery_budget() {
        let mut state = TurnRecoveryState::new(TurnRecoveryMode::Conservative);
        for text in [
            "42.",
            "~~~markdown\n``` literal fence\n~~~",
            "    ``` literal indented code",
        ] {
            assert!(state.evaluate(StopReason::Stop, text).is_none(), "{text:?}");
        }
        assert_eq!(state.continuations(), 0);
        assert!(state.evaluate(StopReason::Length, "cut off").is_some());
        assert_eq!(state.continuations(), 1);
    }

    #[test]
    fn unfulfilled_promise_detected() {
        let text = "The bug is in parse(). I will now edit the three files.";
        assert_eq!(
            classify(StopReason::Stop, text),
            RecoveryClass::SemanticPrematureStop
        );
        let fulfilled =
            "I will now edit the file. Done — the change is applied and the test passes.";
        assert_eq!(
            classify(StopReason::Stop, fulfilled),
            RecoveryClass::CleanStop
        );
    }

    #[test]
    fn mode_gating_matrix() {
        let semantic = "I'll now update the config.";
        let budget_text = "cut off";

        let mut off = TurnRecoveryState::new(TurnRecoveryMode::Off);
        assert!(off.evaluate(StopReason::Length, budget_text).is_none());

        let mut conservative = TurnRecoveryState::new(TurnRecoveryMode::Conservative);
        assert!(
            conservative
                .evaluate(StopReason::Length, budget_text)
                .is_some(),
            "conservative handles budget truncation"
        );
        assert!(
            conservative.evaluate(StopReason::Stop, semantic).is_none(),
            "conservative ignores the semantic class"
        );

        let mut aggressive = TurnRecoveryState::new(TurnRecoveryMode::Aggressive);
        assert!(
            aggressive.evaluate(StopReason::Stop, semantic).is_some(),
            "aggressive handles the semantic class"
        );
    }

    #[test]
    fn cap_stops_after_two() {
        let mut state = TurnRecoveryState::new(TurnRecoveryMode::Conservative);
        assert!(state.evaluate(StopReason::Length, "a").is_some());
        assert!(state.evaluate(StopReason::Length, "b").is_some());
        assert!(
            state.evaluate(StopReason::Length, "c").is_none(),
            "third auto-continue must be refused"
        );
        assert_eq!(state.continuations(), 2);
    }

    #[test]
    fn clean_stops_do_not_consume_the_cap() {
        let mut state = TurnRecoveryState::new(TurnRecoveryMode::Conservative);
        for _ in 0..10 {
            assert!(state.evaluate(StopReason::Stop, "all done.").is_none());
        }
        assert_eq!(state.continuations(), 0);
    }

    #[test]
    fn structure_heuristics_only_apply_to_the_first_stop() {
        let mut state = TurnRecoveryState::new(TurnRecoveryMode::Conservative);
        assert!(
            state
                .evaluate(StopReason::Length, "```rust\nfn main() {")
                .is_some()
        );
        assert!(
            state
                .evaluate(StopReason::Stop, "}\n```\nAll done.")
                .is_none(),
            "a continuation closing an earlier fence must not re-trigger"
        );
        assert!(
            state
                .evaluate(StopReason::Length, "more truncation")
                .is_some(),
            "provider-signaled truncation still continues"
        );
    }

    #[test]
    fn nudge_text_carries_reason_and_counter() {
        let mut state = TurnRecoveryState::new(TurnRecoveryMode::Conservative);
        let action = state
            .evaluate(StopReason::Length, "partial")
            .expect("actionable");
        assert!(action.nudge_text.contains("auto-continue 1/2"));
        assert!(action.nudge_text.contains("token budget"));
    }

    #[test]
    fn iteration_rollover_mode_defaults_to_continue_and_parses_lowercase() {
        assert_eq!(
            IterationRolloverMode::default(),
            IterationRolloverMode::Continue
        );
        let parsed: IterationRolloverMode = serde_json::from_str("\"stop\"").expect("stop parses");
        assert_eq!(parsed, IterationRolloverMode::Stop);
        let parsed: IterationRolloverMode =
            serde_json::from_str("\"continue\"").expect("continue parses");
        assert_eq!(parsed, IterationRolloverMode::Continue);
        assert!(serde_json::from_str::<IterationRolloverMode>("\"auto\"").is_err());
    }

    #[test]
    fn budget_warning_keeps_the_shared_prefix_and_differs_by_mode() {
        let stop = budget_warning_text(IterationRolloverMode::Stop, 40, 50);
        let cont = budget_warning_text(IterationRolloverMode::Continue, 40, 50);
        for text in [&stop, &cont] {
            assert!(text.contains("Tool-iteration budget at >=80%"));
            assert!(text.contains("used 40 of 50"));
        }
        assert!(stop.contains("incomplete-handoff envelope"));
        assert!(cont.contains("rolls over automatically"));
        assert!(cont.contains("no handoff document"));
    }

    #[test]
    fn rollover_texts_carry_counters_and_budget() {
        assert!(budget_checkpoint_marker(50).contains("50 tool iterations used"));
        let nudge = rollover_nudge_text(3, 20, 50);
        assert!(nudge.contains("[iteration rollover 3/20]"));
        assert!(nudge.contains("reset to 50"));
        assert!(nudge.contains("re-issue any tool calls that were pending"));
        let ceiling = rollover_ceiling_text(20, 50);
        assert!(ceiling.contains("20 automatic rollovers"));
        assert!(ceiling.contains("stopping for the user"));
    }
}
