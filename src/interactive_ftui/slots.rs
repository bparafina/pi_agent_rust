//! Prompt-area slots: the one registration path for everything drawn around
//! the editor that is not the transcript.
//!
//! Before this, each panel around the prompt was its own field on the model
//! with its own row in `layout_regions`, and extension widgets
//! (`ctx.ui.setWidget`, the colbar `__colbar.set` bridge) had no surface at
//! all — they fell back to a line in the transcript. A todo panel, a working
//! set, a model/dev/flow strip and an extension's widget are all the same
//! thing to the layout: some lines with an id, an order and a size appetite.
//! This module owns that shape. Producers register content; the layout engine
//! decides where it goes, re-evaluated every frame from the current terminal
//! size:
//!
//! * **Side** — at ≥ [`SIDEBAR_MIN_WIDTH`] columns, slots that allow it stack
//!   in a right-hand column next to the transcript. The column is
//!   `clamp(26, 28% of width, 48)` wide.
//! * **Below** — otherwise (or for slots that refuse the side) they stack
//!   between the status line and the editor, inside a budget of at most
//!   [`BELOW_MAX_PERCENT`] of the screen height. Ephemeral slots are
//!   fillers: they yield first when the budget is tight.
//! * **Float** — one slot may be promoted to a bordered overlay drawn last
//!   over the transcript (`/col float <id>`); Esc dismisses it. Ask cards
//!   and the pickers will move onto the same primitive, so there is one
//!   z-layer rather than three.
//!
//! The layout pass is a pure function of `(slots, width, height)` so it is
//! cheap enough to run every frame and simple enough to test without a
//! terminal.

use ftui::core::geometry::Rect;
use ftui::text::{Line, Text};
use ftui::widgets::Widget;
use ftui::widgets::block::Block;
use ftui::widgets::borders::{BorderType, Borders};
use ftui::widgets::paragraph::Paragraph;
use ftui::{Frame, Style};

/// Narrowest terminal that gets a sidebar.
pub(crate) const SIDEBAR_MIN_WIDTH: u16 = 110;
/// Sidebar width bounds (columns); the middle term is 28% of the width.
const SIDEBAR_MIN_COLS: u16 = 26;
const SIDEBAR_MAX_COLS: u16 = 48;
/// Most of the screen height the below-editor stack may take (percent).
const BELOW_MAX_PERCENT: u32 = 40;
/// Floating window width bounds; the middle term is 60% of the width.
const FLOAT_MIN_COLS: u16 = 40;
const FLOAT_MAX_COLS: u16 = 100;

/// How a slot wants to be placed. Everything a producer may say about layout;
/// where the slot actually lands is the engine's call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotSpec {
    /// Stable identity; re-registering the same id replaces the content.
    pub id: String,
    /// Sort key within a stack. Lower is nearer the editor (below) or the top
    /// (side). Native panels use the hundreds; extension widgets default to
    /// 500 so they land after them unless they ask otherwise.
    pub order: i32,
    /// Rows the slot is useless under; it is dropped rather than squeezed.
    pub min_rows: u16,
    /// Rows the slot never needs more than (content beyond is cut).
    pub cap_rows: u16,
    /// A filler: yields to heavier content when the budget is tight.
    pub ephemeral: bool,
    /// May live in the sidebar.
    pub side_ok: bool,
    /// May be promoted to the floating window.
    pub float_ok: bool,
}

impl SlotSpec {
    /// A native panel with the usual appetite.
    pub(crate) fn native(id: impl Into<String>, order: i32) -> Self {
        Self {
            id: id.into(),
            order,
            min_rows: 1,
            cap_rows: 12,
            ephemeral: false,
            side_ok: true,
            float_ok: true,
        }
    }

    /// An extension widget: same shape, sorted after the native panels.
    pub(crate) fn extension(id: impl Into<String>) -> Self {
        Self::native(id, 500)
    }

    #[must_use]
    pub(crate) const fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    #[must_use]
    pub(crate) const fn cap_rows(mut self, cap_rows: u16) -> Self {
        self.cap_rows = cap_rows;
        self
    }
}

/// One registered slot: its spec and the lines it currently shows.
#[derive(Debug, Clone)]
pub(crate) struct Slot {
    pub spec: SlotSpec,
    pub lines: Vec<Line<'static>>,
}

impl Slot {
    /// Rows this slot would like: its content, capped.
    fn wanted_rows(&self) -> u16 {
        u16::try_from(self.lines.len())
            .unwrap_or(u16::MAX)
            .min(self.spec.cap_rows)
    }
}

/// Where the user asked the stack to go. `Auto` is the tiering above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Placement {
    #[default]
    Auto,
    /// Never use the sidebar.
    Below,
    /// Prefer the sidebar whenever the terminal is wide enough.
    Side,
    /// Draw no slots at all (the floating window still works).
    Hidden,
}

/// Every slot the surface currently knows about, plus the user's placement
/// choices. Owned by the model; mutated by producers, read by the renderer.
#[derive(Debug, Default)]
pub(crate) struct SlotRegistry {
    slots: Vec<Slot>,
    placement: Placement,
    /// The slot currently promoted to the floating window, by id.
    floating: Option<String>,
    /// Cursor for `/col next` / `/col prev`.
    focus: usize,
}

/// A placed slot: index into the registry and the rectangle it draws into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlacedSlot {
    pub index: usize,
    pub rect: Rect,
}

/// What the frame layout needs to know before it splits the screen, and what
/// the renderer needs afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SlotLayout {
    /// Columns to take off the right of the transcript for the sidebar.
    pub side_cols: u16,
    /// Rows to insert between the status line and the editor.
    pub below_rows: u16,
    /// Slots placed in the sidebar, rects relative to the sidebar origin.
    pub side: Vec<PlacedSlot>,
    /// Slots placed below, rects relative to the below-stack origin.
    pub below: Vec<PlacedSlot>,
    /// The floating slot, if any; rect is absolute (over the body).
    pub float: Option<PlacedSlot>,
}

impl SlotRegistry {
    /// Register or replace a slot. Empty content removes it, so a producer
    /// can push whatever it has without a separate clear step.
    pub(crate) fn set(&mut self, spec: SlotSpec, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            self.clear(&spec.id);
            return;
        }
        match self.slots.iter_mut().find(|slot| slot.spec.id == spec.id) {
            Some(slot) => {
                slot.spec = spec;
                slot.lines = lines;
            }
            None => self.slots.push(Slot { spec, lines }),
        }
        self.slots.sort_by_key(|slot| slot.spec.order);
    }

    pub(crate) fn clear(&mut self, id: &str) {
        self.slots.retain(|slot| slot.spec.id != id);
        if self.floating.as_deref() == Some(id) {
            self.floating = None;
        }
        self.focus = self.focus.min(self.slots.len().saturating_sub(1));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub(crate) fn slot(&self, index: usize) -> Option<&Slot> {
        self.slots.get(index)
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.slots.iter().map(|slot| slot.spec.id.as_str())
    }

    pub(crate) const fn placement(&self) -> Placement {
        self.placement
    }

    pub(crate) const fn set_placement(&mut self, placement: Placement) {
        self.placement = placement;
    }

    pub(crate) fn floating(&self) -> Option<&str> {
        self.floating.as_deref()
    }

    /// Promote a slot to the floating window. `Err` names the problem for the
    /// command's reply.
    pub(crate) fn float(&mut self, id: &str) -> Result<(), String> {
        let Some(slot) = self.slots.iter().find(|slot| slot.spec.id == id) else {
            return Err(format!("no slot named {id}"));
        };
        if !slot.spec.float_ok {
            return Err(format!("{id} cannot float"));
        }
        self.floating = Some(id.to_string());
        Ok(())
    }

    /// Close the floating window. Reports whether one was open, so an Esc
    /// handler knows if it consumed the key.
    pub(crate) fn unfloat(&mut self) -> bool {
        self.floating.take().is_some()
    }

    /// Move the `/col next|prev` cursor; returns the id now focused.
    pub(crate) fn step_focus(&mut self, forward: bool) -> Option<&str> {
        let len = self.slots.len();
        if len == 0 {
            return None;
        }
        self.focus = if forward {
            (self.focus + 1) % len
        } else {
            (self.focus + len - 1) % len
        };
        self.slots.get(self.focus).map(|slot| slot.spec.id.as_str())
    }

    pub(crate) fn focused(&self) -> Option<&str> {
        self.slots.get(self.focus).map(|slot| slot.spec.id.as_str())
    }

    /// Focus a slot by id for `/col <id>`. Reports whether it exists.
    pub(crate) fn focus_id(&mut self, id: &str) -> bool {
        match self.slots.iter().position(|slot| slot.spec.id == id) {
            Some(index) => {
                self.focus = index;
                true
            }
            None => false,
        }
    }

    /// Decide where every slot goes for a frame of `width × height`, given
    /// the rows the editor and the fixed chrome (header, status, footer,
    /// banner, completion) already take.
    pub(crate) fn layout(&self, width: u16, height: u16, fixed_rows: u16) -> SlotLayout {
        let mut layout = SlotLayout::default();
        if let Some(float) = self.float_rect(width, height) {
            layout.float = Some(float);
        }
        if self.placement == Placement::Hidden || self.slots.is_empty() {
            return layout;
        }

        let use_side = match self.placement {
            Placement::Auto | Placement::Side => {
                width >= SIDEBAR_MIN_WIDTH && self.slots.iter().any(|slot| slot.spec.side_ok)
            }
            Placement::Below | Placement::Hidden => false,
        };

        // Candidates for each stack. The floating slot is drawn in the window
        // instead, never twice.
        let mut side_candidates = Vec::new();
        let mut below_candidates = Vec::new();
        for (index, slot) in self.slots.iter().enumerate() {
            if self.floating.as_deref() == Some(slot.spec.id.as_str()) {
                continue;
            }
            if use_side && slot.spec.side_ok {
                side_candidates.push(index);
            } else {
                below_candidates.push(index);
            }
        }

        if !side_candidates.is_empty() {
            let side_cols = sidebar_cols(width);
            // The sidebar spans the transcript rows: everything but the chrome
            // and the editor.
            let side_height = height.saturating_sub(fixed_rows);
            layout.side_cols = side_cols;
            layout.side = self.stack(&side_candidates, side_cols, side_height);
            if layout.side.is_empty() {
                layout.side_cols = 0;
            }
        }

        if !below_candidates.is_empty() {
            let budget = percent_of(height, BELOW_MAX_PERCENT)
                .min(height.saturating_sub(fixed_rows).saturating_sub(3));
            let below_width = width.saturating_sub(layout.side_cols);
            layout.below = self.stack(&below_candidates, below_width, budget);
            layout.below_rows = layout.below.iter().map(|placed| placed.rect.height).sum();
        }
        layout
    }

    /// Stack `candidates` top-down into `budget` rows of `width` columns.
    /// Rects are relative to the stack origin.
    fn stack(&self, candidates: &[usize], width: u16, budget: u16) -> Vec<PlacedSlot> {
        let mut remaining = budget;
        let mut placed = Vec::new();
        // Heavy content first, then fillers: an ephemeral slot that would
        // have fit is still shown, but never at the expense of a real panel.
        let mut order: Vec<usize> = candidates.to_vec();
        order.sort_by_key(|&index| self.slots[index].spec.ephemeral);
        let mut heights = vec![0u16; candidates.len()];
        for &index in &order {
            let slot = &self.slots[index];
            let wanted = slot.wanted_rows();
            if wanted == 0 || remaining < slot.spec.min_rows {
                continue;
            }
            let rows = wanted.min(remaining);
            if rows < slot.spec.min_rows {
                continue;
            }
            let position = candidates
                .iter()
                .position(|&c| c == index)
                .expect("candidate index");
            heights[position] = rows;
            remaining -= rows;
        }
        let mut y = 0u16;
        for (position, &index) in candidates.iter().enumerate() {
            let rows = heights[position];
            if rows == 0 {
                continue;
            }
            placed.push(PlacedSlot {
                index,
                rect: Rect::new(0, y, width, rows),
            });
            y += rows;
        }
        placed
    }

    fn float_rect(&self, width: u16, height: u16) -> Option<PlacedSlot> {
        let id = self.floating.as_deref()?;
        let index = self.slots.iter().position(|slot| slot.spec.id == id)?;
        let slot = &self.slots[index];
        let cols = percent_of(width, 60)
            .clamp(FLOAT_MIN_COLS.min(width), FLOAT_MAX_COLS)
            .min(width);
        // Content plus the border, never more than 70% of the screen.
        let max_rows = percent_of(height, 70).max(3);
        let rows = (slot.wanted_rows() + 2).clamp(3, max_rows).min(height);
        // Bottom-right of the transcript, one row above the chrome at the
        // bottom so it reads as attached to the prompt area.
        let x = width.saturating_sub(cols).saturating_sub(1);
        let y = height.saturating_sub(rows).saturating_sub(4);
        Some(PlacedSlot {
            index,
            rect: Rect::new(x, y, cols, rows),
        })
    }

    /// Draw the slots of one stack at `origin`.
    pub(crate) fn render_stack(&self, placed: &[PlacedSlot], origin: Rect, frame: &mut Frame) {
        for item in placed {
            let Some(slot) = self.slots.get(item.index) else {
                continue;
            };
            let rect = Rect::new(
                origin.x + item.rect.x,
                origin.y + item.rect.y,
                item.rect.width.min(origin.width),
                item.rect
                    .height
                    .min(origin.height.saturating_sub(item.rect.y)),
            );
            if rect.height == 0 || rect.width == 0 {
                continue;
            }
            let lines = slot.lines.iter().take(usize::from(rect.height)).cloned();
            Paragraph::new(Text::from_lines(lines)).render(rect, frame);
        }
    }

    /// Draw the floating window: a rounded border titled with the slot id,
    /// content inside.
    pub(crate) fn render_float(&self, float: &PlacedSlot, border: Style, frame: &mut Frame) {
        let Some(slot) = self.slots.get(float.index) else {
            return;
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(slot.spec.id.as_str());
        let inner = block.inner(float.rect);
        // Clear what is under the window so transcript text cannot bleed
        // through gaps in short lines.
        let blank = std::iter::repeat_n(
            Line::raw(" ".repeat(usize::from(float.rect.width))),
            usize::from(float.rect.height),
        );
        Paragraph::new(Text::from_lines(blank)).render(float.rect, frame);
        block.render(float.rect, frame);
        let lines = slot.lines.iter().take(usize::from(inner.height)).cloned();
        Paragraph::new(Text::from_lines(lines)).render(inner, frame);
    }
}

/// Sidebar width for a terminal `width` columns wide.
fn sidebar_cols(width: u16) -> u16 {
    percent_of(width, 28).clamp(SIDEBAR_MIN_COLS, SIDEBAR_MAX_COLS)
}

/// `percent`% of `value`, rounded down, in integer arithmetic so the layout
/// is identical on every platform.
fn percent_of(value: u16, percent: u32) -> u16 {
    u16::try_from(u32::from(value) * percent / 100).unwrap_or(u16::MAX)
}

/// Parse `/col …` into an action. `None` means "not a /col command".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ColCommand {
    Next,
    Prev,
    Placement(Placement),
    Float(String),
    /// Focus a slot by id (also closes the float if it was that slot).
    Select(String),
    /// Bare `/col`: list slots and the current placement.
    List,
}

impl ColCommand {
    pub(crate) fn parse(input: &str) -> Option<Self> {
        let mut words = input.split_whitespace();
        let head = words.next()?;
        if !head.eq_ignore_ascii_case("/col") {
            return None;
        }
        let Some(verb) = words.next() else {
            return Some(Self::List);
        };
        let arg = words.next();
        Some(match verb.to_ascii_lowercase().as_str() {
            "next" => Self::Next,
            "prev" | "previous" => Self::Prev,
            "none" | "hide" | "hidden" => Self::Placement(Placement::Hidden),
            "side" | "sidebar" => Self::Placement(Placement::Side),
            "below" | "bottom" => Self::Placement(Placement::Below),
            "auto" | "show" => Self::Placement(Placement::Auto),
            "float" => Self::Float(arg.unwrap_or_default().to_string()),
            other => Self::Select(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::raw(format!("line {i}"))).collect()
    }

    #[test]
    fn wide_terminal_puts_side_ok_slots_in_a_sidebar() {
        let mut registry = SlotRegistry::default();
        registry.set(SlotSpec::native("todos", 100), lines(4));
        registry.set(SlotSpec::extension("colbar"), lines(2));
        let layout = registry.layout(160, 50, 5);
        assert_eq!(layout.side_cols, 44, "28% of 160, within [26, 48]");
        assert_eq!(layout.below_rows, 0);
        assert_eq!(layout.side.len(), 2);
        assert_eq!(layout.side[0].rect, Rect::new(0, 0, 44, 4));
        assert_eq!(layout.side[1].rect, Rect::new(0, 4, 44, 2));
    }

    #[test]
    fn narrow_terminal_stacks_below_within_budget_and_fillers_yield() {
        let mut registry = SlotRegistry::default();
        registry.set(SlotSpec::native("todos", 100).ephemeral(true), lines(10));
        registry.set(SlotSpec::native("working-set", 200), lines(6));
        // 30 rows → 40% budget is 12 rows; the real panel takes 6, the
        // filler gets the remaining 6 of its 10.
        let layout = registry.layout(90, 30, 5);
        assert_eq!(layout.side_cols, 0);
        assert_eq!(layout.below_rows, 12);
        assert_eq!(layout.below[0].rect.height, 6, "filler squeezed");
        assert_eq!(layout.below[1].rect.height, 6, "real panel whole");
        // Order on screen follows `order`, not the allocation order.
        assert_eq!(layout.below[0].index, 0);
    }

    #[test]
    fn placement_overrides_and_hidden_draws_nothing() {
        let mut registry = SlotRegistry::default();
        registry.set(SlotSpec::native("todos", 100), lines(3));
        registry.set_placement(Placement::Below);
        let layout = registry.layout(200, 50, 5);
        assert_eq!(layout.side_cols, 0);
        assert_eq!(layout.below_rows, 3);
        registry.set_placement(Placement::Hidden);
        let layout = registry.layout(200, 50, 5);
        assert_eq!(layout, SlotLayout::default());
    }

    #[test]
    fn float_promotes_one_slot_and_removes_it_from_the_stack() {
        let mut registry = SlotRegistry::default();
        registry.set(SlotSpec::native("todos", 100), lines(3));
        registry.set(SlotSpec::native("plan", 200), lines(5));
        registry.float("plan").expect("plan floats");
        let layout = registry.layout(120, 40, 5);
        let float = layout.float.expect("float placed");
        assert_eq!(float.index, 1);
        assert_eq!(float.rect.height, 7, "5 lines + border");
        assert_eq!(float.rect.width, 72, "60% of 120");
        assert_eq!(layout.side.len(), 1, "floating slot leaves the sidebar");
        assert!(registry.unfloat());
        assert!(!registry.unfloat());
        assert_eq!(
            registry.float("nope"),
            Err(String::from("no slot named nope"))
        );
    }

    #[test]
    fn empty_content_clears_and_min_rows_drops_rather_than_squeezes() {
        let mut registry = SlotRegistry::default();
        registry.set(SlotSpec::native("a", 1), lines(2));
        registry.set(SlotSpec::native("a", 1), Vec::new());
        assert!(registry.is_empty());
        let mut spec = SlotSpec::native("tall", 1);
        spec.min_rows = 5;
        registry.set(spec, lines(8));
        // 10 rows → 4-row budget, under min_rows: dropped entirely.
        let layout = registry.layout(80, 10, 5);
        assert!(layout.below.is_empty());
        assert_eq!(layout.below_rows, 0);
    }

    #[test]
    fn col_command_parses_every_verb() {
        assert_eq!(ColCommand::parse("/col"), Some(ColCommand::List));
        assert_eq!(ColCommand::parse("/COL next"), Some(ColCommand::Next));
        assert_eq!(ColCommand::parse("/col prev"), Some(ColCommand::Prev));
        assert_eq!(
            ColCommand::parse("/col hide"),
            Some(ColCommand::Placement(Placement::Hidden))
        );
        assert_eq!(
            ColCommand::parse("/col side"),
            Some(ColCommand::Placement(Placement::Side))
        );
        assert_eq!(
            ColCommand::parse("/col float todos"),
            Some(ColCommand::Float(String::from("todos")))
        );
        assert_eq!(
            ColCommand::parse("/col todos"),
            Some(ColCommand::Select(String::from("todos")))
        );
        assert_eq!(ColCommand::parse("/column"), None);
        assert_eq!(ColCommand::parse("hello"), None);
    }
}
