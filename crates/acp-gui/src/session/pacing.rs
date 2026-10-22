//! The agent's text in a live turn, shown at an even pace instead of in the bursts it
//! arrives in, and the events that came after it, which wait until it has all shown.
//!
//! Agents hand text over in pieces of very different sizes: two characters, then nothing
//! for a moment, then a whole sentence. Held here, it shows a few characters a frame at
//! about the rate it arrives, faster when it falls behind, so it trails what has arrived
//! by about `LAG`. Where it stops is chosen so that what shows renders as it will once
//! the rest comes: half of a `**bold**` run would show its asterisks, half of a link its
//! brackets and target, and a table's header row would show as a paragraph until its
//! delimiter row came.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use acp_core::SessionEvent;

/// How far the shown text may trail what has arrived; it speeds up to stay within it.
const LAG: Duration = Duration::from_millis(250);
/// How far it may trail once something waits behind it, such as the turn's end.
const CATCH_UP: Duration = Duration::from_millis(100);
/// How quickly its pace follows the pace it wants, so a burst speeds it up smoothly.
const RATE_EASE: Duration = Duration::from_millis(80);
/// The slowest pace, in characters a second, so the last few never crawl.
const MIN_RATE: f32 = 12.;
/// How far back the arrival pace is measured.
const ARRIVAL_WINDOW: Duration = Duration::from_secs(1);
/// How long it waits for a markdown run to close before showing the line as it is.
const WAIT_LIMIT: Duration = Duration::from_millis(800);

#[derive(Default)]
pub(crate) struct Pacer {
    /// The text streamed into the last row, shown or not, which the stopping rules read.
    row: String,
    /// How much of `row` shows; the rest is held back.
    shown: usize,
    /// Which lines of `row` are code, as far as the stopping rules have read it.
    fences: Fences,
    /// Events that came after the held text, landing once it has all shown.
    pub(crate) after: VecDeque<SessionEvent>,
    /// The held text opens a new row, a thought or not, which comes with its first piece
    /// so the row never shows empty.
    pub(crate) opens: Option<bool>,
    /// Characters that arrived within `ARRIVAL_WINDOW`, by when.
    arrivals: VecDeque<(Instant, usize)>,
    /// Characters a second it shows at.
    rate: f32,
    /// Characters it may show and hasn't, as frames don't fall on whole characters. A unit
    /// shown at once may take more than there is, which the next frames pay back.
    budget: f32,
    /// Where in the row's text it waits for more to arrive, and since when.
    wait: Option<(usize, Instant)>,
    /// The row's text up to here shows as it is: a run that didn't close in time.
    given_up_to: usize,
}

impl Pacer {
    /// Whether any text or event is held back.
    pub(crate) fn holds(&self) -> bool {
        self.has_text() || !self.after.is_empty()
    }

    pub(crate) fn has_text(&self) -> bool {
        self.shown < self.row.len()
    }

    /// The text arriving next opens a new row, a thought or not. Its pace starts from
    /// rest, not at the last row's, which may have been catching up.
    pub(crate) fn start_row(&mut self, thought: bool) {
        self.opens = Some(thought);
        self.row.clear();
        self.shown = 0;
        self.fences = Fences::default();
        self.arrivals.clear();
        self.rate = 0.;
        self.budget = 0.;
        self.wait = None;
        self.given_up_to = 0;
    }

    /// More of the row's text, arrived at `now`.
    pub(crate) fn arrived(&mut self, text: &str, now: Instant) {
        self.row.push_str(text);
        self.arrivals.push_back((now, text.chars().count()));
    }

    /// All of the held text, to show at once.
    pub(crate) fn take_all(&mut self) -> String {
        let text = self.row[self.shown..].to_string();
        self.shown = self.row.len();
        self.budget = 0.;
        text
    }

    /// The next piece of held text to show, `dt` after the last frame, if one is due.
    pub(crate) fn next(&mut self, dt: Duration, now: Instant) -> Option<String> {
        if !self.has_text() {
            self.budget = 0.;
            return None;
        }
        while self
            .arrivals
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > ARRIVAL_WINDOW)
        {
            self.arrivals.pop_front();
        }
        let arriving = self.arrivals.iter().map(|(_, chars)| *chars).sum::<usize>() as f32
            / ARRIVAL_WINDOW.as_secs_f32();
        let held = &self.row[self.shown..];
        let behind = held.chars().count() as f32;
        let lag = if self.after.is_empty() { LAG } else { CATCH_UP };
        let want = arriving.max(behind / lag.as_secs_f32()).max(MIN_RATE);
        let dt = dt.as_secs_f32();
        self.rate += (want - self.rate) * (1. - (-dt / RATE_EASE.as_secs_f32()).exp());
        self.budget += self.rate * dt;
        if self.budget < 1. {
            return None;
        }

        let mut end = held
            .char_indices()
            .nth(self.budget as usize)
            .map_or(held.len(), |(ix, _)| ix);
        // Latin text shows in whole words, as far as they have arrived.
        let bytes = held.as_bytes();
        while end < bytes.len() && is_word_byte(bytes[end - 1]) && is_word_byte(bytes[end]) {
            end += 1;
        }
        // Text is complete once something came after it: no more comes for this row.
        // (An update to a tool call already shown never waits here; see `Session::apply`.)
        let complete = !self.after.is_empty();
        let from = self.shown;
        // Taken out for `stop` to read while it keeps its place in it.
        let row = std::mem::take(&mut self.row);
        let stop = self.stop(&row, from, from + end, complete, now);
        self.row = row;
        if stop == from {
            // Waiting: don't save up a burst for when it ends.
            self.budget = self.budget.min(2.);
            return None;
        }
        let piece = self.row[from..stop].to_string();
        self.budget -= piece.chars().count() as f32;
        self.shown = stop;
        Some(piece)
    }

    /// Where the text from `from` up to about `want` may stop: at `want`, past it to the
    /// end of a unit that has to show at once, or before it where more text is needed.
    fn stop(&mut self, src: &str, from: usize, want: usize, complete: bool, now: Instant) -> usize {
        let mut p = from;
        while p < want {
            if p < self.given_up_to {
                p = next_char(src, p);
                continue;
            }
            self.fences = self.fences.read_to(src, p);
            match unit_at(src, p, complete, self.fences.open) {
                Unit::Char => p = next_char(src, p),
                Unit::Through(end) => p = end.max(next_char(src, p)),
                Unit::Wait => {
                    let since = match self.wait {
                        Some((at, since)) if at == p => since,
                        _ => {
                            self.wait = Some((p, now));
                            now
                        }
                    };
                    if now.duration_since(since) < WAIT_LIMIT {
                        break;
                    }
                    self.given_up_to = next_line(src, line_end(src, p));
                }
            }
        }
        p
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether the text read so far ends inside a fenced code block. Kept as the pacer reads
/// on, as finding out afresh means reading the row from its start for every character.
#[derive(Clone, Copy, Default)]
struct Fences {
    /// The start of the line it has read up to.
    line: usize,
    /// The lines before it leave a code block open.
    open: bool,
}

impl Fences {
    /// The same, read on to the line `p` is in: an odd number of fence lines ending
    /// before it leave it in a code block.
    fn read_to(mut self, src: &str, p: usize) -> Self {
        if p < self.line {
            self = Fences::default();
        }
        loop {
            let end = line_end(src, self.line);
            if end >= p {
                return self;
            }
            if src[self.line..end].starts_with("```") {
                self.open = !self.open;
            }
            self.line = end + 1;
        }
    }
}

/// What has to show together from a point in the text so that what shows renders as it
/// will once more arrives.
#[derive(Debug, PartialEq)]
enum Unit {
    /// One character.
    Char,
    /// Everything up to this offset.
    Through(usize),
    /// Nothing until more text arrives.
    Wait,
}

/// The unit starting at `p` in a row's markdown `src`. `complete` says no more text comes
/// for the row, so nothing waits; `fenced` that `p` is inside a fenced code block.
fn unit_at(src: &str, p: usize, complete: bool, fenced: bool) -> Unit {
    let end = line_end(src, p);
    let line_done = end < src.len() || complete;
    let line = &src[p..end];
    let at_start = p == 0 || src.as_bytes()[p - 1] == b'\n';
    let whole_line = || {
        if line_done {
            Unit::Through(next_line(src, end))
        } else {
            Unit::Wait
        }
    };

    if fenced {
        // Only a closing fence matters inside a code block; it shows whole.
        return match () {
            _ if at_start && line.starts_with("```") => whole_line(),
            _ if at_start && line.starts_with('`') && !line_done => Unit::Wait,
            _ => Unit::Char,
        };
    }
    if at_start {
        // An opening fence shows whole, so its language never shows half-typed.
        if line.starts_with("```") {
            return whole_line();
        }
        if line.starts_with('`') && !line_done && line.len() < 3 {
            return Unit::Wait;
        }
        if line.starts_with('|') {
            // A body row types into its cells; a header row shows with its delimiter
            // row, already as a table.
            if previous_line(src, p).starts_with('|') {
                return Unit::Char;
            }
            if !line_done {
                return Unit::Wait;
            }
            let next_start = next_line(src, end);
            let next_end = line_end(src, next_start);
            if next_end == src.len() && !complete {
                return Unit::Wait;
            }
            return if is_delimiter_row(&src[next_start..next_end], cells(line).len()) {
                Unit::Through(next_line(src, next_end))
            } else {
                Unit::Char
            };
        }
        // A heading or list marker shows with the first unit of its text, so the block
        // takes its look at once.
        if let Some(marker) = block_marker(line) {
            let first = p + marker;
            return match unit_at(src, first, complete, false) {
                Unit::Wait => Unit::Wait,
                Unit::Char => Unit::Through(next_char(src, first)),
                through => through,
            };
        }
        if !line_done && could_be_marker(line) {
            return Unit::Wait;
        }
    }
    let rest = &src[p..];
    let waiting = if line_done { Unit::Char } else { Unit::Wait };
    // A link or an image shows whole once its target closes on its line.
    if rest.starts_with('[') || rest.starts_with("![") {
        let label = p + rest.find('[').unwrap_or(0) + 1;
        let Some(close) = src[label..end].find(']') else {
            return waiting;
        };
        let after = label + close + 1;
        return match src[after..end].chars().next() {
            Some('(') => src[after..end]
                .find(')')
                .map_or(waiting, |target| Unit::Through(after + target + 1)),
            // Brackets with no target after them are text.
            Some(_) => Unit::Char,
            None => waiting,
        };
    }
    // An inline run shows whole once it closes on its line.
    for mark in ["**", "__", "~~", "`", "*", "_"] {
        if !rest.starts_with(mark) {
            continue;
        }
        // Underscores inside a word, as in `snake_case`, are text.
        let before = src[..p].chars().next_back();
        if mark.starts_with('_') && before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return Unit::Char;
        }
        let inner = p + mark.len();
        // A single `*` or `_` opens a run only right before text; one at the end of what
        // has arrived may still be half of `**`.
        if mark.len() == 1 && mark != "`" {
            match src[inner..end].chars().next() {
                None => return waiting,
                Some(c) if c.is_whitespace() => return Unit::Char,
                Some(_) => {}
            }
        }
        return match src[inner..end].find(mark) {
            Some(close) if close > 0 => Unit::Through(inner + close + mark.len()),
            _ => waiting,
        };
    }
    Unit::Char
}

fn next_char(src: &str, p: usize) -> usize {
    src[p..].chars().next().map_or(p, |c| p + c.len_utf8())
}

fn line_end(src: &str, p: usize) -> usize {
    src[p..].find('\n').map_or(src.len(), |ix| p + ix)
}

/// Where the line after the one ending at `end` starts, or the text's end.
fn next_line(src: &str, end: usize) -> usize {
    (end + 1).min(src.len())
}

/// The line before the one starting at `p`.
fn previous_line(src: &str, p: usize) -> &str {
    if p == 0 {
        return "";
    }
    let end = p - 1;
    let start = src[..end].rfind('\n').map_or(0, |ix| ix + 1);
    &src[start..end]
}

/// The length of a heading or list marker and the space after it, when text follows.
fn block_marker(line: &str) -> Option<usize> {
    let marker = marker_len(line)?;
    let space = line[marker..]
        .find(|c: char| c != ' ' && c != '\t')
        .filter(|&space| space > 0)?;
    Some(marker + space)
}

/// The length of `#` to `######`, `-`, `*` or `1.` at the start of `line`.
fn marker_len(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let hashes = bytes.iter().take_while(|&&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        return Some(hashes);
    }
    if matches!(bytes.first(), Some(b'-' | b'*')) {
        return Some(1);
    }
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    (digits > 0 && bytes.get(digits) == Some(&b'.')).then_some(digits + 1)
}

/// Whether an unfinished line may still turn into a heading or a list item.
fn could_be_marker(line: &str) -> bool {
    let trimmed = line.trim_end_matches([' ', '\t']);
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    marker_len(trimmed) == Some(trimmed.len()) || (digits > 0 && digits == trimmed.len())
}

/// A table row's cells, without the pipes at its ends.
fn cells(line: &str) -> Vec<&str> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    line.split('|').map(str::trim).collect()
}

/// Whether `line` is a table's delimiter row for a header of `columns` cells, as GFM
/// reads it: only then does the header row above it turn into a table.
fn is_delimiter_row(line: &str, columns: usize) -> bool {
    let cells = cells(line);
    line.trim_start().starts_with('|')
        && cells.len() == columns
        && cells.iter().all(|cell| {
            let dashes = cell.trim_start_matches(':').trim_end_matches(':');
            !dashes.is_empty() && dashes.bytes().all(|b| b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_fence(src: &str, p: usize) -> bool {
        Fences::default().read_to(src, p).open
    }

    fn unit(src: &str, p: usize) -> Unit {
        unit_at(src, p, false, in_fence(src, p))
    }

    #[test]
    fn plain_text_goes_a_character_at_a_time() {
        assert_eq!(unit("ab", 0), Unit::Char);
        assert_eq!(unit("中文", 0), Unit::Char);
        assert_eq!(next_char("中文", 0), 3);
    }

    #[test]
    fn bold_waits_for_its_close_then_shows_whole() {
        assert_eq!(unit("a **bol", 2), Unit::Wait);
        assert_eq!(unit("a **bold** b", 2), Unit::Through(10));
        // The line ended without a close, so the asterisks are text.
        assert_eq!(unit("a **bold\nmore", 2), Unit::Char);
        assert_eq!(unit("a *", 2), Unit::Wait);
        assert_eq!(unit_at("a *", 2, true, false), Unit::Char);
    }

    #[test]
    fn links_show_whole_once_their_target_closes() {
        let src = "see [session.rs](crates/acp-gui/src/session.rs) now";
        assert_eq!(unit(src, 4), Unit::Through(47));
        assert_eq!(unit("see [session.rs](crates/ac", 4), Unit::Wait);
        assert_eq!(unit("see [session.rs]", 4), Unit::Wait);
        assert_eq!(unit("see ![logo](a.png)", 4), Unit::Through(18));
        // Brackets with no target are text, as is one that never closes on its line.
        assert_eq!(unit("a [x] b", 2), Unit::Char);
        assert_eq!(unit("a [x\nb", 2), Unit::Char);
    }

    #[test]
    fn single_marks_and_strikethrough_wait_for_their_close() {
        assert_eq!(unit("a *note", 2), Unit::Wait);
        assert_eq!(unit("a *note* b", 2), Unit::Through(8));
        assert_eq!(unit("a _note_ b", 2), Unit::Through(8));
        assert_eq!(unit("a ~~old~~ b", 2), Unit::Through(9));
        assert_eq!(unit("a ~~ol", 2), Unit::Wait);
        // Before a space a mark opens nothing, and underscores inside a word are text.
        assert_eq!(unit("2 * 3", 2), Unit::Char);
        assert_eq!(unit("snake_case", 5), Unit::Char);
        assert_eq!(unit("my__var", 3), Unit::Char);
    }

    #[test]
    fn inline_code_shows_whole() {
        assert_eq!(unit("see `sess", 4), Unit::Wait);
        assert_eq!(unit("see `session.rs` here", 4), Unit::Through(16));
    }

    #[test]
    fn markers_show_with_the_first_unit_of_their_text() {
        assert_eq!(unit("## 原因", 0), Unit::Through(6));
        assert_eq!(unit("##", 0), Unit::Wait);
        assert_eq!(unit("1", 0), Unit::Wait);
        assert_eq!(unit("1. ", 0), Unit::Wait);
        assert_eq!(unit("1. **分块** x", 0), Unit::Through(13));
        assert_eq!(unit("- item", 0), Unit::Through(3));
        // A rule is text, not a marker waiting for its item.
        assert_eq!(unit("---\n", 0), Unit::Char);
    }

    #[test]
    fn a_table_header_waits_for_its_delimiter_row() {
        let header = "| a | b |\n";
        assert_eq!(unit(header, 0), Unit::Wait);
        assert_eq!(unit("| a | b |\n| --- | -", 0), Unit::Wait);
        let src = "| a | b |\n| --- | --- |\n| 1";
        assert_eq!(unit(src, 0), Unit::Through(24));
        // Body rows type into their cells.
        assert_eq!(unit(src, 24), Unit::Char);
        // Pipes that never make a table are text.
        assert_eq!(unit("| a | b |\nno table\n", 0), Unit::Char);
    }

    #[test]
    fn fences_show_whole_and_their_code_types() {
        assert_eq!(unit("```ru", 0), Unit::Wait);
        assert_eq!(unit("```rust\nfn", 0), Unit::Through(8));
        let src = "```rust\nlet a = **b;\n```\n";
        assert_eq!(unit(src, 16), Unit::Char);
        assert_eq!(unit(src, 21), Unit::Through(25));
        assert!(!in_fence(src, 25));
    }

    #[test]
    fn delimiter_rows_need_as_many_cells_as_the_header() {
        assert!(is_delimiter_row("| --- | :-: |", 2));
        assert!(is_delimiter_row("| --- | -", 2));
        assert!(!is_delimiter_row("| --- |", 2));
        assert!(!is_delimiter_row("| --- | ab |", 2));
    }

    #[test]
    fn pieces_keep_pace_and_never_split_a_run() {
        let start = Instant::now();
        let mut pacer = Pacer::default();
        pacer.arrived("我看了 `session.rs` 里的路径", start);
        let mut shown = String::new();
        let mut now = start;
        for _ in 0..120 {
            now += Duration::from_millis(16);
            if let Some(piece) = pacer.next(Duration::from_millis(16), now) {
                shown.push_str(&piece);
                // Never stops inside the code span.
                assert_eq!(shown.matches('`').count() % 2, 0, "{shown}");
            }
        }
        assert_eq!(shown, "我看了 `session.rs` 里的路径");
        assert!(!pacer.holds());
    }

    #[test]
    fn a_burst_after_a_stall_spreads_over_about_the_lag() {
        let start = Instant::now();
        let mut pacer = Pacer::default();
        let burst = "适配器把模型的增量原样转发，一块可能是两个字，也可能是一整句，然后又停住。";
        pacer.arrived(burst, start);
        let tick = Duration::from_millis(16);
        let mut now = start + tick;
        let first = pacer.next(tick, now).unwrap_or_default();
        assert!(first.chars().count() < 5, "{first}");
        let mut shown = first;
        while pacer.has_text() {
            now += tick;
            shown.extend(pacer.next(tick, now));
        }
        assert_eq!(shown, burst);
        let took = now.duration_since(start);
        assert!(
            took > Duration::from_millis(200) && took < Duration::from_millis(600),
            "{took:?}"
        );
    }

    #[test]
    fn a_streamed_reply_never_shows_half_a_run_or_a_bare_table_header() {
        let reply = "我看了 `session.rs` 里的路径：\n\n## 原因\n\n1. **分块不均匀**：一块可能是两个字。\n\n\
                     ```rust\nlet a = **b;\n```\n\n| 方案 | 改动 |\n| --- | --- |\n| 淡入 | 2 行 |\n\n\
                     这样**不会**蹦出一大段。";
        let tick = Duration::from_millis(16);
        let start = Instant::now();
        let mut now = start;
        let mut pacer = Pacer::default();
        let mut shown = String::new();
        // Bursts of 7 characters every 120ms.
        let chars: Vec<char> = reply.chars().collect();
        let mut sent = 0;
        while sent < chars.len() || pacer.has_text() {
            if sent < chars.len()
                && now.duration_since(start).as_millis() as usize >= sent / 7 * 120
            {
                let burst: String = chars[sent..(sent + 7).min(chars.len())].iter().collect();
                sent += burst.chars().count();
                pacer.arrived(&burst, now);
            }
            now += tick;
            if let Some(piece) = pacer.next(tick, now) {
                shown.push_str(&piece);
                let prose = shown.split("```").step_by(2).collect::<String>();
                assert_eq!(prose.matches("**").count() % 2, 0, "{shown}");
                assert_eq!(prose.matches('`').count() % 2, 0, "{shown}");
                assert!(!shown.ends_with("| 改动 |\n"), "{shown}");
                assert!(!shown.ends_with("```ru"), "{shown}");
            }
        }
        assert_eq!(shown, reply);
    }

    #[test]
    fn a_new_row_starts_from_rest_after_a_fast_one() {
        let tick = Duration::from_millis(16);
        let start = Instant::now();
        let mut pacer = Pacer::default();
        pacer.start_row(false);
        // A long reply that ends while catching up, with the turn's end behind it.
        pacer.arrived(&"字".repeat(400), start);
        pacer
            .after
            .push_back(SessionEvent::TurnFailed(String::new()));
        let mut now = start;
        while pacer.has_text() {
            now += tick;
            pacer.next(tick, now);
        }
        pacer.after.clear();
        pacer.start_row(false);
        let next = "下一个回合的第一句话，大约二十个字。";
        pacer.arrived(next, now);
        now += tick;
        let first = pacer.next(tick, now).unwrap_or_default();
        assert!(first.chars().count() < 5, "{first}");
    }

    #[test]
    fn a_run_that_never_closes_shows_after_the_wait_limit() {
        let start = Instant::now();
        let mut pacer = Pacer::default();
        pacer.arrived("a **b", start);
        let mut shown = String::new();
        let mut now = start;
        for _ in 0..30 {
            now += Duration::from_millis(16);
            shown.extend(pacer.next(Duration::from_millis(16), now));
        }
        assert_eq!(shown, "a ");
        for _ in 0..60 {
            now += Duration::from_millis(16);
            shown.extend(pacer.next(Duration::from_millis(16), now));
        }
        assert_eq!(shown, "a **b");
    }
}
