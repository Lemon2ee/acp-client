//! A session's context window as its info panel shows it: how full it is, as the agent
//! last counted; what fills it, from the Markdown Claude Code's `/context` writes; and the
//! tokens its turns took, which is all Codex's adapter says.

use acp_core::{ContextUsage, TurnUsage};
use serde::{Deserialize, Serialize};

/// What the window knows of a session's context window.
#[derive(Default)]
pub(crate) struct SessionContext {
    /// How full it is, as the agent last counted.
    pub(crate) usage: Option<ContextUsage>,
    /// When `usage` was counted, for a count kept from an earlier run: neither adapter
    /// counts as it loads a session, so the kept one shows until the next turn. None
    /// once the agent has counted in this run.
    pub(crate) kept_at: Option<i64>,
    /// The tokens of every turn the session has had, added up.
    pub(crate) totals: TurnUsage,
    /// What filled it at the last read, and when that was, in seconds since 1970.
    pub(crate) report: Option<(ContextReport, i64)>,
    /// The agent can say what fills it (see `acp_core::Command::ReadContext`).
    pub(crate) readable: bool,
    /// A read has been asked for and not answered.
    pub(crate) reading: bool,
    /// The last read was asked for after the last prompt, so asking again would tell
    /// nothing new. A read asked for during a turn runs once the turn is over.
    pub(crate) fresh: bool,
}

impl SessionContext {
    /// Adds a turn's tokens to the session's.
    pub(crate) fn took(&mut self, turn: TurnUsage) {
        let totals = &mut self.totals;
        totals.input += turn.input;
        totals.cached_read += turn.cached_read;
        totals.cached_write += turn.cached_write;
        totals.output += turn.output;
        totals.thought += turn.thought;
    }

    /// What is kept of it for later runs, once the agent has counted.
    pub(crate) fn kept(&self) -> Option<KeptContext> {
        let usage = self.usage?;
        Some(KeptContext {
            used: usage.used,
            size: usage.size,
            at: self.kept_at.unwrap_or_else(now),
            totals: self.totals,
        })
    }

    /// The count and the totals kept from an earlier run.
    pub(crate) fn restore(&mut self, kept: &KeptContext) {
        self.usage = Some(ContextUsage {
            used: kept.used,
            size: kept.size,
        });
        self.kept_at = Some(kept.at);
        self.totals = kept.totals;
    }

    /// What is kept of the last report for later runs, once there is one.
    pub(crate) fn kept_report(&self) -> Option<KeptReport> {
        let (report, at) = self.report.as_ref()?;
        Some(KeptReport {
            report: report.clone(),
            at: *at,
        })
    }

    /// The report kept from an earlier run, after `restore`. One read after the last count
    /// kept, which each turn's end makes, came after the last turn: it tells as much as a
    /// read now would, so none is asked for.
    pub(crate) fn restore_report(&mut self, kept: KeptReport) {
        self.fresh = self.kept_at.is_none_or(|counted| kept.at >= counted);
        self.report = Some((kept.report, kept.at));
    }
}

/// A session's context window as kept for later runs: the last count and when it was
/// made, and the tokens its turns took.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct KeptContext {
    pub(crate) used: u64,
    pub(crate) size: u64,
    /// In seconds since 1970.
    pub(crate) at: i64,
    pub(crate) totals: TurnUsage,
}

/// What filled a session's context window at its last read, as kept for later runs.
#[derive(Serialize, Deserialize)]
pub(crate) struct KeptReport {
    pub(crate) report: ContextReport,
    /// In seconds since 1970.
    pub(crate) at: i64,
}

/// Now, in seconds since 1970.
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// What fills the context window, from `/context`'s Markdown.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ContextReport {
    /// In the order the report lists them, those holding no tokens left out, except
    /// the free space and the buffer.
    pub(crate) categories: Vec<Category>,
    /// The window the counts are out of, from the report's `Tokens:` line.
    pub(crate) size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Category {
    pub(crate) name: String,
    pub(crate) tokens: u64,
    pub(crate) kind: CategoryKind,
    /// What makes it up, where the report lists that: each MCP server's tools, each
    /// memory file, custom agent and skill.
    pub(crate) items: Vec<Item>,
}

/// What a category's tokens do. Claude Code also lists tool schemas it loads only when
/// asked for, as "(deferred)", outside the window; those are left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CategoryKind {
    /// In the window.
    Used,
    /// Kept free for compacting: once what is used reaches it, Claude Code compacts.
    Buffer,
    Free,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Item {
    pub(crate) name: String,
    pub(crate) tokens: u64,
}

/// One of `/context`'s tables, the separator row left out.
struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// The column whose header is `name`, ignoring case.
    fn column(&self, name: &str) -> Option<usize> {
        self.header
            .iter()
            .position(|cell| cell.eq_ignore_ascii_case(name))
    }
}

impl ContextReport {
    /// Reads `/context`'s Markdown: a table of categories, `| Category | Tokens |
    /// Percentage |`, and a table of items for some of them under a heading that names
    /// the category (`### MCP Tools`, `### Memory Files`, ...). Only the Markdown is
    /// there to read: the SDK's structured twin of it doesn't reach ACP clients. None
    /// for text with no categories.
    pub(crate) fn parse(markdown: &str) -> Option<Self> {
        let mut report = ContextReport {
            categories: Vec::new(),
            size: None,
        };
        let mut heading = String::new();
        let mut table: Option<Table> = None;
        for line in markdown.lines().map(str::trim) {
            if let Some(cells) = cells(line) {
                match &mut table {
                    None => {
                        table = Some(Table {
                            header: cells,
                            rows: Vec::new(),
                        })
                    }
                    Some(table) if !is_rule(&cells) => table.rows.push(cells),
                    Some(_) => {}
                }
                continue;
            }
            if let Some(table) = table.take()
                && !report.take(&table, &heading)
            {
                return Some(report);
            }
            if line.starts_with('#') {
                heading = line.trim_start_matches('#').trim().to_lowercase();
            } else if let Some((_, counts)) = line.split_once("Tokens:") {
                // `**Tokens:** 84.2k / 200.0k (42%)`
                let window = counts.split('/').nth(1).unwrap_or_default();
                let window = window.split('(').next().unwrap_or_default();
                report.size = tokens(window.trim_matches(|c: char| c == '*' || c.is_whitespace()));
            }
        }
        if let Some(table) = table.take() {
            // A second table of categories ending the text goes unread all the same.
            report.take(&table, &heading);
        }
        (!report.categories.is_empty()).then_some(report)
    }

    /// Takes in one of the report's tables, read under `heading`. False for a second table
    /// of categories, which starts the report over: claude-agent-acp 0.94.0 lets
    /// `/context`'s output through twice when the SDK's copies of it differ at all, so
    /// the answer can hold the whole report twice.
    fn take(&mut self, table: &Table, heading: &str) -> bool {
        let tokens_at = table
            .column("tokens")
            .unwrap_or(table.header.len().saturating_sub(1));
        let count = |row: &[String]| row.get(tokens_at).and_then(|cell| tokens(cell));
        if table.column("category") == Some(0) {
            if !self.categories.is_empty() {
                return false;
            }
            for row in &table.rows {
                let (Some(name), Some(tokens)) = (row.first(), count(row)) else {
                    continue;
                };
                let lower = name.to_lowercase();
                let kind = if lower.contains("free space") {
                    CategoryKind::Free
                } else if lower.contains("buffer") {
                    CategoryKind::Buffer
                } else {
                    CategoryKind::Used
                };
                if lower.contains("deferred") || kind == CategoryKind::Used && tokens == 0 {
                    continue;
                }
                self.categories.push(Category {
                    name: name.clone(),
                    tokens,
                    kind,
                    items: Vec::new(),
                });
            }
            return true;
        }
        // The heading names the category the items make up.
        let Some(category) = [
            ("mcp", "mcp tools"),
            ("memory", "memory files"),
            ("agent", "custom agents"),
            ("skill", "skills"),
        ]
        .iter()
        .find(|(word, _)| heading.contains(word))
        .and_then(|(_, name)| {
            self.categories
                .iter_mut()
                .find(|category| category.name.eq_ignore_ascii_case(name))
        }) else {
            return true;
        };
        let (server, path, kind) = (
            table.column("server"),
            table.column("path"),
            table.column("type"),
        );
        let cell = |row: &[String], at: Option<usize>| at.and_then(|at| row.get(at)).cloned();
        // An MCP server's tools make one item, as there can be dozens of them: its name,
        // their tokens and how many there are.
        let mut servers: Vec<(String, u64, usize)> = Vec::new();
        for row in &table.rows {
            let Some(tokens) = count(row) else {
                continue;
            };
            if let Some(server) = cell(row, server) {
                match servers.iter_mut().find(|(name, ..)| *name == server) {
                    Some((_, sum, tools)) => {
                        *sum += tokens;
                        *tools += 1;
                    }
                    None => servers.push((server, tokens, 1)),
                }
                continue;
            }
            // A memory file's path comes first, for the panel to shorten.
            let name = match (cell(row, path), cell(row, kind)) {
                (Some(path), Some(kind)) => format!("{path} · {kind}"),
                (Some(path), None) => path,
                _ => row.first().cloned().unwrap_or_default(),
            };
            category.items.push(Item { name, tokens });
        }
        for (server, tokens, tools) in servers {
            let tools = match tools {
                1 => "1 tool".to_string(),
                tools => format!("{tools} tools"),
            };
            let name = format!("{server} · {tools}");
            category.items.push(Item { name, tokens });
        }
        true
    }
}

/// A Markdown table row's cells, or None for a line that isn't one.
fn cells(line: &str) -> Option<Vec<String>> {
    let inner = line.strip_prefix('|')?;
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    Some(
        inner
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

/// The row of dashes under a table's header.
fn is_rule(cells: &[String]) -> bool {
    cells
        .iter()
        .all(|cell| !cell.is_empty() && cell.chars().all(|c| matches!(c, '-' | ':' | ' ')))
}

/// A count as `/context` writes it: `600`, `14,800`, `3.1k`, `1.2M`.
fn tokens(text: &str) -> Option<u64> {
    let text = text.trim().replace(',', "");
    let (number, scale) = match text.chars().last()? {
        'k' | 'K' => (&text[..text.len() - 1], 1e3),
        'm' | 'M' => (&text[..text.len() - 1], 1e6),
        _ => (text.as_str(), 1.),
    };
    let number: f64 = number.trim().parse().ok()?;
    (number >= 0.).then(|| (number * scale).round() as u64)
}

/// A count of tokens as the panel writes it: `0.6k`, `84.2k`, `200k`, `1.2M`.
pub(crate) fn short_tokens(count: u64) -> String {
    let thousands = count as f64 / 1e3;
    if thousands >= 1e3 {
        format!("{:.1}M", thousands / 1e3)
    } else if thousands >= 100. {
        format!("{}k", thousands.round())
    } else {
        format!("{thousands:.1}k")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "## Context Usage

**Model:** claude-fable-5-1
**Tokens:** 84.2k / 200.0k (42%)

### Estimated usage by category

| Category | Tokens | Percentage |
|----------|--------|------------|
| System prompt | 3.1k | 1.6% |
| System tools | 14.8k | 7.4% |
| MCP tools | 2.3k | 1.2% |
| MCP tools (deferred) | 9.0k | 4.5% |
| Custom agents | 0 | 0.0% |
| Memory files | 4,100 | 2.1% |
| Messages | 58.1k | 29.1% |
| Free space | 82.8k | 41.4% |
| Autocompact buffer | 33.0k | 16.5% |

### MCP Tools

| Tool | Server | Tokens |
|------|--------|--------|
| mcp__github__create_issue | github | 1.5k |
| mcp__github__get_me | github | 800 |

### Memory Files

| Type | Path | Tokens |
|------|------|--------|
| Project | /Users/lemon/acp-client/CLAUDE.md | 3.6k |
| User | /Users/lemon/.claude/CLAUDE.md | 500 |
";

    #[test]
    fn reads_the_categories_and_their_items() {
        let report = ContextReport::parse(REPORT).unwrap();
        assert_eq!(report.size, Some(200_000));
        let names: Vec<_> = report.categories.iter().map(|c| c.name.as_str()).collect();
        // The deferred tools and the empty category are left out.
        assert_eq!(
            names,
            [
                "System prompt",
                "System tools",
                "MCP tools",
                "Memory files",
                "Messages",
                "Free space",
                "Autocompact buffer"
            ]
        );
        let kinds: Vec<_> = report.categories.iter().map(|c| c.kind).collect();
        assert_eq!(kinds[4], CategoryKind::Used);
        assert_eq!(kinds[5], CategoryKind::Free);
        assert_eq!(kinds[6], CategoryKind::Buffer);
        assert_eq!(report.categories[3].tokens, 4_100);
        assert_eq!(
            report.categories[2].items,
            [Item {
                name: "github · 2 tools".into(),
                tokens: 2_300
            }]
        );
        assert_eq!(
            report.categories[3].items,
            [
                Item {
                    name: "/Users/lemon/acp-client/CLAUDE.md · Project".into(),
                    tokens: 3_600
                },
                Item {
                    name: "/Users/lemon/.claude/CLAUDE.md · User".into(),
                    tokens: 500
                }
            ]
        );
    }

    #[test]
    fn a_report_sent_twice_is_read_once() {
        let once = ContextReport::parse(REPORT).unwrap();
        let twice = ContextReport::parse(&format!("{REPORT}\n{REPORT}")).unwrap();
        assert_eq!(twice, once);
    }

    #[test]
    fn a_kept_report_read_after_the_last_turn_is_fresh() {
        let kept = |at| KeptReport {
            report: ContextReport::parse(REPORT).unwrap(),
            at,
        };
        let counted = KeptContext {
            used: 84_200,
            size: 200_000,
            at: 1_000,
            totals: TurnUsage::default(),
        };
        let mut context = SessionContext::default();
        context.restore(&counted);
        context.restore_report(kept(1_010));
        assert!(context.fresh);
        // A turn ended after it, with no read since.
        context.restore_report(kept(990));
        assert!(!context.fresh);
    }

    #[test]
    fn text_without_categories_is_no_report() {
        assert_eq!(ContextReport::parse("Unknown command: /context"), None);
    }

    #[test]
    fn writes_counts_short() {
        assert_eq!(short_tokens(600), "0.6k");
        assert_eq!(short_tokens(84_213), "84.2k");
        assert_eq!(short_tokens(200_000), "200k");
        assert_eq!(short_tokens(258_400), "258k");
        assert_eq!(short_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn reads_counts() {
        assert_eq!(tokens("3.1k"), Some(3_100));
        assert_eq!(tokens("14,800"), Some(14_800));
        assert_eq!(tokens("1.2M"), Some(1_200_000));
        assert_eq!(tokens("n/a"), None);
    }
}
