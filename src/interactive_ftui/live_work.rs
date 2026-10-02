//! Live background work for the floating window (bd-b6bja): bash jobs
//! (`bash {background: true}`) and subagent delegations (`subagent` tool,
//! `/tan`) owned by the displayed session.
//!
//! Same lifecycle as the ask card: while anything runs, one row per item
//! lives in the floating window; when an item settles it leaves the window
//! and a compact record goes into the transcript; the window disappears when
//! empty. The card is display-only — it never owns a key.
//!
//! This module is the pure half: [`LiveWork`] diffs successive snapshots and
//! formats rows and records from values a test can construct. The registries
//! are read in one place, [`snapshot`], so the model never touches them
//! directly and every other path is deterministic.

use std::time::Duration;

use ftui::render::sanitize::sanitize;
use ftui::text::display_width;

use super::format_elapsed;

/// Where an item came from. Decides the row icon label and the record verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkKind {
    /// A background shell job from the `bash` tool (`jobs` registry).
    Job,
    /// A `subagent` tool delegation (agent hub).
    Subagent,
    /// A `/tan` background tangent (agent hub, `ChildKind::Tan`).
    Tan,
}

impl WorkKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::Subagent => "subagent",
            Self::Tan => "tan",
        }
    }
}

/// How a settled item ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub ok: bool,
    /// `exit 0`, `exit 101`, `killed`, `timed out`, `done`, `failed`, ...
    pub detail: String,
}

/// One unit of background work as the registries report it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkItem {
    /// Stable identity across snapshots: `job:<id>` / `agent:<id>`.
    pub key: String,
    pub kind: WorkKind,
    /// Short display id (`job-d9466b11…`, `scout-3`).
    pub id: String,
    /// One sanitized line: the command or the task head.
    pub label: String,
    /// Unix ms when it started; drives the elapsed column.
    pub started_ms: u64,
    /// Last line of output so far (jobs only), already sanitized.
    pub tail: Option<String>,
    /// `None` while running.
    pub outcome: Option<Outcome>,
}

/// A row of the card: the head line and an optional dim tail line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub head: String,
    pub tail: Option<String>,
}

/// Transcript record for an item that settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettledRecord {
    pub ok: bool,
    /// `job-d9466b11… cargo test --lib · 18m04s · exit 0` (the ✓/✗ mark
    /// is the role prefix's job).
    pub text: String,
}

/// Longest label the card shows before eliding; keeps the window from
/// claiming the whole screen for a long command line.
const MAX_LABEL_COLS: usize = 72;
/// Longest output tail line shown under a job row.
const MAX_TAIL_COLS: usize = 80;
/// How long after `/tan` (or any explicit kick) ticks keep flowing while the
/// child registers itself — the driver spawns it asynchronously, so the first
/// poll after the command can legitimately see nothing yet.
pub(crate) const EXPECT_GRACE: Duration = Duration::from_secs(5);

/// Tracker of what the card is showing. Only items observed live are ever
/// recorded: a registry that already holds settled jobs from before this UI
/// looked (a resumed session) must not replay them into the transcript.
#[derive(Debug, Default)]
pub(crate) struct LiveWork {
    live: Vec<WorkItem>,
    /// Keep ticks flowing until this instant even with nothing live yet.
    expect_until: Option<std::time::Instant>,
}

impl LiveWork {
    /// Whether anything needs the tick chain: a live item, or a recent kick.
    pub(crate) fn is_active(&self, now: std::time::Instant) -> bool {
        !self.live.is_empty() || self.expect_until.is_some_and(|until| until > now)
    }

    pub(crate) fn count(&self) -> usize {
        self.live.len()
    }

    /// Arm the grace window: something was just asked to start.
    pub(crate) fn expect(&mut self, now: std::time::Instant) {
        self.expect_until = Some(now + EXPECT_GRACE);
    }

    /// Forget everything (session switch). Nothing is recorded: the records
    /// belong to the session that owned the work.
    pub(crate) fn clear(&mut self) {
        self.live.clear();
        self.expect_until = None;
    }

    /// Absorb a fresh snapshot of the registries. Returns one record for
    /// each previously live item that has settled (or vanished), in the
    /// order they were first seen. New live items join the card; items that
    /// settled before the card ever showed them are ignored.
    pub(crate) fn apply(&mut self, snapshot: Vec<WorkItem>, now_ms: u64) -> Vec<SettledRecord> {
        let mut records = Vec::new();
        let mut still_live = Vec::with_capacity(self.live.len());
        for previous in self.live.drain(..) {
            match snapshot.iter().find(|item| item.key == previous.key) {
                Some(current) if current.outcome.is_none() => {
                    still_live.push(current.clone());
                }
                Some(settled) => records.push(settled_record(settled, now_ms)),
                None => records.push(settled_record(
                    &WorkItem {
                        outcome: Some(Outcome {
                            ok: false,
                            detail: String::from("gone"),
                        }),
                        ..previous
                    },
                    now_ms,
                )),
            }
        }
        for item in snapshot {
            if item.outcome.is_none() && !still_live.iter().any(|live| live.key == item.key) {
                still_live.push(item);
            }
        }
        still_live.sort_by_key(|item| item.started_ms);
        self.live = still_live;
        if !self.live.is_empty() {
            // The kick did its job; the items keep the chain alive now.
            self.expect_until = None;
        }
        records
    }

    /// The card rows, oldest first. `spin` is the shared spinner glyph for
    /// this frame so the rows animate with the status line.
    pub(crate) fn rows(&self, now_ms: u64, spin: &str) -> Vec<Row> {
        self.live
            .iter()
            .map(|item| {
                let elapsed = format_elapsed(Duration::from_millis(now_ms.saturating_sub(item.started_ms)));
                let head = format!(
                    "{spin} {} {} {elapsed:>6}  {}",
                    item.kind.label(),
                    item.id,
                    elide(&item.label, MAX_LABEL_COLS)
                );
                let tail = item
                    .tail
                    .as_deref()
                    .map(str::trim)
                    .filter(|tail| !tail.is_empty())
                    .map(|tail| format!("    {}", elide(tail, MAX_TAIL_COLS)));
                Row { head, tail }
            })
            .collect()
    }
}

fn settled_record(item: &WorkItem, now_ms: u64) -> SettledRecord {
    let outcome = item.outcome.as_ref();
    let elapsed = format_elapsed(Duration::from_millis(now_ms.saturating_sub(item.started_ms)));
    let detail = outcome.map_or("gone", |outcome| outcome.detail.as_str());
    SettledRecord {
        ok: outcome.is_some_and(|outcome| outcome.ok),
        text: format!(
            "{} {} {} · {elapsed} · {detail}",
            item.kind.label(),
            item.id,
            elide(&item.label, MAX_LABEL_COLS)
        ),
    }
}

/// Cut `text` to `max` columns with a trailing `…`.
fn elide(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = display_width(ch.encode_utf8(&mut [0; 4]));
        if width + w > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

/// Collapse a command or task to its first non-empty line, sanitized.
fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    sanitize(line).into_owned()
}

/// Short form of a background job id: `job-d9466b111c32…` → `job-d9466b11…`.
fn short_job_id(id: &str) -> String {
    const KEEP: usize = 12;
    if id.chars().count() <= KEEP {
        id.to_string()
    } else {
        let mut out: String = id.chars().take(KEEP).collect();
        out.push('…');
        out
    }
}

/// Convert a job registry snapshot into a card item.
pub(crate) fn job_item(job: &crate::jobs::JobSnapshot) -> WorkItem {
    let outcome = match job.status.as_str() {
        "running" => None,
        "exited" => Some(Outcome {
            ok: job.exit_code == Some(0),
            detail: job
                .exit_code
                .map_or_else(|| String::from("exited"), |code| format!("exit {code}")),
        }),
        "killed" => Some(Outcome {
            ok: false,
            detail: String::from("killed"),
        }),
        "timedOut" => Some(Outcome {
            ok: false,
            detail: String::from("timed out"),
        }),
        other => Some(Outcome {
            ok: false,
            detail: sanitize(other).into_owned(),
        }),
    };
    let tail = job
        .output_tail
        .lines()
        .rev()
        .map(str::trim_end)
        .find(|line| !line.trim().is_empty())
        .map(|line| sanitize(line).into_owned());
    WorkItem {
        key: format!("job:{}", job.id),
        kind: WorkKind::Job,
        id: short_job_id(&job.id),
        label: one_line(&job.command),
        started_ms: u64::try_from(job.started_at_ms).unwrap_or(0),
        tail,
        outcome,
    }
}

/// Convert an agent-hub roster entry into a card item.
pub(crate) fn child_item(child: &crate::agent_hub::ChildEntry) -> WorkItem {
    use crate::agent_hub::{ChildKind, ChildStatus};
    let kind = match child.kind {
        ChildKind::Subagent => WorkKind::Subagent,
        ChildKind::Tan => WorkKind::Tan,
    };
    let outcome = match child.status {
        ChildStatus::Starting | ChildStatus::Running => None,
        ChildStatus::Done => Some(Outcome {
            ok: true,
            detail: String::from("done"),
        }),
        settled => Some(Outcome {
            ok: false,
            detail: settled.as_str().to_string(),
        }),
    };
    WorkItem {
        key: format!("agent:{}", child.id),
        kind,
        id: sanitize(&child.id).into_owned(),
        label: one_line(&child.task),
        started_ms: child.started_ms,
        tail: None,
        outcome,
    }
}

/// Read both registries for `owner_session_id`. Jobs are session-owned;
/// the agent hub is one roster per parent process, which is this session.
pub(crate) fn snapshot(owner_session_id: Option<&str>) -> Vec<WorkItem> {
    let mut items = Vec::new();
    if let Some(owner) = owner_session_id
        && let Ok(jobs) = crate::jobs::list(owner)
    {
        items.extend(jobs.iter().map(job_item));
    }
    let roster = crate::agent_hub::registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .roster();
    items.extend(roster.iter().map(child_item));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn item(key: &str, kind: WorkKind, label: &str, started_ms: u64) -> WorkItem {
        WorkItem {
            key: key.to_string(),
            kind,
            id: key.rsplit(':').next().unwrap_or(key).to_string(),
            label: label.to_string(),
            started_ms,
            tail: None,
            outcome: None,
        }
    }

    fn settled(mut item: WorkItem, ok: bool, detail: &str) -> WorkItem {
        item.outcome = Some(Outcome {
            ok,
            detail: detail.to_string(),
        });
        item
    }

    #[test]
    fn new_live_items_join_and_settled_ones_leave_with_a_record() {
        let mut work = LiveWork::default();
        let job = item("job:job-1", WorkKind::Job, "cargo test --lib", 1_000);
        let agent = item("agent:scout-3", WorkKind::Subagent, "map the tick chain", 2_000);
        assert!(work.apply(vec![job.clone(), agent.clone()], 3_000).is_empty());
        assert_eq!(work.count(), 2);

        let records = work.apply(
            vec![settled(job.clone(), true, "exit 0"), agent.clone()],
            61_000,
        );
        assert_eq!(records.len(), 1);
        assert!(records[0].ok);
        assert_eq!(records[0].text, "job job-1 cargo test --lib · 1m00s · exit 0");
        assert_eq!(work.count(), 1);

        // A vanished item is recorded as gone, not silently dropped.
        let records = work.apply(Vec::new(), 65_000);
        assert_eq!(records.len(), 1);
        assert!(!records[0].ok);
        assert!(records[0].text.ends_with("· gone"), "{}", records[0].text);
        assert_eq!(work.count(), 0);
    }

    #[test]
    fn items_already_settled_on_first_sight_are_never_recorded() {
        let mut work = LiveWork::default();
        let old = settled(
            item("job:job-0", WorkKind::Job, "old job", 10),
            false,
            "exit 1",
        );
        assert!(work.apply(vec![old], 20).is_empty());
        assert_eq!(work.count(), 0);
    }

    #[test]
    fn rows_carry_spinner_kind_id_elapsed_label_and_tail() {
        let mut work = LiveWork::default();
        let mut job = item("job:job-1", WorkKind::Job, "cargo test --lib", 0);
        job.tail = Some(String::from("test foo ... ok"));
        work.apply(vec![job], 0);
        let rows = work.rows(2_500, "⠋");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].head, "⠋ job job-1   2.5s  cargo test --lib");
        assert_eq!(rows[0].tail.as_deref(), Some("    test foo ... ok"));
    }

    #[test]
    fn long_labels_are_elided_to_a_bounded_width() {
        let long = "x".repeat(200);
        let cut = elide(&long, MAX_LABEL_COLS);
        assert_eq!(display_width(&cut), MAX_LABEL_COLS);
        assert!(cut.ends_with('…'));
        assert_eq!(elide("short", 10), "short");
    }

    #[test]
    fn expectation_keeps_the_tracker_active_until_an_item_shows_up_or_time_passes() {
        let mut work = LiveWork::default();
        let now = Instant::now();
        assert!(!work.is_active(now));
        work.expect(now);
        assert!(work.is_active(now));
        assert!(!work.is_active(now + EXPECT_GRACE + Duration::from_millis(1)));
        work.apply(vec![item("agent:tan-1", WorkKind::Tan, "look into x", 0)], 0);
        assert!(work.expect_until.is_none(), "an item replaces the grace window");
        assert!(work.is_active(now + EXPECT_GRACE * 2));
        work.clear();
        assert!(!work.is_active(now));
    }

    #[test]
    fn job_snapshot_maps_status_exit_code_and_last_output_line() {
        let base = crate::jobs::JobSnapshot {
            schema: String::new(),
            id: String::from("job-d9466b111c32405e9c7eb56483ab0e46"),
            command: String::from("cargo test --locked\n--lib"),
            started_at_ms: 1_700_000_000_000,
            status: String::from("running"),
            exit_code: None,
            pid: Some(1),
            artifact_path: String::new(),
            artifact_cleanup: crate::jobs::ArtifactCleanupOutcome {
                policy: String::new(),
                removed_files: 0,
                reclaimed_bytes: 0,
            },
            output_tail: String::from("line one\nline two\n\n"),
            artifact_truncated: false,
            artifact_error: None,
            output_complete: false,
        };
        let running = job_item(&base);
        assert_eq!(running.key, "job:job-d9466b111c32405e9c7eb56483ab0e46");
        assert_eq!(running.id, "job-d9466b11…");
        assert_eq!(running.label, "cargo test --locked");
        assert_eq!(running.tail.as_deref(), Some("line two"));
        assert!(running.outcome.is_none());

        let mut exited = base.clone();
        exited.status = String::from("exited");
        exited.exit_code = Some(101);
        let outcome = job_item(&exited).outcome.expect("settled");
        assert!(!outcome.ok);
        assert_eq!(outcome.detail, "exit 101");

        let mut timed_out = base;
        timed_out.status = String::from("timedOut");
        assert_eq!(job_item(&timed_out).outcome.expect("settled").detail, "timed out");
    }
}
