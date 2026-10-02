# Handoff — rpi native "pretty" cards, freeze fix, Bedrock fixes (2026-09-30)

## STATUS UPDATE (session 12, 2026-10-02, iteration-budget handoff) — INVESTIGATION ONLY, NO CODE CHANGES

User report: "sessions eventually come to a hard lock instead of auto-handoff". Two `ask`/approval prompts
were dismissed, so the lock shape (screen frozen / agent spins / prompt dead after error) is UNCONFIRMED and
the recent session JSONLs under `~/.pi/agent-rust/sessions/--Users-bparafina-Projects-pi_agent_rust--/`
(restarts at 12:53, 12:56, 12:57, 13:00 local on 10-02 — the restart cadence itself smells like locks) were
NOT read. **Next agent, step 1:** get the user's answer to those two questions, or read those files
(`grep -o '"errorMessage":"[^"]\{0,140\}'`, count `Tool-iteration budget`, `"type":"compaction"`, last entry).

### What "auto-handoff" is today (no native rollover exists — `bd-s9oeu` is still OPEN)
- `src/agent.rs:3579-3600`: at `iterations >= 80%` of `max_tool_iterations` (default 50 → fires at 40) the
  run loop pushes `iteration_handoff_steering_text` into the steering queue once per run loop (per user prompt).
  `src/agent.rs:3601`: at `iterations > max` the turn ends with `StopReason::Error "Maximum tool iterations"`.
- FTUI: `run_controlled_turn` → `report_turn_result` (`src/interactive_ftui.rs:6983-7080`) sends
  `PiMsg::AgentError`; `AgentEnd` → `PiMsg::AgentDone` (6325). Nothing there re-prompts or rolls over.
- `plan-handoff.ts` (`~/.pi/agent-rust/extensions/`) is a different thing (plan→worker→review), not this.

### Findings so far (ranked by how well they explain "hard lock instead of handoff")
1. **Compaction never runs mid-run-loop.** `maybe_compact` (`agent.rs:14277`) is only called at prompt start
   (`run_agent_with_prompt_message` 16028, `run_agent_with_text` 16101, +16185). A 50-iteration autonomous run
   can blow past the window with zero compaction opportunity; the provider then returns a context-overflow
   400 (`error.rs:1008-1040 is_context_overflow`, classified `ProviderErrorKind::ContextOverflow`, NOT
   retryable, card says "compact the context (/compact) and resend"). Every subsequent prompt: `maybe_compact`
   starts a *background* compaction (two-phase; result applied only at the NEXT prompt, 14283) and the prompt
   itself still goes out with the oversized history → fails again. If the summary request also overflows,
   `attempt_count` climbs with a 60 s cooldown (`compaction_worker.rs:33-40`, 100 attempts) and
   `force_local_compaction_if_oversized` (14593) only rescues at **≥ 2× window**
   (`FORCED_LOCAL_COMPACTION_WINDOW_FACTOR`, `compaction.rs:1787`). A session parked between 1× and 2× is
   effectively dead = "prompt dead after error" lock. Was about to read `prepare_compaction`
   (`compaction.rs:1903`) to confirm whether `tokens_before` uses measured usage or the estimate.
   **Candidate fix:** on a `ContextOverflow` turn result, run a *synchronous* compaction (local fallback if
   the provider summary fails) and auto-resend once; and/or check `should_compact` at tool-iteration
   boundaries inside the run loop, not only at prompt start.
2. **80% steer skips the whole tool batch.** The steer is pushed at 3593, *before* `execute_tool_calls`;
   `execute_tool_calls` drains steering before the first effect batch (4993-4997) and `break`s, so every
   tool call the model issued at iteration 40 is replaced with a "Skipped due to steering" result
   (5053-5055). Functional but surprising — the model loses that turn's work. Not a lock, but worth
   pushing the steer *after* the batch (`steering_after_tools`) instead.
3. Frame-guard freeze (build 7 `allow_frame_skip: false`) — `tui.log` shows **0 `SkipFrame` lines on
   10-01/10-02**, so that fix is holding; the current lock is probably not the renderer.
4. Noise in `tui.log` 10-02: `Event handler error` from `maturity.ts:75`, `modelbar.ts:140`,
   `workset.ts:95/125` on every tool_result/turn_end, and periodic `setTimeout callback error: {}`. Unrelated
   to the lock but they fire on every turn; worth fixing in the extensions.

### Not touched
No source edits, nothing built, nothing installed (build 9 still live). `src/providers/bedrock/streaming.rs`
remains uncommitted and NOT mine. The session-11 queue (download.rs mkfifo, `cargo test --lib interactive_ftui`
on macOS, release build + live-check) is unchanged below.

---

## STATUS UPDATE (session 13, 2026-10-02, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`, 23 commits ahead of origin, not pushed. Build of `a72590ba4` installed as
`~/.local/bin/pi-rust` (12:45). Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs` (rustfmt-only).
User's chosen order for this and the next sessions: **(1) bd-b6bja jobs/subagent float card → (2) shimmer "Working…" +
cycling thinking words / "Thought for Ns" → (3) native colbar panels + read gutter.** Load average was ~100 all session.

### (1) bd-b6bja — live background-work float card: CODE COMPLETE, **NOT TYPE-CHECKED, NO SIM TEST YET**
Landed in this commit:
- NEW `src/interactive_ftui/live_work.rs` (pure half, 6 unit tests): `WorkItem{key,kind,id,label,started_ms,tail,outcome}`,
  `LiveWork::{apply,rows,is_active,expect,clear,count}`, `job_item(&JobSnapshot)`, `child_item(&agent_hub::ChildEntry)`,
  `snapshot(owner_session_id)` (reads `jobs::list` + `agent_hub::registry().roster()`). Only items *observed live* are ever
  recorded; vanished items record as `· gone`. Rows: `⠋ job job-d9466b11…   2.5s  cargo test --lib` + dim tail line.
  Deviation from the bead text: rows show **elapsed**, not a HH:MM start time (timezone-free; same clock as card `· 1.2s`).
- `src/interactive_ftui.rs`: fields `live_work`, `live_work_tracking` (off by default; launch path sets
  `.with_live_work_tracking(true)` so sim tests never see the process-global hub roster), `live_work_polled`,
  `live_work_tick_pending`; const `LIVE_WORK_POLL_INTERVAL = 500ms`; methods `poll_live_work(force)`, `absorb_live_work(snapshot)`
  (✓ → `EntryRole::Ask` `"✓ job … · 18m04s · exit 0"`, ✗ → `EntryRole::Error` whose prefix supplies the ✗), `live_work_window()`
  (title `background · N`, `wanted_cols` = widest row, no footer). Wiring: Tick condition now includes
  `live_work.is_active(now)` and calls `poll_live_work(false)`; `ToolEnd` for `bash|jobs|hub|subagent`, `AgentDone`,
  `SessionSystemNote`, `ConversationReset` (clears on session-id change) all `poll_live_work(true)`; `/tan` arms
  `live_work.expect(now)` (5 s grace) + `live_work_tick_pending`, consumed in `take_busy_tick`; `float_content` precedence
  picker > ask > ext > **live-work card** > floated slot; status chip `⟳ N` appended to the status line.
- No Esc/key handling on purpose — display-only; Esc keeps its existing meaning (interrupt turn / rewind).

**Next agent, step 1:** `cargo check --locked --lib --tests` (expect small fixes: possibly `format_elapsed` visibility from the
child module is fine since it's `super::`; `ChildStatus::as_str` exists; `JobSnapshot: Clone` yes). Then
`cargo test --locked --lib interactive_ftui` (247 were green at HEAD~1). Step 2: add a sim test next to
`ask_card_floats_and_leaves_a_compact_record_when_it_settles` (~line 12330): build `PiFtuiModel::new(rx)` (tracking off), call
`sim.model_mut().absorb_live_work(vec![live job item])` → frame contains `╭`, `background · 1`, the row, status `⟳ 1`;
inject `Event::Tick` → Cmd is a tick (chain alive while idle); absorb the settled snapshot → `EntryRole::Ask` record
`"✓ job job-1 cargo test … · exit 0"`, window gone, next Tick returns `Cmd::none()`. Step 3: release build in `/tmp/pi-release-wt`,
install (`cp … pi-rust.new && mv -f`), live-check: `bash {background:true, command:"sleep 20; echo hi"}` via a prompt, `/tan`,
and a `subagent` delegation; confirm zero ticks when idle with nothing live (`RUST_LOG=ftui_runtime=info`).
Then close bd-b6bja with the commit sha, `br sync --flush-only`.

### Then (2) shimmer + thinking words, (3) colbar panels + read gutter — design unchanged, see "Pi-rust sugar — revised design".

---

## STATUS UPDATE (session 11, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`. Not pushed. Build 9 still installed (nothing rebuilt).
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs` (rustfmt-only diff).

### Floating overlay for ask cards / ext prompts / pickers — WIRING COMPLETE, `cargo check --bin pi` CLEAN ✅
Everything from session 10's steps 1–4 landed in this commit (`src/interactive_ftui.rs`, `src/interactive_ftui/slots.rs`):
- `float_content(&slot_layout, width, height)` — precedence picker > ask card > ext card > `/col float` slot — drawn LAST in
  `render_frame`; the picker early-return + `render_picker` are gone (transcript stays visible under the window).
- `picker_float(picker, w, h)` (title row + windowed items + `PICKER_HINT` footer, `wanted_cols` = widest row) and free fns
  `picker_visible_rows(picker, w, h)` / `picker_wanted_cols` / `picker_kind_label`; PageUp/Down page by `picker_visible_rows`
  at `self.term`. **Renderer sizes from the frame, not `self.term`** — `ProgramSimulator::capture_frame` does not resize.
- `card_window(title, card)`: heading dim / question bold / options plain / last line (hints) → footer; `card_error` in red.
- Ext prompt paths (`activate_ext_request`, `submit_ext_answer`, `cancel_active_ext`) no longer push the card; they use
  `card_error` + `record_card(ext_card_heading(&request), outcome)`. Esc arms reordered: ask → ext → floated slot.
- `slots::float_window_rect` is `pub(crate)`; `FloatWindow::rect/content_rows/footer/wanted_cols` all have call sites now.
- New sim test `ask_card_floats_and_leaves_a_compact_record_when_it_settles` (after `ask_free_text_becomes_other_answer`):
  window border `╭` + question visible, transcript empty while open, `"red, purple"` → `! mixed known` in the window only,
  `"2"` → one `EntryRole::Ask` entry `"Pick a color?\n  → blue"`, window gone. Not yet type-checked (see below).
- Static check of existing tests at their frame sizes: `ask_card_collects_answers_across_questions` (50×12 → 40-col ×7-row
  window, "Pick a color?" fits), `extension_confirm_prompt_renders_and_reply_routes` (50×12, `[demo-ext] confirm: Deploy?`
  27 cols fits), `long_picker_scrolls_to_keep_the_selection_on_screen` (50×10 → 7-row window, 3 visible items; `(31/40)`,
  `!model-00` hold), `picker_supports_page_up_page_down` (page = 3 ≥ 1). No test asserts the old `"  → "`/`(dismissed)` strings.

### Bug 4 (macOS `cargo test --lib` won't build) — HALF FIXED in this commit
rustix 1.1.4 omits `mkfifoat`/`mknodat` on `apple`. `src/artifact_output.rs`: test now calls a new `make_fifo(path)` helper
(apple → `mkfifo -m 600 <path>` via `std::process::Command`; elsewhere → rustix as before).
**Still to do: `src/browser/download.rs:481`** — same pattern: replace the `rustix::fs::mkfifoat(rustix::fs::CWD, dir.path().join("fifo"), …)`
call with a copy of `make_fifo` in that test module (or hoist one helper to a `#[cfg(test)]` spot both can reach — e.g.
`crate::test_support` if it exists; grep `mod test_support`). Then `cargo test --locked --lib interactive_ftui` finally RUNS
on macOS: run `interactive_ftui::` and `interactive_ftui::slots::` tests and fix whatever the new float code gets wrong.

### Type-check in flight (NOT observed)
`cargo check --locked --lib --tests` → `~/.pi/agent-rust/tool-output-artifacts/jobs/job-d9466b111c32405e9c7eb56483ab0e46.log`
(ends with `EXIT n`). Expected: only `src/browser/download.rs:481 mkfifoat` remains, which still stops rustc before the
`lib test` target type-checks the new sim test. **Next agent, step 1:** read the log, fix download.rs as above, re-run.

### Then
Step 2: `cargo test --locked --lib interactive_ftui` on this Mac (first time ever) → fix failures. Step 3: release build in
`/tmp/pi-release-wt` (`git checkout <sha>`, `export PATH="$HOME/.cargo/bin:$PATH"`, `cargo build --locked --release --bin pi`),
install via `cp … ~/.local/bin/pi-rust.new && mv -f … pi-rust`; live-check the `ask` tool window, `/model`, `/resume`,
`/col float todos` + Esc, `/tmp/rpi-quit2.py`, `/tmp/rpi-probe4.py`. Then the unchanged queue: ttfx gate → shimmer
"Working…" → cycling thinking words / "Thought for Ns" → native colbar panels → read gutter.

---

## STATUS UPDATE (session 10, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`. Not pushed. Build 9 still installed (nothing rebuilt this session).
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs` (now a 9-line rustfmt-only diff).

### Floating overlay for ask cards / pickers — PRIMITIVE LANDED, WIRING HALF DONE (this commit)
**Type-check NOT observed.** `cargo check --locked --bin pi` was started in the background at the end of the budget:
`~/.pi/agent-rust/tool-output-artifacts/jobs/job-a634dd5ab13c47ef8194d9b68e602fc4.log` (ends with `EXIT n`).
**Next agent, step 1:** read that log and fix what it reports before anything else.

Done:
- `src/interactive_ftui/slots.rs`: new `pub(crate) struct FloatWindow { title, lines, footer: Option<Line>, wanted_cols: Option<u16> }`
  with `new/footer/wanted_cols/rect(w,h)/content_rows(w,h)/render(rect, border, frame)`; free fn
  `float_window_rect(width, height, content_rows, wanted_cols)` (`FLOAT_MAX_PERCENT = 70`; `wanted_cols` widens the
  60% band up to `width-2`). `SlotRegistry::float_rect` now calls it; `render_float` was REPLACED by
  `float_window(&PlacedSlot) -> Option<FloatWindow>`. Test `float_window_widens_for_long_content_and_reserves_the_footer_row`.
- `src/interactive_ftui.rs`: `render_frame` renders the floating slot via `self.slots.float_window(float)` + `window.render(..)`.
  Model field `card_error: Option<String>` (+ init). Ask card no longer goes into the transcript: `push_ask_card` deleted;
  activation sets `card_error = None`; `submit_ask_answer` stores parse errors in `card_error`, and on settle calls new
  `record_card(heading, outcome)` → one `EntryRole::Ask` entry `"{question}\n  → {answer|(dismissed)}"`. Esc path does the same.
  Doc comments on `ActiveAsk`/`PickerOverlay` updated.

**Remaining (in order) — the tree is in an intermediate state: the ask card is currently NOT RENDERED ANYWHERE
(removed from the transcript, not yet drawn in the window). Do not install a build until step 3 is done.**
1. Fix the type-check (likely: borrow of `question` vs `ask` in `submit_ask_answer` — `question` is `&ask.request…`;
   if rustc complains, clone `question.question` and `question.id` up front; unused-method warnings on
   `FloatWindow::footer/wanted_cols/content_rows` until the picker uses them).
2. `fn float_content(&self) -> Option<slots::FloatWindow>` on the model, precedence picker > ask card > ext card > floating slot:
   - ask: `crate::ask::format_question_card(q, index, total)` → `sanitize` → split `'\n'`; line 0 dim, line 1 bold, last line
     (the hint) as `footer` dim; append `card_error` in `palette.error`; title = `q.header.unwrap_or("ask")`.
   - ext: `format_extension_ui_prompt(&request)` the same way (line 0 is `[prov] method: title`); title = `request.method`.
   - picker: title line (`picker.title` + the `(sel/total)` / `filter:` status exactly as `render_picker` builds it now) as
     line 0, then the windowed items with `▸ ` marker; `footer = PICKER_HINT` dim; `wanted_cols = longest item + 2`.
     Visible rows = `float_window_rect(term.0, term.1, shown + 1 [+1 if "no matches"], cols).height - 2 - 1(footer) - 1(title)`;
     use the same number for the page size in `handle_picker_action` (replaces `self.body_height() - 1` at ~4444) — add
     `fn picker_visible_rows(&self) -> usize`.
3. `render_frame`: delete the early `if let Some(picker) … render_picker … return;` (~5827) so the transcript stays under
   the window; delete `render_picker`; at the end replace the slot-float block with
   `if let Some(window) = self.float_content(&slot_layout) { window.render(window.rect(area.width, area.height), accent, frame) }`.
   The `PiMsg::ToolEnd`/ext paths: `activate_ext_request` must stop pushing the card (`push_entry(EntryRole::Ask, …)` ~5372)
   and set `card_error = None`; `submit_ext_answer` Err arm → `card_error`, Ok arm → `record_card(first line of the card, raw|"(cancelled)")`;
   `cancel_active_ext` → `record_card(…, "(cancelled)")`. Esc on the floating slot (`5032`) must NOT fire while a picker/card
   owns the window (order the arms: picker handled earlier already; put the `slots.floating()` arm after the ask/ext arms).
4. Tests (can only run on the Linux lane — bug 4): existing `ask_card_collects_answers_across_questions` asserts
   `rendered.contains("Pick a color?")` at 50×12 → window is 40 cols × ≤8 rows, fits. `long_picker_scrolls_to_keep_the_selection_on_screen`
   at 50×10 → 7-row window: title + 3 items + hint; its `!contains("model-30")` / `(31/40)` assertions still hold.
   Grep the tests for `transcript` assertions on ask/ext card TEXT (e.g. `"  (dismissed)"`, `"  → "`) and update them to the
   `record_card` shape. Add one sim test: ask arrives → frame shows the question inside `╭─ask─` border AND the transcript
   has no card entry; after answering, transcript has `"Pick a color?\n  → blue"`.
5. Release build in the worktree (`/tmp/pi-release-wt`, `git checkout <sha>`, `export PATH="$HOME/.cargo/bin:$PATH"`,
   `cargo build --locked --release --bin pi`), install via `cp … pi-rust.new && mv -f`, then live: run the `ask` tool, `/model`,
   `/resume`, `/col float todos` + Esc.

### Then (unchanged order)
ttfx gate → shimmer "Working…" → cycling thinking words / "Thought for Ns" → native colbar panels (working set,
modelbar/devbar/flowbar) → read gutter. Step 2 of session 9 (hand live-check of `/col`, Esc, todo footer) is still open.

---

## STATUS UPDATE (session 9, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`. Not pushed. **Build 8 IS INSTALLED**
(`/tmp/pi-release-wt/target/release/pi` == `~/.local/bin/pi-rust`, byte-identical, worktree job `EXIT 0`).
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs`. No source edits this session.

### Probe run — FAILED, root cause identified (Bug 3 is only half-closed)
`python3 /tmp/rpi-extfail-probe.py` ×2: `ready=23.1s/9.6s skip_line_seen=None exited=28.8s/14.5s`. Session starts
fine (fail-open works), but the **"Extension zz-broken-probe failed to load and was skipped" line never renders.**
- Runtime side is CORRECT: `tui.log` 21:21:01Z `WARN … event="ext.load.skipped" extension_id=zz-broken-probe
  … error=Extension error: Error resolving modul…`, then `extension_runtime.shards.reload … shard_count=23 skipped=1`.
  (Not the Pattern-4 npm auto-stub: that needs `RepairMode::AutoStrict`; live default resolves to `suggest`.)
  Not a batch-overwrite either: `load_js_extensions` is called exactly once per startup (`src/agent.rs:15301`).
- TUI side is the bug — **ordering in the driver**, `src/interactive_ftui.rs:8961-8970`:
  `create_driver_session` → `report_extension_load_failures` sends `PiMsg::System(...)` (line 8754/8779), THEN
  `send_conversation_reset(&handle, &agent_tx, "pi interactive stack")` (line 8970) sends `PiMsg::ConversationReset`,
  whose handler `apply_conversation_reset` (line 5324) does `self.transcript.clear()` (line 5329). The skip line is
  pushed to the transcript and wiped ~ms later. pi-voice's "[pi-voice] Extension Pi Voice installed" line survives
  because it arrives later via `session_start` event dispatch.
- Not yet checked: whether the unit test for this (Bug 2/3 FTUI test) uses a simulator path that never sends
  `ConversationReset`, which is why it passed.

### Next agent, step 1 — fix: LANDED AND LIVE-VERIFIED ✅ (`5932daf53`, build 9 installed 18:56)
Option (a): `report_extension_load_failures(&handle, &agent_tx)` moved out of `create_driver_session` to the driver,
right after `send_conversation_reset(...)` (`src/interactive_ftui.rs` ~8971). Simulator test
`a_system_line_sent_before_the_startup_reset_is_wiped_and_one_after_survives` added next to
`a_background_note_arriving_mid_turn_waits_for_the_turn_boundary` (pins the before/after contract; statically
reviewed only — lib test target won't build on macOS, bug 4). `cargo check --locked --bin pi`: EXIT 0.
Release build in the worktree at `5932daf53` → `EXIT 0` → installed as `~/.local/bin/pi-rust` (build 9).
- `rpi-extfail-probe.py`: `ready=6.3s skip_line_seen=6.3s exited=10.9s`; transcript shows
  `· Extension zz-broken-probe failed to load and was skipped: Extension error: Error resolving module '@does-not-exist/anywhere' from …`
  **Bug 3 is now fully closed (runtime + UI).** Probe extension renamed to `zz-broken-probe.ts.disabled` (not deleted).
- `rpi-quit2.py`: `altscreen=1.2s ready=2.0s … shutdown took 0.59s`.
- `rpi-probe4.py`: 8/8 attempts ok (freeze detector clean).
Note: cargo in a background shell here resolved to `/opt/homebrew/bin/cargo` (stable) and failed on `-Z`;
prefix with `export PATH="$HOME/.cargo/bin:$PATH"`.

### Step 2 — still to live-check by hand (needs a human at the keyboard)
`/col`, `/col float todos`, Esc-unfloat, and the todo footer via the `todo` tool. Worktree `/tmp/pi-release-wt`
removal needs the user's OK (Rule 1). First Linux-lane / `dsr quality` run should execute the four new
simulator tests (three slot tests from session 8 + the reset-ordering test above).

### Original fix plan (kept for reference)
Pick one in `src/interactive_ftui.rs`:
(a) move `report_extension_load_failures(&handle, agent_tx)` out of `create_driver_session` (line 8754) to the driver
after `send_conversation_reset(...)` at line 8970 (simplest; keeps the line as a System transcript entry); or
(b) keep the call where it is and have `apply_conversation_reset` preserve `EntryRole::System` entries that were
pushed before the first reset (more invasive, touches a shared path). Prefer (a). Add a `ProgramSimulator` test:
send `PiMsg::System("Extension x failed to load and was skipped: …")` then `PiMsg::ConversationReset{..}` in the
driver's real order and assert the line is in the transcript (mirror the sims at 14107/14535/14796).
Then rebuild in the worktree (`cd /tmp/pi-release-wt && git checkout <new sha> && cargo build --locked --release
--bin pi`, ~40 min cold / faster warm), install, rerun `python3 /tmp/rpi-extfail-probe.py` →
expect `skip_line_seen=<n>s` and a `> Extension zz-broken-probe failed to load and was skipped: …` line.

### Step 2 — remaining live verify (unchanged)
`~/.pi/agent-rust/extensions/zz-broken-probe.ts` is STILL IN PLACE (needed for the re-probe; rename to `.disabled`
afterwards — do not delete). Then `/tmp/rpi-quit2.py`, `/tmp/rpi-probe4.py`; live-check `/col`, `/col float todos`,
Esc, todo footer. Worktree removal needs the user's OK (Rule 1).
Probe tip: with `RUST_LOG=...` set, startup logs still go to `~/.pi/agent-rust/logs/tui.log` (strip ANSI with
`sed 's/\x1b\[[0-9;]*m//g'` before grepping); only shutdown-time logs land on the pty.

### Then (unchanged order)
Floating overlay for ask cards/pickers on the same primitive → ttfx gate → shimmer "Working…" → cycling thinking
words / "Thought for Ns" → native colbar panels (working set, modelbar/devbar/flowbar) → read gutter.

---

## STATUS UPDATE (session 8, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`. Not pushed. Nothing installed since build 7.
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs`.

### Slot framework wiring — CODE COMPLETE (steps 5, 6, 8, 9 landed this commit)
- **Session 7's wiring type-checks.** Its `cargo check` had been SIGTERM'd unobserved; re-run this session reported
  exactly one error — `run_col_command` missing — which was the method I was adding at that moment. Nothing else in
  the layout/render/setWidget/❯-icon wiring needed fixing.
- Step 5 `/col`: `slots::ColCommand::parse(clean)` arm right after `/exit|/quit|/q` in `route_slash_command_tail`;
  `fn run_col_command` (after `route_slash_command_tail`'s end, ~4340) handles Next/Prev/Placement/Float/Select/List
  with System/Error lines. Added `SlotRegistry::focus_id(&str) -> bool` in `slots.rs`.
- Step 6 Esc: `Some(AppAction::Interrupt) if self.slots.floating().is_some()` arm BEFORE the `active_ask` arm.
- Step 8 todos: `agent_event_to_pi_msgs` `E::ToolExecutionEnd` now also emits `PiMsg::TodoSummary` (mirrors
  `src/interactive/agent.rs:556`) — the FTUI todo footer was dead before this. `PiMsg::ToolEnd` handler calls
  `publish_todos_slot(output)` for `todo` tool, non-error → `SlotSpec::native("todos", 100).ephemeral(true)`,
  blank lines dropped, `(todo list is empty)` clears.
- Step 9 tests (in `mod tests` after `set_widget_lands_in_a_slot_instead_of_the_transcript`):
  `col_float_and_escape_promote_and_dismiss_a_slot`, `registered_slot_takes_a_sidebar_when_wide_and_rows_below_when_narrow`,
  `todo_tool_end_feeds_the_todos_slot_and_footer`. Pure/simulator-based; run on the Linux lane (bug 4 on macOS).

### Next agent, step 1 — confirm the tree compiles
`cargo check --locked --bin pi` after all edits: **CLEAN (EXIT 0)**.
`cargo check --locked --all-targets --keep-going`: the ONLY errors in the whole tree are the two pre-existing macOS
`mkfifoat` ones (bug 4: `src/artifact_output.rs:424`, `src/browser/download.rs:481`), which stop rustc before the
`lib test` target reports anything else — so the three new simulator tests are **not type-checked on this machine**.
Every integration test / example / bench crate is clean. Static review of the tests against `slots.rs`: 160-wide →
`side_cols = 44`; 80×30 with `fixed_rows = 4` → budget 12, 3 rows used; `/col` reaches `route_slash_command_tail`
before extension dispatch. First Linux-lane / dsr run should confirm; likely-nothing-to-fix.
In flight: release build in the worktree at `818a23560`:
`~/.pi/agent-rust/tool-output-artifacts/jobs/job-d4383b0bc633432a84ebcdc73a374cff.log`, binary →
`/tmp/pi-release-wt/target/release/pi` (cold cache, ~40+ min, `nice -n 10`, log ends with `EXIT n`).

### Step 2 — release build + live verify (carried from sessions 6/7)
Once the worktree build above lands:
`cp /tmp/pi-release-wt/target/release/pi ~/.local/bin/pi-rust.new && mv -f ~/.local/bin/pi-rust.new ~/.local/bin/pi-rust`,
then `python3 /tmp/rpi-extfail-probe.py`, rename `~/.pi/agent-rust/extensions/zz-broken-probe.ts` → `.disabled`,
`/tmp/rpi-quit2.py`, `/tmp/rpi-probe4.py`. Live-check `/col`, `/col float todos`, Esc, and the todo footer with the
`todo` tool. The worktree removal needs the user's OK (Rule 1).

### Then (unchanged order)
Floating overlay for ask cards/pickers on the same primitive → ttfx gate → shimmer "Working…" → cycling thinking
words / "Thought for Ns" → native colbar panels (working set, modelbar/devbar/flowbar) → read gutter.

---

## STATUS UPDATE (session 7, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`. Not pushed. Nothing installed since build 7.
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs`.

### New bead
`bd-s9oeu` (P1 feature): **Automatic session resumption** — run the iteration-budget handoff natively
(final bounded turn writes a structured handoff Custom entry → successor session auto-created, linked, seeded
with handoff + summary → FTUI continues in place). Full spec in the bead description. This file's manual
"STATUS UPDATE" ritual is exactly what it replaces.

### Release build for live-verify (step 1 of session 6) — RE-STARTED, isolated
Session 6's release build never finished (no log, `target/release/pi` still Sep 30 19:37 = build 7). Re-run in a
**detached git worktree** so source edits here can't race it: `/tmp/pi-release-wt` (HEAD `1d06067a5`), job log
`~/.pi/agent-rust/tool-output-artifacts/jobs/job-410efc89e71f40b0bc1adcc51773cd08.log`, binary will land at
`/tmp/pi-release-wt/target/release/pi`. **Next agent, step 1:** when it exists:
`cp /tmp/pi-release-wt/target/release/pi ~/.local/bin/pi-rust.new && mv -f ~/.local/bin/pi-rust.new ~/.local/bin/pi-rust`
then `python3 /tmp/rpi-extfail-probe.py` (broken probe extension `~/.pi/agent-rust/extensions/zz-broken-probe.ts`
is still in place; rename to `.disabled` afterwards — don't delete). Then `/tmp/rpi-quit2.py`, `/tmp/rpi-probe4.py`.
The worktree can be removed later with `git worktree remove /tmp/pi-release-wt` — ask the user first (Rule 1).

### Slot framework wiring — PARTIAL, in `src/interactive_ftui.rs` (this commit)
Done (plan steps 1, 2, 3, 4, 7 of session 6):
- `mod slots;` declared; `slots: slots::SlotRegistry` field + init.
- `Regions` gained `sidebar` and `slots_below`; `layout_regions(area, input_rows, banner_rows, completion_rows,
  slot_rows, sidebar_cols)` — sidebar is carved off the right of `body` only (chrome rows keep full width).
- `render_frame`: computes `fixed_rows`, calls `self.slots.layout(...)`, renders sidebar stack after the body,
  below-stack after the status line, float LAST after the footer; `❯` prompt icon via `split_prompt_icon` +
  `prompt_icon_style()` (off→muted dim, medium→accent bold, high/xhigh→warning bold, else accent).
- `apply_extension_ui_effect`: `"setWidget" | "set_widget"` → `slots.set(SlotSpec::extension(id), lines)`;
  id from `widgetId|widget_id|widgetKey|widget_key|id|name` (default `"widget"`), lines from `lines[]` or `text`.
- Tests updated: `layout_reserves_the_completion_rows_above_the_editor` (new arity),
  `an_effect_this_stack_cannot_carry_out_still_prints` → renamed `set_widget_lands_in_a_slot_instead_of_the_transcript`.
- `cargo check --locked --bin pi` was started in the background (job
  `~/.pi/agent-rust/tool-output-artifacts/jobs/job-2c4c608eab2948ea91b5eaa566d6348e.log`) — **not observed**.
  **Next agent, step 2:** read that log; fix whatever it reports (likely candidates: `Rect::new` field math on
  `u16`, `Line::raw` taking `String`, `sanitize` returning `Cow`).

Remaining (steps 5, 6, 8, 9):
5. `/col` in `route_slash_command_tail` right after the `/exit|/quit|/q` arm:
   `if let Some(cmd) = slots::ColCommand::parse(clean) { self.run_col_command(cmd); return true; }` and write
   `fn run_col_command(&mut self, cmd: slots::ColCommand)`: Next/Prev → `step_focus` + System line naming the
   focused id; Placement(p) → `set_placement`; Float(id) (empty = `focused()`) → `float()` / push Error on Err;
   Select(id) → set focus (needs a small `focus_id(&str) -> bool` on `SlotRegistry`); List → System entry with
   `ids()` joined + `placement()`.
6. Esc: in the key handler, add BEFORE the `Interrupt if self.active_ask.is_some()` arm (~4873):
   `Some(AppAction::Interrupt) if self.slots.floating().is_some() => { self.slots.unfloat(); return Cmd::none(); }`.
8. Native `todos` producer. Finding: `agent_event_to_pi_msgs` (~5835) **never emits `PiMsg::TodoSummary`**, so
   the FTUI todo footer has been dead. Fix in `E::ToolExecutionEnd`: when `tool_name == "todo" && !is_error &&
   result.details.schema == crate::todo::TODO_LIST_SCHEMA`, also push `PiMsg::TodoSummary { summary: details.summary }`
   (mirror `src/interactive/agent.rs:556`). Then in the `PiMsg::ToolEnd` handler (~3115), when `name == "todo"` and
   not error, `self.slots.set(SlotSpec::native("todos", 100).ephemeral(true), lines)` from `output` split on `\n`
   (output is `list.render()`; `"(todo list is empty)"` → clear).
9. Tests: `render_frame` smoke via `ProgramSimulator` with a registered slot at 160×50 asserting a non-zero sidebar,
   and at 80×30 asserting `slots_below` rows; a `/col float k` + Esc test using `send_ui_effect("setWidget", …)`.

### Then (unchanged order)
Floating overlay for ask cards/pickers on the same primitive → ttfx gate → shimmer "Working…" → cycling thinking
words / "Thought for Ns" → native colbar panels (working set, modelbar/devbar/flowbar) → read gutter.

---

## STATUS UPDATE (session 6, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`, HEAD `5b49bcd48`. Not pushed. Nothing installed since build 7.
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs`.

### Bugs 1, 2, 3 — ALL CLOSED IN CODE ✅
- Bug 3 e2e: `ts_broken_extension_is_skipped_and_reported_by_default ... ok`,
  `ts_broken_extension_fails_the_load_when_fail_closed ... ok` (16 min build, 0.68 s run).
- Docs for `extensionPolicy.failClosedLoad` landed (`06bfa2e34`): `docs/extension-troubleshooting.md`,
  `docs/security/operator-handbook.md`.
- Only Bug 2's unit test remains unrun (bug 4, macOS `--lib`).

### Release build + live verify — IN FLIGHT
`cargo build --locked --release --bin pi` running since ~35 min (pid 38929, cold release cache after the serde_json
feature change; log `~/.pi/agent-rust/tool-output-artifacts/jobs/job-706527dcc3bc4e829880c1802095df96.log`).
**Next agent, step 1:** when it finishes: `cp target/release/pi ~/.local/bin/pi-rust.new && mv -f ~/.local/bin/pi-rust.new ~/.local/bin/pi-rust`
then `python3 /tmp/rpi-extfail-probe.py` (also at nothing else; it launches `rpi` in a pty and greps for the skip line).
A deliberately broken extension is ALREADY IN PLACE at `~/.pi/agent-rust/extensions/zz-broken-probe.ts` —
**⚠ until the new binary is installed, build 7 (fail-closed) will refuse to start a session with it present.**
Expected probe output: `skip_line_seen=<n>s` and a `> Extension zz-broken-probe failed to load and was skipped: …` line.
Afterwards rename the probe file to `zz-broken-probe.ts.disabled` (renaming, not deleting) or ask the user to remove it.
Also sanity-run `/tmp/rpi-quit2.py` (startup/shutdown timing) and `/tmp/rpi-probe4.py` (freeze detector).

### Pretty work, step 1 — slot framework: ENGINE WRITTEN, NOT WIRED
`src/interactive_ftui/slots.rs` (`5b49bcd48`) is complete with 6 unit tests but is **not yet declared** (`mod slots;`
missing in `interactive_ftui.rs` so the tree still compiles as before). Wiring plan, all in `src/interactive_ftui.rs`:
1. Add `mod slots;` next to `mod info_commands;` (~line 65). `cargo check` + run the slots unit tests
   (`cargo test --lib interactive_ftui::slots` won't build on macOS → `cargo check --all-targets` at least; the tests are
   pure and will run on the Linux lane).
2. Model: field `slots: slots::SlotRegistry` next to `ext_status` (~2044 in struct, init in `new()` ~2340).
3. `Regions` (~2211) gains `slots_below: Rect` and `sidebar: Rect`; `layout_regions` (~2316) takes `below_rows`/`side_cols`:
   add `Constraint::Fixed(below_rows)` between status and completion; after the vertical split, carve `side_cols` off the
   right of `body` into `sidebar`. In `render_frame` (~5635) compute
   `fixed_rows = 1 + banner + 1 + completion + input_rows + 1`, `let sl = self.slots.layout(w, h, fixed_rows)`, pass
   `sl.below_rows`/`sl.side_cols`, then after the status line: `self.slots.render_stack(&sl.below, regions.slots_below, frame)`,
   `self.slots.render_stack(&sl.side, regions.sidebar, frame)`, and LAST (after footer):
   `if let Some(f) = &sl.float { self.slots.render_float(f, Style::new().fg(self.palette.accent), frame) }`.
4. `apply_extension_ui_effect` (~4541): add `"setWidget" | "set_widget"` → id from `widgetId|id|name` (default `"widget"`),
   lines from `payload.lines: [str]` or `text` split on `\n` (sanitize each) → `self.slots.set(SlotSpec::extension(id), lines)`;
   empty clears. Update the test at ~12025 that asserts the printed fallback ("setWidget should still surface somehow")
   to assert the slot instead.
5. `/col` in `route_slash_command_tail` (~3742): `if let Some(cmd) = slots::ColCommand::parse(clean)` → Next/Prev →
   `step_focus(true/false)`; Placement → `set_placement`; Float(id) (empty id = focused) → `float()`, push Error on Err;
   Select(id) → focus; List → push a System entry listing `ids()` + placement. Also `ctrl+alt+←/→` → prev/next if the
   key path is cheap to find.
6. Esc: in the key handler, before other Esc handling, `if self.slots.unfloat() { return; }`.
7. `❯` prompt icon: in `render_frame`, when `self.input_active()`, draw `Paragraph` of `❯ ` in a 2-col rect at the left of
   `regions.input` and render the TextArea into `regions.input` shifted right by 2. Colour by
   `self.status_snapshot.thinking` (`off`→muted, `low`→accent, `medium`→accent bold, `high`/`xhigh`→warning), else accent.
8. Native producers (first two): `todo_summary` stays in the status line; additionally register a `todos` slot
   (`SlotSpec::native("todos", 100).ephemeral(true)`) when the todo tool publishes a multi-line list (find `todo_summary =`
   ~3124 and the PiMsg that carries it). The colbar extension (`~/.pi/agent-rust/extensions/colbar.ts`) publishes via
   `setWidget`, so step 4 alone gives it a surface.
9. Tests: a `render_frame` smoke test in the existing `mod tests` that registers a slot and asserts `layout_regions` heights;
   extend `tool_cards_show_exit_code_elapsed_grep_and_find_grouping`-style harness if one exists for frames.

### Then (unchanged order)
Floating overlay for ask cards/pickers on the same primitive → ttfx gate → shimmer "Working…" → cycling thinking
words / "Thought for Ns" → native colbar panels (working set, modelbar/devbar/flowbar) → read gutter.

---

## STATUS UPDATE (session 5, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`, HEAD `ac189af0d`. Not pushed. Nothing installed since build 7.
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs`.

### Bug 1 — CLOSED ✅ (session 4)
### Bug 2 — code type-checks ✅ (`cargo check --locked --bin pi` clean in 5m28s on the warm cache). Unit test
`recover_from_provider_quarantine_reloads_disk_and_clears_gate` still unrun (bug 4: `--lib` tests don't build on macOS).
### Bug 3 — CODE COMPLETE, TREE COMPILES ✅, e2e test run IN FLIGHT
Everything from the session-4 "remaining" list is landed in `ac189af0d`. The two e2e tests were started:
`cargo test --locked --test e2e_ts_extension_loading ts_broken_extension` (pid 65313, log
`~/.pi/agent-rust/tool-output-artifacts/jobs/job-df23c1131bb0407fa8f5eadfa0e7849d.log`, output only at the end).
**Next agent, step 1:** read that log / re-run the command.
- If `ts_broken_extension_is_skipped_and_reported_by_default` fails because the broken import does NOT error at load
  (the shim may lazily resolve bare specifiers), change `broken.ts` in `load_good_and_broken` to a hard syntax error
  (e.g. `export default function init(pi: any) { pi.registerCommand(`  — unterminated) so evaluation fails for sure.
- If the fail-closed test's `!manager.has_command("from-good")` fails, it means a prior partial install leaked; that
  would be a real bug in `load_js_extensions` (payloads are only installed after `?`, so it shouldn't).
**Step 2:** `cargo check --locked --all-targets --message-format short` to catch any other `ExtensionPolicy` /
`ExtensionPolicyConfig` struct literals I missed in tests (grep already covered `secret_broker:` and
`allow_dangerous:`; examples/ and benches/ were not checked).
**Step 3:** release build + install (`cp target/release/pi ~/.local/bin/pi-rust.new && mv -f … pi-rust`), re-add one
dropped package (e.g. `pi-btw`) to `~/.pi/agent-rust/settings.json` `packages`, launch `rpi`, confirm the session comes up
and a `Extension <id> failed to load and was skipped: …` System line appears. Also repro bug 2 if cheap.
**Step 4:** README/docs mention of `extension_policy.failClosedLoad` (one line next to `allowDangerous`).

### Bug 4 — untouched (macOS `cargo test --lib` / `mkfifoat`; `console_input.rs` rustfmt).

### Then the pretty work (unchanged order)
Prompt-area slot framework (+ `❯` icon by thinking level) → floating overlay/sidebar → ttfx gate → shimmer →
cycling thinking words → native colbar panels → read gutter. See "Pi-rust sugar — revised design" below.

---

## STATUS UPDATE (session 4, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`, HEAD `bc8414fd6`. Not pushed. Nothing installed since build 7.
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs`.

### Bug 1 — CLOSED ✅
`cargo test --locked --test session_conformance assistant_entry_serialization_is_stable` → **ok** (34 min full rebuild,
log `~/.pi/agent-rust/tool-output-artifacts/jobs/job-c17b87e835064990b262de8046bfc549.log`). The `float_roundtrip`
serde_json feature in `030b6344c` is the fix. Debug build cache is now warm.

### Bug 2 — committed `e24c0582f`, STILL NOT TYPE-CHECKED (see session 3 notes below)

### Bug 3 — WIP `bc8414fd6`, **TREE DOES NOT COMPILE** until these are done
Policy taken (ask timed out; went with the recommended option): skip-and-warn like TS pi, strict mode opt-in via
`ExtensionPolicy.fail_closed_load` (default false). Done in `src/extensions.rs`: policy field (+ all struct literals, incl.
`tests/phase3_security_invariants.rs`), `pub struct ExtensionLoadFailure {extension_id, entry_path, message}`,
`JsRuntimeShardSet.load_failures`, private `JsExtensionLoadReport {snapshots, failures}`, `LoadExtensions.reply` and
`load_extensions_snapshots` (~12690) return `Result<JsExtensionLoadReport>`, actor reply (~11900) fills it,
`build_js_runtime_shards` (~13720) wraps the per-extension load in an `async {}` block and records+skips failures
unless `policy.fail_closed_load || root_deadline <= Instant::now()`.

**Remaining, in order (next agent, step 1):**
1. `src/extensions/native_runtime.rs:672` `load_js_extensions_snapshots` → return `Result<JsExtensionLoadReport>`
   (the `NativeRust` arm stays an error). `JsExtensionLoadReport` is private to `extensions.rs`; make it `pub(crate)`
   or `pub(super)` as needed.
2. `src/extensions/extension_manager_impl.rs:3171`: `let report = runtime.load_js_extensions_snapshots(specs).await?;`
   then iterate `report.snapshots`; add `load_failures: Vec<ExtensionLoadFailure>` to `ExtensionManagerInner`
   (`src/extensions.rs:~20111`) and set `guard.load_failures = report.failures` in the same guard block (~3263).
   Add `pub fn load_failures(&self) -> Vec<ExtensionLoadFailure>` on `ExtensionManager` (mirror
   `cached_policy_prompt_decision` style at extension_manager_impl.rs:2964).
3. Config: `ExtensionPolicyConfig.fail_closed_load: Option<bool>` (`#[serde(alias = "failClosedLoad")]`, src/config.rs:293),
   merge at `merge_extension_policy` (config.rs:2254–2261), and in `resolve_extension_policy_with_metadata` after
   `allow_dangerous` (~1400): `policy.fail_closed_load = self.extension_policy.as_ref().and_then(|p| p.fail_closed_load).unwrap_or(false);`.
4. FTUI surfacing: `src/interactive_ftui.rs:8493` is the initial `create_agent_session(...).await`; on `Ok(handle)` call
   `handle.extension_manager()` (sdk.rs:2026) → `.load_failures()`; if non-empty send one `PiMsg::System` per failure:
   `"Extension <id> failed to load and was skipped: <message> (<entry_path>)"`. Do the same for `/new` and `/resume`
   replacement paths if cheap.
5. `cargo check --locked --bin pi` (bug 2 code gets checked at the same time), fix, then `cargo test --locked --lib
   extensions` won't build on macOS (bug 4) — rely on `cargo check --all-targets` for the test crates.
6. Add a test: an extension dir with one good and one syntactically broken `.ts`; `load_js_extensions` returns Ok, manager
   lists the good one, `load_failures()` has the broken one; with `fail_closed_load = true` it errors. Look for an
   existing JS load test to copy the harness (grep `load_js_extensions(` in src/extensions/*test*.rs).
7. Rebuild release, install via `cp target/release/pi ~/.local/bin/pi-rust.new && mv -f …`, then re-add one of the
   dropped packages from the rpi profile (e.g. `pi-btw`) to confirm the session comes up and the System line appears.

### Then the pretty work (unchanged order)
Prompt-area slot framework (+ `❯` icon by thinking level) → floating overlay/sidebar → ttfx gate → shimmer →
cycling thinking words → native colbar panels → read gutter. See "Pi-rust sugar — revised design" below.

---

## STATUS UPDATE (session 3, 2026-10-01, iteration-budget handoff)

Branch `fix/bedrock-tool-use-type-and-pijs-compat`, HEAD `e24c0582f` (on top of `030b6344c`). Not pushed.
Still uncommitted and NOT mine: `src/providers/bedrock/streaming.rs`. Nothing installed since build 7.

### Bug 1 — verification was IN FLIGHT when the budget ran out
`PATH=~/.cargo/bin:$PATH cargo test --locked --test session_conformance assistant_entry_serialization_is_stable`
was started in the background (pid 5161, log
`~/.pi/agent-rust/tool-output-artifacts/jobs/job-c17b87e835064990b262de8046bfc549.log`, output only appears
at the end because of `| tail -30`). **Next agent, step 1:** read that log (or re-run the command). Green ⇒
bug 1 closed (the `float_roundtrip` serde_json feature is already committed in `030b6344c`).

### Bug 2 — WRITTEN in `e24c0582f`, NOT YET TYPE-CHECKED (cargo lock was held by the bug-1 build)
- `src/agent.rs` right after `restore_retry_tail_with_admission`: `AgentSession::provider_quarantine_reason()`
  and `async recover_from_provider_quarantine(&mut self, cx) -> Result<bool>` (locks session, acquires admission
  permit, `Session::open(path)` when `save_enabled && path.is_some()`, header.id must match, keeps `session_dir`,
  `replace_messages(to_messages_for_current_path())`, `invalidate_background_compaction()`, `clear()`).
- `src/sdk.rs` `AgentSessionHandle`: thin `provider_quarantine_reason()` / `recover_from_provider_quarantine()`
  (next to `compact`).
- `src/interactive_ftui.rs` `run_controlled_turn`: `persistence_fault` flag → `recover_from_persistence_fault(handle, agent_tx)`
  posts `PiMsg::System("Session reloaded from disk after a persistence fault; the last turn was dropped — resend it.")`.
- Test `recover_from_provider_quarantine_reloads_disk_and_clears_gate` after
  `set_provider_model_quarantines_failed_persistence_without_runtime_mutation` (~20142). Compares messages via
  `serde_json::to_value` (Message has no PartialEq) and `session.path == disk.path` (tempdir may canonicalize).
**Next agent, step 2:** `PATH=~/.cargo/bin:$PATH cargo check --locked --bin pi` then fix whatever it reports
(likely candidates: borrow of `handle` after `turn.await` in `run_controlled_turn` — `control` is an owned clone so it
should be fine; `UserContent` import in the test module — it's already used at ~21131). `cargo test --lib` won't build on
macOS (bug 4), so the new unit test can only run via the Linux lane / dsr. Then rebuild release, install with the
`cp … pi-rust.new && mv -f` recipe, and reproduce: force a quarantine (e.g. `requestTimeoutSecs` low on Bedrock xhigh)
and confirm the next prompt works and the System line appears.

### Bug 3 — investigated, policy decision still open (ask the user)
`sdk::create_agent_session` → `agent_session.enable_extensions_with_policy(…).await?` (sdk.rs ~3085) →
`manager.load_js_extensions(js_specs).await?` (agent.rs ~15301). One bad extension fails the whole session; TS pi
logs and skips. Options: (a) skip-and-warn per extension with a startup System line listing failures (matches TS,
matches the rpi profile pain described below), (b) keep fail-closed but surface the failing extension name in the
FTUI instead of a dead "pi · ready" screen. Recommend (a) with a `extensions.failClosed` setting defaulting false.

### Bug 4 — untouched.

### After the bugs (unchanged order)
Prompt-area slot framework (+ `❯` icon by thinking level) → floating overlay/sidebar → ttfx gate → shimmer →
cycling thinking words → native colbar panels → read gutter. See "Pi-rust sugar — revised design" below.

---

## STATUS UPDATE (session 2, 2026-09-30, iteration-budget handoff)

Committed `030b6344c` on `fix/bedrock-tool-use-type-and-pijs-compat` (everything from the
"Uncommitted changes" section below except the other session's `src/providers/bedrock/streaming.rs`,
which is still uncommitted and NOT mine). Not pushed. Nothing installed since build 7.

### Bug 1 — ROOT-CAUSED AND FIXED (verification run in flight)
Not key order. `serde_json`'s default parser is **best-effort float precision**: the computed cost
`0.012705000000000001` on disk re-parsed as `0.012705`, so `prepare_jsonl_full_rewrite`'s byte
compare flagged an unchanged entry. Fix: `serde_json` feature `float_roundtrip` in `Cargo.toml`
(comment explains why). Regression test added at the end of `tests/session_conformance.rs`:
`assistant_entry_serialization_is_stable_across_disk_round_trip` — observed RED before the feature.
**Next agent, step 1:** `PATH=~/.cargo/bin:$PATH cargo test --locked --test session_conformance assistant_entry_serialization_is_stable`
(full rebuild ~several minutes because the feature change invalidates serde_json downstream; a
background run was started but not observed). If green, bug 1 is closed.

### Bug 2 — DESIGNED, NOT WRITTEN
The quarantine (`ProviderAdmissionGate` in `agent.rs` ~5966; `block/clear/reason`) is intentional:
in-memory vs disk may have diverged. Correct recovery = **reload from disk and clear the gate**.
Planned method on `AgentSession` right after `restore_retry_tail_with_admission` (~13494):

```rust
pub fn provider_quarantine_reason(&self) -> Option<String>   // self.provider_admission.reason()
pub async fn recover_from_provider_quarantine(&mut self, cx: &AgentCx) -> Result<bool>
```
Body: return Ok(false) if no reason; lock `self.session` (OwnedMutexGuard), then
`self.provider_admission.acquire(cx.cx())` permit; if `self.save_enabled && inner.path.is_some()` →
`Session::open(path_str)`, assert `header.id` matches, `reloaded.session_dir.clone_from(&inner.session_dir)`,
`*inner = reloaded`; then `self.invalidate_background_compaction()`,
`self.agent.replace_messages(inner.to_messages_for_current_path())`, `self.provider_admission.clear()`,
Ok(true). Expose on `sdk::AgentSessionHandle` (it has `session_mut()`; add a thin async wrapper).
Surface in FTUI: `report_turn_result` (`interactive_ftui.rs` ~6518) → in the `Err(err)` arm, if
`err.is_session_persistence()`, call the recovery on `handle` from `run_controlled_turn` and send
`PiMsg::System("Session reloaded from disk after a persistence fault; the last turn was dropped — resend it.")`.
Add a unit test next to `set_provider_model_quarantines_failed_persistence_without_runtime_mutation`
(~19969) that blocks the gate and asserts recovery clears it and messages == disk path.

### Bug 3 (extension load fail-closed) and Bug 4 (macOS `cargo test --lib` / rustfmt) — untouched.

### After the bugs (user's stated order)
Prompt-area framework → floating TUI → shimmer "Working…" → thinking words ("Thought for Ns") →
colbar components. See "Pass 2 roadmap" below for the pi-pretty references.

---

Branch: `fix/bedrock-tool-use-type-and-pijs-compat` (HEAD `5a1f22af2`). **Nothing committed yet.**
Installed binary: `~/.local/bin/pi-rust` = build 7 of this tree (`pi 0.6.1 (5a1f22af2 …)`), backup `~/.local/bin/pi-rust.release-0.6.1`.
Always install with `cp target/release/pi ~/.local/bin/pi-rust.new && mv -f … pi-rust` — an in-place `cp` invalidates the macOS code-signature cache (SIGKILL on exec) and clobbers a running session.

## Uncommitted changes (9 files; `git diff --stat`)

Mine (8):
- `Cargo.toml` — `ftui-extras` gains the `syntax` feature (ftui's own tokenizers; no syntect).
- `src/interactive_ftui.rs`
  - **Freeze fix**: `App::…with_budget(FrameBudgetConfig{ total: 33ms, allow_frame_skip: false })`. ftui-runtime 0.7.0's conformal frame guard cascades to `SkipFrame` after ONE slow frame (~25ms vs 16ms default) and, since skipped frames record no timing, never recovers → screen frozen, event loop idle in `kevent`, agent keeps working. Verified: 13/13 probe runs clean, guard still fires but skips 0 frames. Worth an upstream frankentui issue.
  - Native pi-pretty analog: `DetailBody` enum (`Plain|Diff|Code|Listing|Find|Grep`) selected by `TranscriptEntry::detail_body`; `read` bodies syntax-highlighted by extension; `ls` nerd-font icons (`file_icon`, gated by `terminal.nerdFontIcons`); `find` grouped by directory; `grep` per-file headers + `(?i)` pattern highlight (pattern recovered from the card head); bash `· exit N` (parsed off `Command exited with code N` trailer) and `· 1.2s` elapsed on every settled card; failed-card bodies in error color. Shared `Arc<SyntaxHighlighter>` also feeds `MarkdownRenderer::with_syntax_highlighter` → assistant code fences highlight.
  - Tests added (can't run locally: `cargo test --lib` doesn't build on macOS at HEAD — pre-existing `rustix::fs::mkfifoat` in `artifact_output.rs:424`, `browser/download.rs:481`): `read_card_body_is_syntax_highlighted_and_ls_card_gets_icons`, `tool_cards_show_exit_code_elapsed_grep_and_find_grouping`, `split_bash_exit_trailer_and_format_elapsed`.
- `src/config.rs`, `src/main.rs`, `tests/config_precedence.rs`, `tests/tui_state.rs` — `terminal.nerdFontIcons` (`TerminalSettings.nerd_font_icons`, default false) plumbed into `FtuiSettings`.
- `src/providers/bedrock.rs` — **Bedrock fix**: `build_request` folds consecutive same-role messages. Pi stores each tool result as its own `Message::ToolResult`, so parallel tool calls became consecutive `user` messages and Converse 400'd (`Expected toolResult blocks at messages.2.content`). Test `build_request_folds_parallel_tool_results_into_one_user_message`.
- `src/extensions_js.rs` — pi-tui shim exports `HStack`/`VStack` (stubs). `~/.pi/agent-rust/extensions/colbar.ts` imports `HStack`; a missing export fails the whole extension at load.

Not mine (leave alone): `src/providers/bedrock/streaming.rs` (another session's edit, 9 lines).

Gate used: `cargo check --locked --bin pi` clean (nightly-2026-08-31 via `~/.cargo/bin`). `dsr` is not installed on this machine. The repo is not rustfmt-clean at HEAD — never run bare `cargo fmt` (a worker did; 33 files reverted).

## rpi profile changes (`~/.pi/agent-rust/settings.json`, backup `settings.json.bak-1946`)
- `packages` pruned to `[npm:@earendil-works/pi-voice, git:github.com/apmantza/pi-lens]`. Dropped (fail to load in rpi's QuickJS → **session creation is fail-closed** → UI stuck at "pi · ready", no status bar, prompts dead, quit hangs): pi-mcp-adapter, pi-plan-mode, rpiv-todo (`@juicesharp/rpiv-config` unresolved), rpiv-ask-user-question, ponytail (JS private fields), pi-web-access, pi-fff, pi-btw, pi-subagents (OOMs the 256MB shard when combined; native `subagent` tool is already enabled by the `rpi` wrapper). Startup now 2.3s, shutdown 0.8s.
- `terminal.nerdFontIcons: true`.
- `requestTimeoutSecs: 300` — the body-stream **idle** timeout defaults to 60s for remote providers (`http/client.rs DEFAULT_REMOTE_REQUEST_TIMEOUT_SECS`); Bedrock + xhigh thinking pauses longer than that and the resulting `Request timed out reading body stream` is what kicks off the quarantine bug below.

## Open bugs (not fixed)
1. **Retry restoration quarantines the session on a false conflict.** `Agent::restore_retry_tail_with_admission` (`agent.rs` ~13433) → `candidate.save()` → `prepare_jsonl_full_rewrite` (`session.rs` ~1824) byte-compares re-parsed disk entries against in-memory ones and errored `session entry ID 37793397 has conflicting persisted and in-memory payloads` on a *completed, normal* assistant message (thinking + bash toolCall). Nothing mutates persisted entries in-process (`get_entry_mut` only used for plan Custom entries), so it's a serialize/round-trip instability — prime suspect: `serde_json` `preserve_order` is active via a transitive dep (lock shows `indexmap` under serde_json despite the Cargo.toml comment) so object key order can differ between the streamed form and the re-parsed form. Repro material: `~/.pi/agent-rust/sessions/--Users-bparafina-Projects-pi_agent_rust--/2026-10-01T01-10-56.738Z_10931da9.jsonl` line 70. Fix direction: compare `serde_json::Value`s (semantic), not bytes.
2. **Quarantine is a one-way door.** After (1), every provider call fails with `[SESSION_PERSISTENCE_FAILED] provider re-entry is quarantined…` until a new session. Needs a recovery path.
3. Extension load failure aborts session creation (`sdk::create_agent_session` fail-closed; TS pi skips the extension and continues). Policy question.
4. `cargo test --lib` doesn't compile on macOS (`mkfifoat`); `console_input.rs` isn't rustfmt-clean at HEAD.

## Pass 2 roadmap (pi-pretty parity, all native, `interactive_ftui.rs`)
Shimmer "Working…" sweep with rotating phrases + per-session OKLCH accent hue (djb2 of session name; pi-pretty `session-color.ts`, `working-indicator.ts`) → thinking "Thought for Ns" label reusing the sweep → `read` line-number gutter (`highlight_numbered`, needs the read `offset` on the card) → `❯` prompt icon colored by thinking level. Skip: inline images (needs out-of-band escape writes), FFF search, editor replacement.

## Pi-rust sugar — revised design (session 2, authoritative for the Pass 2 beads)

Decisions taken 2026-09-30: default effect = **single quiet sweep**; effects stay **confined to the
status line and floating window** (no tool-card settle animation). Everything lives in
`src/interactive_ftui.rs` (+ a focused submodule only where the bar is genuinely high), pure Rust,
reusing `DetailBody`, the shared `Arc<SyntaxHighlighter>`, `TerminalSettings`, and the 33 ms frame
budget. Extensions keep working; the five things below stop needing TS to exist.

### 1. Prompt area as a modular slot framework
```
PromptArea { above: Vec<Slot>, editor, status, below: Vec<Slot> }
Slot { id, order, min_rows, cap_rows, weight, ephemeral, side_ok, float_ok }
trait SlotProducer { fn render(&self, state: &AppState, width: u16) -> Vec<Line>; fn invalidate(&self); }
```
- **One registration path for all metadata extensions.** Native panels (todos, working set,
  model/dev/flow bars) and TS extension widgets (`ctx.ui.setWidget`, `__colbar.set`) both register a
  `SlotProducer`; the layout engine places them. Extensions never touch layout.
- **Placement tiers, re-evaluated every frame:** fullscreen ≥110 cols → right sidebar
  (`clamp(26, 28% width, 48)`); otherwise weighted columns below the editor; `ephemeral` slots are
  fillers that yield to heavy content. `/col [next|prev|none|hide|side|float <id>|<id>]` and
  `ctrl+alt+←/→` ported from `~/.pi/agent-rust/extensions/colbar.ts`.
- One `layout(width, height) -> Vec<Region>` pass per frame; regions render independently and cache
  by `(width, content_hash)` so the frame guard never sees them.
- **Transient floating window.** Any `float_ok` slot can be promoted to an overlay region drawn last
  (focus it twice, or `/col float <id>`); dismiss on Esc / blur / timeout. Ask cards and the session
  picker use the same overlay primitive — one z-layer, not three. This is the "context-bound panel"
  ask: a todo/task/PR flow gets a floating panel for the duration of the flow instead of permanently
  taking colbar space.
- `❯` prompt icon colored by thinking level (off/low/medium/high/xhigh → theme ramp), falling back
  to the per-session OKLCH hue — ships with this step since it's a status-slot detail.

### 2. Shimmer "Working…" indicator
- Per-session accent: `hue = djb2(session_name) % 360`, OKLCH `(L=0.78, C=0.12, hue)` → sRGB, with a
  24-bit → 256 → 16 colour fallback chain from terminal caps.
- Sweep: a 3-cell bright window moving across the phrase at ~12 Hz, driven by the **existing tick** —
  never its own timer — so it can't fight the frame guard.
- Rotating phrases from a built-in list (`terminal.workingPhrases` override), rotating every ~3 s,
  seeded by session hue so two panes don't sync.

### 3. Thinking words — cycling phrases
A seeded ring of phrases rotates every ~3 s while thinking streams; the shimmer sweep runs over the
current phrase. On settle the header collapses to `Thought for Ns` in dim text (same clock as the
`· 1.2s` card elapsed: first thinking delta → first text/tool delta). Expand/collapse of the thinking
body is the existing card toggle. Built-in list, overridable via `terminal.thinkingPhrases`.

### 4. ttfx integration (https://github.com/omacom/ttfx)
ttfx is an MIT Rust port of TerminalTextEffects. Candidate **effect engine** behind the
working/thinking line (`sweep`, `highlight`, `decrypt`, `colorshift`) and floating-window transitions
(`slide`/`expand` on open, `wipe` on close). Design: an `EffectDriver` trait
(`next_frame(dt) -> Option<Grid>`) with a ttfx-backed impl, ticked from the frame tick, auto-disabled
when the frame guard reports pressure or `terminal.effects = false`.

**Gate before adding the dependency** (suite rule against dependency smuggling): verify ttfx exposes
a `lib` target with a frame API yielding cells/spans rather than ANSI strings, confirm the
dependency-free claim, and measure binary-size impact against the 48 MiB budget. If there's no usable
lib API, vendor only the 3–4 needed effects as a focused module with the NOTICE preserved. The
default sweep must not require ttfx at all.

### 5. Colbar components (native panels)
Port the panels that depend on TS today: `todos` (built-in `todo` tool state — ephemeral filler),
`working set`, `modelbar`/`devbar`/`flowbar`-style status strips. Each is a `SlotProducer`.
Extension-published panels still arrive via `__colbar.set` → `setWidget`, now adopted by the native
layout instead of a TS HStack.

### Order
Bugs 1–3 → slot framework (+ prompt icon) → floating overlay/sidebar → ttfx gate → shimmer →
cycling thinking words → native colbar panels → read gutter. Beads carry the dependency edges.

## Probes (in /tmp, may not survive reboot)
`/tmp/rpi-probe4.py` (freeze detector: pty, prompt, sample on >4s silence), `/tmp/rpi-quit2.py` (time-to-ready + ctrl+c×2 shutdown time). Both exec `~/.local/bin/rpi` with `PI_PERF_TELEMETRY=1`; `RUST_LOG=ftui_runtime=info` shows the frame-guard decisions in `~/.pi/agent-rust/logs/tui.log`.
