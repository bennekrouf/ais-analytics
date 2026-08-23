//! Following one key value through the tables that carry it.
//!
//! The setup phase decided *what* to trace on; this decides *what happened*
//! to one particular value. The result is a set of lanes, each in one of
//! four states — which is the whole point of the view: an empty lane is as
//! informative as a full one, as long as you can tell "hasn't arrived" apart
//! from "was never on this path".
//!
//! A lane is a Log Analytics table by default, but it doesn't have to be. A
//! schema that names its own stages (a workflow name, say) can drive the
//! axis instead of the physical table layout, and `relane` regroups what is
//! already in hand rather than re-querying.
//!
//! Where the Cosmos original had to fan out — one query per container,
//! because that was the only axis Cosmos offered — KQL unions, so a whole
//! trace is a handful of round-trips regardless of how many lanes it spans.

use crate::services::discover::KeyCandidate;
use crate::services::loganalytics::{Client, TimeRange, column_ref, kql_string, let_literal, table_ref};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Per lane. A single flow producing more than this is pathological.
const MAX_BLOCKS: usize = 200;
/// Facts shown on a block card before it stops being "essential data".
const MAX_FACTS: usize = 3;
/// Tables unioned per query. Bounds the blast radius of one unqueryable
/// table: the Cosmos version could fail a single container without losing
/// the rest, and chunking preserves that.
const TABLES_PER_QUERY: usize = 25;

/// Injected by the union so each row knows which table it came from. Named
/// to be unlikely to collide with a real column.
const LANE_COLUMN: &str = "__ais_lane";

/// Columns Log Analytics injects into every table — never worth showing as
/// a fact on a card.
const SYSTEM_FIELDS: [&str; 12] = [
    "TenantId",
    "SourceSystem",
    "MG",
    "ManagementGroupName",
    "Type",
    "_ResourceId",
    "_SubscriptionId",
    "_ItemId",
    "_BilledSize",
    "_IsBillable",
    "_TimeReceived",
    "_Internal_WorkspaceResourceId",
];

/// What to trace, and how to read each row once found.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceSpec {
    pub key: KeyCandidate,
    pub value: String,
    /// Lowercased column paths; empty string means "not chosen".
    pub time_field: String,
    pub label_field: String,
    /// Columns worth showing on a card, best first.
    pub fact_fields: Vec<String>,
    pub range: TimeRange,
}

/// A column value that means "this step failed".
///
/// What counts as an error is domain knowledge the data doesn't carry —
/// `ResultCode: 500` means nothing without someone saying so. Rules are
/// therefore user-supplied and persisted per workspace, never guessed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorRule {
    /// Lowercased dotted column path — what matching uses.
    pub field: String,
    /// The column's original spelling. Matching is case-insensitive, but the
    /// UI shows columns as the workspace spells them everywhere else.
    #[serde(default)]
    pub display: String,
    /// Compared against the row's value as text, case-insensitively.
    pub value: String,
}

impl ErrorRule {
    /// `ResultCode = 500`, for display.
    pub fn label(&self) -> String {
        let name = if self.display.is_empty() {
            leaf(&self.field)
        } else {
            self.display.clone()
        };
        format!("{name} = {}", self.value)
    }
}

/// Whether any rule matches — rules are OR'd, so several values of the same
/// column (or several columns) can all mean failure.
pub fn is_error(doc: &Value, rules: &[ErrorRule]) -> bool {
    rules.iter().any(|rule| {
        lookup(doc, &rule.field)
            .and_then(scalar_text)
            .is_some_and(|actual| actual.eq_ignore_ascii_case(&rule.value))
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// Stable across the sort, so a selected card stays selected.
    pub id: String,
    pub label: String,
    /// Epoch milliseconds, when a usable time column was found.
    pub at: Option<i64>,
    pub at_text: String,
    pub facts: Vec<(String, String)>,
    /// Where the row actually lives. Once lanes can be something other than
    /// tables, the card is the only thing that still knows.
    pub table: String,
    /// The correlation value this row actually carries. With fragment search
    /// that need not equal what the user typed.
    pub key_value: String,
    /// The row as returned, for the detail panel. A card can only ever show
    /// a summary; this is what it summarises.
    pub doc: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneState {
    /// The key value was found here.
    Reached,
    /// This table carries the key, but not this value — the data has not
    /// arrived (or never will).
    Awaiting,
    /// The key doesn't exist in this table at all: it isn't on this path, so
    /// its emptiness means nothing.
    OffPath,
    Failed(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    /// A table name, or a value of the chosen lane column.
    pub name: String,
    /// Secondary line: the key's column path for table lanes, the source
    /// tables for field lanes.
    pub detail: Option<String>,
    pub blocks: Vec<Block>,
    pub state: LaneState,
    pub error: Option<String>,
}

impl Lane {
    pub fn first_at(&self) -> Option<i64> {
        self.blocks.iter().filter_map(|b| b.at).min()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    pub value: String,
    pub key_label: String,
    pub lanes: Vec<Lane>,
    /// Earliest and latest observed times, when any were found.
    pub span: Option<(i64, i64)>,
    pub blocks_found: usize,
    /// True when the value was matched as a fragment rather than in full.
    pub partial: bool,
    /// Distinct key values the search hit, with their row counts. More than
    /// one means the fragment is ambiguous and the timeline would be mixing
    /// unrelated flows.
    pub matches: Vec<(String, usize)>,
}

impl Trace {
    pub fn reached(&self) -> usize {
        self.lanes
            .iter()
            .filter(|l| l.state == LaneState::Reached)
            .count()
    }

    pub fn awaiting(&self) -> usize {
        self.lanes
            .iter()
            .filter(|l| l.state == LaneState::Awaiting)
            .count()
    }

    /// Resolves a card selection back to its lane and row.
    pub fn find_block(&self, id: &str) -> Option<(&Lane, &Block)> {
        self.lanes.iter().find_map(|lane| {
            lane.blocks
                .iter()
                .find(|b| b.id == id)
                .map(|block| (lane, block))
        })
    }
}

/// Shortest fragment worth scanning for. Below this almost everything
/// matches, and the scan is wasted.
pub const MIN_FRAGMENT: usize = 4;
/// Suggestions offered for a fragment before it's simply too vague.
const MAX_SUGGESTIONS: usize = 25;

/// The distinct key values containing `fragment`, for a type-ahead.
///
/// Deliberately not `run`: this asks for `distinct` values, so it returns a
/// handful of strings rather than every matching row. That is the difference
/// between a search you can run while someone types and one you can't.
pub async fn suggest(
    client: &Client,
    workspace_id: &str,
    tables: &[String],
    key: &KeyCandidate,
    fragment: &str,
    range: TimeRange,
) -> Vec<String> {
    let fragment = fragment.trim();
    if fragment.len() < MIN_FRAGMENT {
        return Vec::new();
    }

    let bound: Vec<&crate::services::discover::Binding> =
        tables.iter().filter_map(|t| key.binding_for(t)).collect();
    if bound.is_empty() {
        return Vec::new();
    }

    let mut found: BTreeSet<String> = BTreeSet::new();
    for chunk in bound.chunks(TABLES_PER_QUERY) {
        let kql = suggest_query(chunk, fragment);
        // A chunk that can't be read shouldn't silence the others.
        if let Ok(rows) = client.query(workspace_id, &kql, range).await {
            found.extend(
                rows.iter()
                    .filter_map(|r| r.get("Value").and_then(Value::as_str))
                    .map(str::to_string),
            );
        }
        if found.len() >= MAX_SUGGESTIONS {
            break;
        }
    }

    found.into_iter().take(MAX_SUGGESTIONS).collect()
}

fn suggest_query(bound: &[&crate::services::discover::Binding], fragment: &str) -> String {
    let branches = bound
        .iter()
        .map(|b| {
            let column = column_ref(&b.field);
            format!(
                "    ({} | where tostring({column}) contains v | project Value = tostring({column}))",
                table_ref(&b.table)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{}\nunion isfuzzy=true\n{branches}\n| distinct Value\n| take {MAX_SUGGESTIONS}",
        let_literal("v", fragment)
    )
}

/// Queries every table bound to the key, and records the ones that aren't
/// bound at all so the view can show the difference.
pub async fn run(
    client: &Client,
    workspace_id: &str,
    tables: &[String],
    spec: &TraceSpec,
) -> Trace {
    // Exact first: it can use the index and is the overwhelmingly common
    // case. Only when nothing carries that value do we fall back to scanning
    // for it as a fragment, which lets a user paste the first block of a
    // trace id.
    let exact = fetch(client, workspace_id, tables, spec, false).await;
    if exact.blocks_found > 0 || spec.value.trim().is_empty() {
        return exact;
    }
    let fragment = fetch(client, workspace_id, tables, spec, true).await;
    if fragment.blocks_found == 0 {
        // Nothing either way — report the exact attempt, whose lane states
        // describe the workspace rather than a failed scan.
        return exact;
    }
    fragment
}

async fn fetch(
    client: &Client,
    workspace_id: &str,
    tables: &[String],
    spec: &TraceSpec,
    partial: bool,
) -> Trace {
    let mut lanes: Vec<Lane> = Vec::new();
    let mut bound: Vec<&crate::services::discover::Binding> = Vec::new();

    for path in tables {
        match spec.key.binding_for(path) {
            Some(binding) => bound.push(binding),
            None => lanes.push(Lane {
                name: path.clone(),
                detail: None,
                blocks: Vec::new(),
                state: LaneState::OffPath,
                error: None,
            }),
        }
    }

    // Rows arrive interleaved from the union, so they are collected by lane
    // and turned into blocks once the whole chunk is in.
    let mut rows_by_table: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut failures: BTreeMap<&str, String> = BTreeMap::new();

    for chunk in bound.chunks(TABLES_PER_QUERY) {
        let kql = trace_query(chunk, &spec.value, partial);
        match client.query(workspace_id, &kql, spec.range).await {
            Ok(rows) => {
                for row in rows {
                    let Some(table) = row.get(LANE_COLUMN).and_then(Value::as_str) else {
                        continue;
                    };
                    rows_by_table
                        .entry(table.to_string())
                        .or_default()
                        .push(row);
                }
            }
            Err(e) => {
                for binding in chunk {
                    failures.insert(binding.table.as_str(), e.clone());
                }
            }
        }
    }

    for binding in &bound {
        if let Some(error) = failures.get(binding.table.as_str()) {
            lanes.push(Lane {
                name: binding.table.clone(),
                detail: Some(binding.field.clone()),
                blocks: Vec::new(),
                state: LaneState::Failed(0),
                error: Some(error.clone()),
            });
            continue;
        }

        let rows = rows_by_table.remove(&binding.table).unwrap_or_default();
        let mut blocks: Vec<Block> = rows
            .iter()
            .take(MAX_BLOCKS)
            .enumerate()
            .map(|(i, row)| {
                build_block(
                    row,
                    spec,
                    &binding.table,
                    &binding.field,
                    &format!("{}#{i}", binding.table),
                )
            })
            .collect();
        blocks.sort_by_key(|b| b.at.unwrap_or(i64::MAX));
        let state = if blocks.is_empty() {
            LaneState::Awaiting
        } else {
            LaneState::Reached
        };
        lanes.push(Lane {
            name: binding.table.clone(),
            detail: Some(binding.field.clone()),
            blocks,
            state,
            error: None,
        });
    }

    // Always table lanes here. Choosing a different axis is a display
    // decision, applied by `relane` — it must not require re-querying.
    sort_lanes(&mut lanes);

    let times: Vec<i64> = lanes
        .iter()
        .flat_map(|l| l.blocks.iter().filter_map(|b| b.at))
        .collect();
    let span = match (times.iter().min(), times.iter().max()) {
        (Some(&lo), Some(&hi)) => Some((lo, hi)),
        _ => None,
    };

    // Which distinct correlation values the search actually landed on. For
    // an exact search that is always one; a fragment can straddle several,
    // and the caller has to disambiguate before drawing a timeline.
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for block in lanes.iter().flat_map(|l| l.blocks.iter()) {
        if !block.key_value.is_empty() {
            *counts.entry(block.key_value.as_str()).or_default() += 1;
        }
    }
    let mut matches: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(v, n)| (v.to_string(), n))
        .collect();
    matches.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    Trace {
        value: spec.value.clone(),
        key_label: spec.key.label.clone(),
        blocks_found: lanes.iter().map(|l| l.blocks.len()).sum(),
        lanes,
        span,
        partial,
        matches,
    }
}

/// One union covering every bound table, each branch filtering on its own
/// column and tagging its rows with the lane they belong to.
///
/// `isfuzzy=true` matters here for the same reason it does in the scan: a
/// table that has since been retired should cost that lane, not the trace.
fn trace_query(
    bound: &[&crate::services::discover::Binding],
    value: &str,
    partial: bool,
) -> String {
    let branches = bound
        .iter()
        .map(|b| {
            format!(
                "    ({} | where {} | extend {LANE_COLUMN} = {})",
                table_ref(&b.table),
                predicate(&b.field, &b.kind, partial),
                kql_string(&b.table),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");

    format!(
        "{}\nunion isfuzzy=true\n{branches}\n| take {}",
        let_literal("v", value),
        MAX_BLOCKS * bound.len().max(1),
    )
}

/// How a column is compared against the searched value.
///
/// KQL is typed, and the user has typed characters. A declared `string`
/// column can be compared directly, which keeps the query indexed; anything
/// else — `guid`, `int`, a path inside a dynamic column — goes through
/// `tostring()` so a numeric id stored as a number still matches. The
/// fragment case is a scan either way.
fn predicate(field: &str, kind: &str, partial: bool) -> String {
    let column = column_ref(field);
    let comparable = if kind == "string" {
        column
    } else {
        format!("tostring({column})")
    };
    if partial {
        format!("{comparable} contains v")
    } else {
        format!("{comparable} == v")
    }
}

/// Chronological where we know the time, so the lanes read as the path the
/// data actually took; unreached lanes sink to the bottom.
fn sort_lanes(lanes: &mut [Lane]) {
    lanes.sort_by(|a, b| {
        let rank = |l: &Lane| match l.state {
            LaneState::Reached => 0,
            LaneState::Awaiting => 1,
            LaneState::Failed(_) => 2,
            LaneState::OffPath => 3,
        };
        rank(a)
            .cmp(&rank(b))
            .then(
                a.first_at()
                    .unwrap_or(i64::MAX)
                    .cmp(&b.first_at().unwrap_or(i64::MAX)),
            )
            .then(a.name.cmp(&b.name))
    });
}

/// Re-expresses a trace on a different axis, without touching the workspace.
///
/// The rows are already in hand, so switching between "one lane per table"
/// and "one lane per workflow name" is a pure transformation of what's on
/// screen. `lane_field` empty gives the table lanes back unchanged, so the
/// choice is reversible.
pub fn relane(base: &Trace, lane_field: &str, expected_lanes: &[String]) -> Trace {
    if lane_field.is_empty() {
        return base.clone();
    }
    let mut lanes = regroup(base.lanes.clone(), lane_field, expected_lanes);
    sort_lanes(&mut lanes);
    Trace {
        lanes,
        ..base.clone()
    }
}

/// Rebuilds table lanes into lanes of `lane_field`'s values.
///
/// Tables whose query failed stay as their own lanes — an error must not be
/// silently folded into "nothing here".
fn regroup(table_lanes: Vec<Lane>, lane_field: &str, expected_lanes: &[String]) -> Vec<Lane> {
    let unlabelled = format!("(no {})", leaf(lane_field));
    let mut grouped: BTreeMap<String, Vec<Block>> = BTreeMap::new();
    let mut failed = Vec::new();

    for lane in table_lanes {
        if lane.state == LaneState::Failed(0) {
            failed.push(lane);
            continue;
        }
        for block in lane.blocks {
            let name = lookup(&block.doc, lane_field)
                .and_then(scalar_text)
                .unwrap_or_else(|| unlabelled.clone());
            grouped.entry(name).or_default().push(block);
        }
    }

    // Every stage we know about, whether or not this value reached it. A
    // stage the sampling never saw cannot appear here — which is exactly why
    // `expected_lanes` is worth carrying.
    let mut names: BTreeSet<&str> = expected_lanes.iter().map(String::as_str).collect();
    names.extend(grouped.keys().map(String::as_str));

    let mut lanes: Vec<Lane> = names
        .into_iter()
        .map(|name| {
            let mut blocks = grouped.get(name).cloned().unwrap_or_default();
            blocks.sort_by_key(|b| b.at.unwrap_or(i64::MAX));
            let sources: BTreeSet<&str> = blocks.iter().map(|b| b.table.as_str()).collect();
            Lane {
                name: name.to_string(),
                detail: (!sources.is_empty())
                    .then(|| sources.into_iter().collect::<Vec<_>>().join(", ")),
                state: if blocks.is_empty() {
                    LaneState::Awaiting
                } else {
                    LaneState::Reached
                },
                blocks,
                error: None,
            }
        })
        .collect();

    lanes.append(&mut failed);
    lanes
}

fn build_block(
    doc: &Value,
    spec: &TraceSpec,
    table: &str,
    key_field: &str,
    id: &str,
) -> Block {
    let at = if spec.time_field.is_empty() {
        None
    } else {
        lookup(doc, &spec.time_field).and_then(parse_time)
    };

    let label = lookup(doc, &spec.label_field)
        .and_then(scalar_text)
        .unwrap_or_else(|| table.to_string());

    let mut facts: Vec<(String, String)> = Vec::new();
    let mut taken: Vec<String> = vec![
        key_field.to_lowercase(),
        spec.time_field.clone(),
        spec.label_field.clone(),
        LANE_COLUMN.to_lowercase(),
    ];

    // Preferred columns first — the ones discovery already judged
    // descriptive.
    for path in &spec.fact_fields {
        if facts.len() >= MAX_FACTS || taken.contains(path) {
            continue;
        }
        // Chosen columns are held lowercased for matching, but a card must
        // show the column the way the workspace spells it — `SeverityLevel`,
        // not `severitylevel`.
        if let Some((name, value)) = lookup_entry(doc, path) {
            if let Some(text) = scalar_text(value) {
                facts.push((name.to_string(), truncate(&text, 28)));
                taken.push(path.clone());
            }
        }
    }

    // Then whatever short scalars the row has, so a card is never blank.
    if facts.len() < MAX_FACTS {
        if let Value::Object(map) = doc {
            for (name, value) in map {
                if facts.len() >= MAX_FACTS
                    || name == LANE_COLUMN
                    || SYSTEM_FIELDS.iter().any(|s| s.eq_ignore_ascii_case(name))
                    || taken.contains(&name.to_lowercase())
                {
                    continue;
                }
                if let Some(text) = scalar_text(value).filter(|t| t.len() <= 28) {
                    facts.push((name.clone(), text));
                }
            }
        }
    }

    Block {
        id: id.to_string(),
        key_value: lookup(doc, &key_field.to_lowercase())
            .and_then(scalar_text)
            .unwrap_or_default(),
        label,
        at,
        at_text: at.map(format_time).unwrap_or_default(),
        facts,
        table: table.to_string(),
        doc: doc.clone(),
    }
}

/// Case-insensitive lookup down a dotted path.
fn lookup<'a>(doc: &'a Value, lower_path: &str) -> Option<&'a Value> {
    lookup_entry(doc, lower_path).map(|(_, value)| value)
}

/// As `lookup`, but also returns the key as the row actually spells it.
///
/// Paths are matched lowercased so a rule written once works across tables
/// that disagree on casing; anything user-facing wants the real spelling
/// back, which only the row can supply.
fn lookup_entry<'a>(doc: &'a Value, lower_path: &str) -> Option<(&'a str, &'a Value)> {
    if lower_path.is_empty() {
        return None;
    }
    let mut current = doc;
    let mut name = "";
    for segment in lower_path.split('.') {
        let Value::Object(map) = current else {
            return None;
        };
        let (key, value) = map.iter().find(|(k, _)| k.to_lowercase() == segment)?;
        name = key.as_str();
        current = value;
    }
    Some((name, current))
}

fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Epoch milliseconds from whatever shape the timestamp is stored in.
pub fn parse_time(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_f64().and_then(|n| epoch_to_millis(n as i64)),
        Value::String(s) => {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.timestamp_millis());
            }
            for format in [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%d",
            ] {
                if let Ok(dt) = NaiveDateTime::parse_from_str(s, format) {
                    return Some(dt.and_utc().timestamp_millis());
                }
            }
            s.parse::<i64>().ok().and_then(epoch_to_millis)
        }
        _ => None,
    }
}

/// Accepts epoch seconds or milliseconds, rejecting values outside a
/// plausible range so a random integer isn't read as a date.
fn epoch_to_millis(n: i64) -> Option<i64> {
    match n {
        1_000_000_000..=4_000_000_000 => Some(n * 1000),
        1_000_000_000_000..=4_000_000_000_000 => Some(n),
        _ => None,
    }
}

pub fn format_time(millis: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .map(|dt| dt.format("%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_default()
}

/// Human-readable gap between two instants.
pub fn format_gap(millis: i64) -> String {
    let s = millis as f64 / 1000.0;
    if millis < 1000 {
        format!("+{millis}ms")
    } else if s < 60.0 {
        format!("+{s:.1}s")
    } else if s < 3600.0 {
        format!("+{:.1}min", s / 60.0)
    } else if s < 86_400.0 {
        format!("+{:.1}h", s / 3600.0)
    } else {
        format!("+{:.1}d", s / 86_400.0)
    }
}

fn leaf(path: &str) -> String {
    path.rsplit('.').next().unwrap_or(path).to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::discover::Binding;
    use serde_json::json;

    fn binding(table: &str, field: &str, kind: &str) -> Binding {
        Binding {
            table: table.into(),
            field: field.into(),
            kind: kind.into(),
            seen_in: 10,
            sampled_rows: 10,
            distinct: 10,
        }
    }

    #[test]
    fn a_trace_is_one_union_with_a_branch_per_table() {
        let a = binding("AppRequests", "OperationId", "string");
        let b = binding("MyApp_CL", "job_ref_g", "string");
        let kql = trace_query(&[&a, &b], "abc-123", false);

        assert!(kql.starts_with(r#"let v = "abc-123";"#), "got: {kql}");
        assert!(kql.contains("['AppRequests']"));
        assert!(kql.contains("['MyApp_CL']"));
        // Each branch filters on its own column, which is the whole reason a
        // union is usable here at all.
        assert!(kql.contains("['OperationId'] == v"));
        assert!(kql.contains("['job_ref_g'] == v"));
        // One retired table must not fail the whole trace.
        assert!(kql.contains("isfuzzy=true"));
        // Rows must carry their lane back.
        assert!(kql.contains(r#"__ais_lane = "AppRequests""#));
    }

    /// KQL is typed and the user typed characters, so anything that isn't a
    /// declared string has to be coerced before comparison.
    #[test]
    fn non_string_columns_are_coerced_before_comparison() {
        assert_eq!(predicate("OperationId", "string", false), "['OperationId'] == v");
        assert_eq!(
            predicate("RequestId", "guid", false),
            "tostring(['RequestId']) == v"
        );
        assert_eq!(
            predicate("ResultCode", "int", false),
            "tostring(['ResultCode']) == v"
        );
        // Paths inside a dynamic column have no declared type at all.
        assert_eq!(
            predicate("Properties.correlationId", "", false),
            "tostring(['Properties']['correlationId']) == v"
        );
    }

    #[test]
    fn fragment_search_falls_back_to_a_substring_scan() {
        assert_eq!(predicate("OperationId", "string", true), "['OperationId'] contains v");
    }

    /// The injection case, end to end: a pasted value that tries to close
    /// the literal and append its own operators stays inside the string.
    #[test]
    fn a_hostile_value_cannot_escape_the_query() {
        let a = binding("AppRequests", "OperationId", "string");
        let kql = trace_query(&[&a], r#"x" | project 1 //"#, false);
        assert!(
            kql.starts_with(r#"let v = "x\" | project 1 //";"#),
            "the value must stay escaped inside the literal, got: {kql}"
        );
        // Exactly one statement terminator: the one we wrote.
        assert_eq!(kql.matches(';').count(), 1);
    }

    #[test]
    fn suggestions_ask_for_distinct_values_not_rows() {
        let a = binding("AppRequests", "OperationId", "string");
        let kql = suggest_query(&[&a], "abc");
        assert!(kql.contains("distinct Value"), "got: {kql}");
        assert!(kql.contains("contains v"));
        assert!(kql.contains("take 25"));
    }

    #[test]
    fn lanes_read_as_the_path_the_data_took() {
        let lane = |name: &str, state: LaneState, at: Option<i64>| Lane {
            name: name.into(),
            detail: None,
            blocks: at
                .map(|at| {
                    vec![Block {
                        id: name.into(),
                        label: name.into(),
                        at: Some(at),
                        at_text: String::new(),
                        facts: vec![],
                        table: name.into(),
                        key_value: "v".into(),
                        doc: json!({}),
                    }]
                })
                .unwrap_or_default(),
            state,
            error: None,
        };

        let mut lanes = vec![
            lane("off", LaneState::OffPath, None),
            lane("late", LaneState::Reached, Some(200)),
            lane("waiting", LaneState::Awaiting, None),
            lane("early", LaneState::Reached, Some(100)),
        ];
        sort_lanes(&mut lanes);
        let order: Vec<&str> = lanes.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(order, vec!["early", "late", "waiting", "off"]);
    }

    /// Regrouping is a pure transform of rows already fetched — switching
    /// axis must never need another query.
    #[test]
    fn relane_regroups_without_losing_rows_or_failures() {
        let block = |id: &str, stage: &str, table: &str| Block {
            id: id.into(),
            label: stage.into(),
            at: Some(1),
            at_text: String::new(),
            facts: vec![],
            table: table.into(),
            key_value: "v".into(),
            doc: json!({ "Stage": stage }),
        };
        let base = Trace {
            value: "v".into(),
            key_label: "OperationId".into(),
            blocks_found: 2,
            span: None,
            partial: false,
            matches: vec![],
            lanes: vec![
                Lane {
                    name: "AppRequests".into(),
                    detail: None,
                    blocks: vec![block("1", "Validate", "AppRequests")],
                    state: LaneState::Reached,
                    error: None,
                },
                Lane {
                    name: "MyApp_CL".into(),
                    detail: None,
                    blocks: vec![block("2", "Invoice", "MyApp_CL")],
                    state: LaneState::Reached,
                    error: None,
                },
                Lane {
                    name: "Broken_CL".into(),
                    detail: None,
                    blocks: vec![],
                    state: LaneState::Failed(0),
                    error: Some("denied".into()),
                },
            ],
        };

        let relaned = relane(&base, "stage", &["Validate".into(), "Invoice".into(), "Archive".into()]);
        let names: Vec<&str> = relaned.lanes.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"Validate"));
        assert!(names.contains(&"Invoice"));
        // A stage the value never reached still has to appear, or its
        // absence says nothing.
        assert!(names.contains(&"Archive"));
        // A failure must not be folded into "nothing here".
        assert!(
            relaned.lanes.iter().any(|l| l.state == LaneState::Failed(0)),
            "a failed lane must survive regrouping"
        );
        // Empty axis is the identity, so the choice is reversible.
        assert_eq!(relane(&base, "", &[]).lanes.len(), base.lanes.len());
    }

    #[test]
    fn error_rules_match_case_insensitively_across_types() {
        let rules = vec![ErrorRule {
            field: "resultcode".into(),
            display: "ResultCode".into(),
            value: "500".into(),
        }];
        assert!(is_error(&json!({"ResultCode": 500}), &rules));
        assert!(is_error(&json!({"resultcode": "500"}), &rules));
        assert!(!is_error(&json!({"ResultCode": 200}), &rules));
        assert_eq!(rules[0].label(), "ResultCode = 500");
    }

    #[test]
    fn nested_paths_resolve_case_insensitively() {
        let doc = json!({"Properties": {"CorrelationId": "abc"}});
        assert_eq!(
            lookup(&doc, "properties.correlationid").and_then(scalar_text),
            Some("abc".to_string())
        );
        assert!(lookup(&doc, "properties.missing").is_none());
        assert!(lookup(&doc, "").is_none());
    }

    #[test]
    fn timestamps_parse_from_every_shape_log_analytics_returns() {
        // The wire format for a datetime column.
        assert_eq!(
            parse_time(&json!("2026-01-04T10:00:00Z")),
            Some(1_767_520_800_000)
        );
        assert!(parse_time(&json!("2026-01-04T10:00:00.123Z")).is_some());
        assert!(parse_time(&json!("2026-01-04 10:00:00")).is_some());
        assert!(parse_time(&json!(1_767_520_800)).is_some());
        assert!(parse_time(&json!(1_767_520_800_000i64)).is_some());
        // A small integer is a count, not a date.
        assert_eq!(parse_time(&json!(42)), None);
        assert_eq!(parse_time(&json!("not a time")), None);
    }

    #[test]
    fn gaps_read_in_sensible_units() {
        assert_eq!(format_gap(250), "+250ms");
        assert_eq!(format_gap(1_500), "+1.5s");
        assert_eq!(format_gap(90_000), "+1.5min");
        assert_eq!(format_gap(5_400_000), "+1.5h");
    }

    #[test]
    fn cards_prefer_chosen_facts_then_fill_from_whatever_is_short() {
        let spec = TraceSpec {
            key: KeyCandidate {
                id: "k".into(),
                label: "OperationId".into(),
                bindings: vec![],
                missing: vec![],
                shared_values: 0,
                cross_named: false,
                id_shaped: true,
                well_known: true,
                avg_fill: 1.0,
                avg_distinct_ratio: 1.0,
                score: 1.0,
                evidence: vec![],
            },
            value: "abc".into(),
            time_field: "timegenerated".into(),
            label_field: "message".into(),
            fact_fields: vec!["severitylevel".into()],
            range: TimeRange::LastDay,
        };
        let doc = json!({
            "TimeGenerated": "2026-01-04T10:00:00Z",
            "Message": "started",
            "SeverityLevel": "Information",
            "OperationId": "abc",
            "TenantId": "should-not-show",
            "__ais_lane": "AppTraces",
            "Extra": "short"
        });

        let block = build_block(&doc, &spec, "AppTraces", "OperationId", "AppTraces#0");
        assert_eq!(block.label, "started");
        assert_eq!(block.key_value, "abc");
        assert!(block.at.is_some());

        let names: Vec<&str> = block.facts.iter().map(|(n, _)| n.as_str()).collect();
        // Chosen facts come first, spelled as the workspace spells them
        // rather than as the lowercased id they were matched by.
        assert_eq!(names.first(), Some(&"SeverityLevel"), "chosen facts come first");
        // The injected lane column and workspace boilerplate are not facts.
        assert!(!names.contains(&"__ais_lane"));
        assert!(!names.contains(&"TenantId"));
        // The key, time and label are already on the card in their own right.
        assert!(!names.contains(&"OperationId"));
        assert!(!names.contains(&"Message"));
    }

    #[test]
    fn a_card_without_a_label_column_falls_back_to_its_table() {
        let spec = TraceSpec {
            key: KeyCandidate {
                id: "k".into(),
                label: "OperationId".into(),
                bindings: vec![],
                missing: vec![],
                shared_values: 0,
                cross_named: false,
                id_shaped: false,
                well_known: false,
                avg_fill: 1.0,
                avg_distinct_ratio: 1.0,
                score: 1.0,
                evidence: vec![],
            },
            value: "abc".into(),
            time_field: String::new(),
            label_field: String::new(),
            fact_fields: vec![],
            range: TimeRange::LastHour,
        };
        let block = build_block(&json!({"OperationId": "abc"}), &spec, "MyApp_CL", "OperationId", "x");
        assert_eq!(block.label, "MyApp_CL");
        assert_eq!(block.at_text, "");
    }
}
