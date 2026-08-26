//! Domain-agnostic discovery of what makes a set of tables traceable.
//!
//! Nothing here knows about any particular schema. Given the sampled rows,
//! it works out three things a trace view needs:
//!
//!   * **a correlation key** — the column whose value ties the steps of one
//!     flow together,
//!   * **a time column** — what orders those steps,
//!   * **a step label** — what to call each step.
//!
//! The key is the hard one, and it is decided from the data rather than from
//! column names: columns in different tables that carry *the same values*
//! are treated as the same identifier even when they're spelled differently
//! (`OperationId` here, `correlationId_g` there). Names are a weak
//! tie-breaker only, so an unfamiliar custom table ranks on evidence alone.
//!
//! Log Analytics does change the balance from the Cosmos original in two
//! ways. Timestamps are *declared* rather than sniffed, so time detection is
//! nearly free. And the well-known OTel/App Insights correlation columns get
//! a named prior — worth something, but deliberately worth less than actual
//! evidence of linking, because the interesting workspaces are the ones
//! mixing App Insights with custom and diagnostic tables that follow no
//! convention at all.

use crate::services::schema::TableSchema;
use std::collections::{BTreeMap, BTreeSet};

/// Columns Log Analytics injects into every table. They are perfectly
/// filled and useless as keys, so they never become candidates.
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

/// The column every Log Analytics table is ingested with. It is always a
/// correct answer for the time role, so it is the default — but not an
/// automatic winner, since a domain timestamp usually describes the step
/// better than the moment it reached the workspace.
pub const INGESTION_TIME: &str = "TimeGenerated";

/// How many values two columns must share before they're taken to be the
/// same identifier. Two is enough to be well past coincidence for long
/// values.
const LINK_MIN_SHARED: usize = 2;
/// Values shorter than this are too collision-prone to link on — `"1"` and
/// `"ok"` appear everywhere and mean nothing.
const LINK_MIN_LEN: usize = 8;
/// A value appearing in more than this many columns is not an identifier,
/// it is boilerplate — a region name, a fixed version string. Skipping it
/// keeps the pair-counting below from degenerating on wide workspaces, and
/// costs nothing real: an id shared by thirty columns is not an id.
const LINK_MAX_FANOUT: usize = 32;

/// One table's participation in a key: which column carries it there.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub table: String,
    pub field: String,
    /// The KQL type the workspace declares for this column, when it declares
    /// one. `trace.rs` uses it to decide whether a value can be compared
    /// directly (indexed) or has to go through `tostring()` first.
    pub kind: String,
    pub seen_in: usize,
    pub sampled_rows: usize,
    pub distinct: usize,
}

/// A reason to trust — or distrust — a candidate, shown to the user so the
/// ranking is arguable rather than magic.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub good: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyCandidate {
    /// Stable identity of the group across rescans.
    pub id: String,
    pub label: String,
    pub bindings: Vec<Binding>,
    /// Tables where nothing in this group was sampled.
    pub missing: Vec<String>,
    /// Distinct values seen in more than one table — direct proof the column
    /// links rows rather than just existing in several places.
    pub shared_values: usize,
    /// The group spans more than one spelling, so only the values connect it.
    pub cross_named: bool,
    pub id_shaped: bool,
    pub well_known: bool,
    pub avg_fill: f32,
    pub avg_distinct_ratio: f32,
    pub score: f32,
    pub evidence: Vec<Evidence>,
}

/// A simpler candidate for the roles that don't need value-linking.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleCandidate {
    /// Lowercased column path; matched case-insensitively per table.
    pub id: String,
    pub label: String,
    pub tables: Vec<String>,
    pub note: String,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Insights {
    pub tables: Vec<String>,
    pub keys: Vec<KeyCandidate>,
    pub times: Vec<RoleCandidate>,
    pub labels: Vec<RoleCandidate>,
}

impl KeyCandidate {
    /// The column path carrying this key in a given table, if any.
    pub fn binding_for(&self, table: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.table == table)
    }
}

pub fn analyze(schemas: &[TableSchema]) -> Insights {
    Insights {
        tables: schemas.iter().map(TableSchema::path).collect(),
        keys: key_candidates(schemas),
        times: time_candidates(schemas),
        labels: label_candidates(schemas),
    }
}

/// Every distinct value sampled for a column, across all tables.
///
/// When a column drives the lane axis, this is the set of stages we know
/// exist — and therefore the only stages that can be reported as *not*
/// reached. It is exactly as complete as the sample was.
pub fn field_values(schemas: &[TableSchema], lower_path: &str) -> Vec<String> {
    if lower_path.is_empty() {
        return Vec::new();
    }
    let mut out: BTreeSet<&str> = BTreeSet::new();
    for schema in schemas {
        for field in &schema.fields {
            if field.name.to_lowercase() == lower_path {
                out.extend(field.values.iter().map(String::as_str));
            }
        }
    }
    out.into_iter().map(str::to_string).collect()
}

/// Every scalar column in the workspace, for pickers that need the full list
/// rather than a ranked subset — an error flag can live on any column,
/// however unremarkable, so this deliberately does no filtering beyond "has
/// a value you could compare against".
pub fn scalar_fields(schemas: &[TableSchema]) -> Vec<RoleCandidate> {
    let mut by_key: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();

    for schema in schemas {
        let path = schema.path();
        for field in &schema.fields {
            if is_system(&field.name) || !field.is_scalar() {
                continue;
            }
            let entry = by_key
                .entry(field.name.to_lowercase())
                .or_insert_with(|| (field.name.clone(), Vec::new()));
            entry.1.push(path.clone());
        }
    }

    by_key
        .into_iter()
        .map(|(id, (label, tables))| RoleCandidate {
            score: tables.len() as f32,
            note: format!("in {} table(s)", tables.len()),
            id,
            label,
            tables,
        })
        .collect()
}

fn is_system(name: &str) -> bool {
    SYSTEM_FIELDS.iter().any(|s| s.eq_ignore_ascii_case(name))
}

// ── Correlation keys ──────────────────────────────────────────────────────

struct Node<'a> {
    schema: usize,
    table: String,
    field: &'a str,
    kind: &'a str,
    name_key: String,
    seen_in: usize,
    sampled_rows: usize,
    distinct: usize,
    fill: f32,
    distinct_ratio: f32,
    /// Values long enough to be worth linking on.
    linkable: BTreeSet<&'a str>,
    id_shaped: bool,
    well_known: bool,
}

fn key_candidates(schemas: &[TableSchema]) -> Vec<KeyCandidate> {
    let all_paths: Vec<String> = schemas.iter().map(TableSchema::path).collect();
    let mut nodes: Vec<Node> = Vec::new();

    for (idx, schema) in schemas.iter().enumerate() {
        let path = schema.path();
        for field in &schema.fields {
            if is_system(&field.name) || !field.is_scalar() {
                continue;
            }
            let linkable: BTreeSet<&str> = field
                .values
                .iter()
                .filter(|v| v.len() >= LINK_MIN_LEN)
                .map(String::as_str)
                .collect();
            let id_like = field.values.iter().filter(|v| id_shaped(v)).count();
            nodes.push(Node {
                schema: idx,
                table: path.clone(),
                field: &field.name,
                kind: &field.kind,
                name_key: field.name.to_lowercase(),
                seen_in: field.seen_in,
                sampled_rows: schema.sampled_rows,
                distinct: field.distinct,
                fill: field.fill(schema.sampled_rows),
                distinct_ratio: field.distinct_ratio(),
                linkable,
                id_shaped: !field.values.is_empty() && id_like * 2 > field.values.len(),
                well_known: is_well_known(&field.name.to_lowercase()),
            });
        }
    }

    let mut dsu = Dsu::new(nodes.len());
    link_by_name(&nodes, &mut dsu);
    link_by_value(&nodes, &mut dsu);

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..nodes.len() {
        groups.entry(dsu.find(i)).or_default().push(i);
    }

    let mut candidates: Vec<KeyCandidate> = groups
        .into_values()
        .map(|members| build_candidate(&nodes, &members, &all_paths))
        .collect();

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.cmp(&b.label))
    });
    candidates
}

/// Same spelling in different tables is the same key, by assumption.
fn link_by_name(nodes: &[Node], dsu: &mut Dsu) {
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        by_name.entry(node.name_key.as_str()).or_default().push(i);
    }
    for members in by_name.values() {
        for pair in members.windows(2) {
            // Only ever link across tables. Two columns of one row sharing a
            // name cannot happen, but two paths inside a dynamic column can.
            if nodes[pair[0]].schema != nodes[pair[1]].schema {
                dsu.union(pair[0], pair[1]);
            }
        }
    }
}

/// Columns carrying the same values are the same key, whatever they're
/// called — the whole point of the module.
///
/// This is value-indexed rather than pairwise. The Cosmos original compared
/// every column against every other, which was fine for a handful of
/// containers; a Log Analytics workspace can present thousands of columns
/// (`AzureDiagnostics` alone carries hundreds), and the quadratic version
/// does not survive that. Inverting to value → columns touches only pairs
/// that actually share something.
fn link_by_value(nodes: &[Node], dsu: &mut Dsu) {
    let mut by_value: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        for value in &node.linkable {
            by_value.entry(value).or_default().push(i);
        }
    }

    // A pair must share `LINK_MIN_SHARED` values before it counts, so
    // co-occurrences are tallied first and unioned after.
    let mut pairs: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for holders in by_value.values() {
        if holders.len() < 2 || holders.len() > LINK_MAX_FANOUT {
            continue;
        }
        for a in 0..holders.len() {
            for b in (a + 1)..holders.len() {
                let (i, j) = (holders[a], holders[b]);
                // Two columns of one table sharing values (`OperationId` /
                // `ParentId`) are related, but they are not one key.
                if nodes[i].schema == nodes[j].schema {
                    continue;
                }
                *pairs.entry((i.min(j), i.max(j))).or_default() += 1;
            }
        }
    }

    for ((i, j), shared) in pairs {
        if shared >= LINK_MIN_SHARED {
            dsu.union(i, j);
        }
    }
}

fn build_candidate(nodes: &[Node], members: &[usize], all_paths: &[String]) -> KeyCandidate {
    // One binding per table: if a table contributes several columns to the
    // group, show the most selective one.
    let mut best_per_table: BTreeMap<&str, &Node> = BTreeMap::new();
    for &i in members {
        let node = &nodes[i];
        best_per_table
            .entry(node.table.as_str())
            .and_modify(|current| {
                if (node.distinct, node.seen_in) > (current.distinct, current.seen_in) {
                    *current = node;
                }
            })
            .or_insert(node);
    }

    // Values landing in more than one table are the linking evidence.
    let mut value_tables: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for node in best_per_table.values() {
        for value in &node.linkable {
            value_tables
                .entry(value)
                .or_default()
                .insert(node.table.as_str());
        }
    }
    let shared_values = value_tables.values().filter(|t| t.len() > 1).count();

    let bindings: Vec<Binding> = best_per_table
        .values()
        .map(|n| Binding {
            table: n.table.clone(),
            field: n.field.to_string(),
            kind: n.kind.to_string(),
            seen_in: n.seen_in,
            sampled_rows: n.sampled_rows,
            distinct: n.distinct,
        })
        .collect();

    let names: BTreeSet<&str> = best_per_table.values().map(|n| n.field).collect();
    let cross_named = names.len() > 1;
    let reach = bindings.len();
    let count = best_per_table.len().max(1) as f32;
    let avg_fill = best_per_table.values().map(|n| n.fill).sum::<f32>() / count;
    let avg_distinct_ratio = best_per_table
        .values()
        .map(|n| n.distinct_ratio)
        .sum::<f32>()
        / count;
    let id_shaped = best_per_table.values().filter(|n| n.id_shaped).count() * 2 > reach;
    let well_known = best_per_table.values().any(|n| n.well_known);
    let name_hint = best_per_table
        .values()
        .any(|n| name_suggests_identifier(&n.name_key));

    // Label: prefer the spelling used by the most tables, then the shortest,
    // so a group reads by its most common name.
    let mut spelling_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for node in best_per_table.values() {
        *spelling_counts.entry(node.field).or_default() += 1;
    }
    let label = spelling_counts
        .iter()
        .max_by_key(|(name, count)| (**count, std::cmp::Reverse(name.len())))
        .map(|(name, _)| name.to_string())
        .unwrap_or_default();

    let id = best_per_table
        .values()
        .map(|n| format!("{}\u{1}{}", n.table, n.field))
        .min()
        .unwrap_or_default();

    let mut score = 0.0f32;
    let mut evidence = Vec::new();

    // Reach: a key that only exists in one place cannot trace anything.
    score += 3.0 * (reach.min(6) as f32 - 1.0);
    if reach > 1 {
        evidence.push(Evidence {
            good: true,
            text: format!("present in {reach} tables"),
        });
    } else {
        evidence.push(Evidence {
            good: false,
            text: "only in one table — nothing to trace across".into(),
        });
    }

    // Shared values: the one piece of hard proof that it links.
    if shared_values > 0 {
        score += 4.0 + (shared_values.min(10) as f32) * 0.3;
        evidence.push(Evidence {
            good: true,
            text: format!("{shared_values} values seen in more than one table"),
        });
    } else if reach > 1 {
        evidence.push(Evidence {
            good: false,
            text: "no shared values in the sample — the link is unproven".into(),
        });
    }

    if cross_named {
        evidence.push(Evidence {
            good: true,
            text: format!("same values under different names: {}", join(&names)),
        });
    }

    // The convention prior. Worth less than proof of linking, deliberately:
    // a workspace whose custom tables spell it their own way is exactly the
    // case this app exists for, and it must not be outranked by a
    // conventional column that links nothing.
    if well_known {
        score += 2.5;
        evidence.push(Evidence {
            good: true,
            text: "a standard Azure Monitor correlation column".into(),
        });
    }

    if id_shaped {
        score += 1.5;
        evidence.push(Evidence {
            good: true,
            text: "values look like identifiers".into(),
        });
    }

    // Selectivity: a column with a handful of repeated values is a status,
    // not an identity.
    if avg_distinct_ratio < 0.15 {
        score -= 5.0;
        evidence.push(Evidence {
            good: false,
            text: format!(
                "only {:.0}% distinct values — looks like a status, not an id",
                avg_distinct_ratio * 100.0
            ),
        });
    }

    score += avg_fill * 2.0;
    if avg_fill < 0.5 {
        evidence.push(Evidence {
            good: false,
            text: format!("set on only {:.0}% of sampled rows", avg_fill * 100.0),
        });
    }

    // Weakest signal, deliberately: naming conventions are a hint, never the
    // reason a candidate wins.
    if name_hint && !well_known {
        score += 1.0;
        evidence.push(Evidence {
            good: true,
            text: "name follows a common id convention".into(),
        });
    }

    let tables: BTreeSet<&str> = bindings.iter().map(|b| b.table.as_str()).collect();
    let missing = all_paths
        .iter()
        .filter(|p| !tables.contains(p.as_str()))
        .cloned()
        .collect();

    KeyCandidate {
        id,
        label,
        bindings,
        missing,
        shared_values,
        cross_named,
        id_shaped,
        well_known,
        avg_fill,
        avg_distinct_ratio,
        score,
        evidence,
    }
}

// ── Time and label roles ──────────────────────────────────────────────────

/// Unlike the Cosmos original this barely has to guess: Log Analytics
/// declares column types, so a `datetime` column is a time column by
/// definition. Value-shape sniffing survives only for paths inside dynamic
/// columns, whose contents nothing declares.
fn time_candidates(schemas: &[TableSchema]) -> Vec<RoleCandidate> {
    let mut by_key: BTreeMap<String, (String, Vec<String>, bool)> = BTreeMap::new();

    for schema in schemas {
        let path = schema.path();
        for field in &schema.fields {
            let declared = field.is_declared_time();
            if !declared {
                if field.values.is_empty() {
                    continue;
                }
                let hits = field.values.iter().filter(|v| time_shaped(v)).count();
                if hits * 5 < field.values.len() * 3 {
                    continue; // fewer than 60% of values are time-shaped
                }
            }
            let entry = by_key
                .entry(field.name.to_lowercase())
                .or_insert_with(|| (field.name.clone(), Vec::new(), false));
            entry.1.push(path.clone());
            entry.2 |= declared;
        }
    }

    let mut out: Vec<RoleCandidate> = by_key
        .into_iter()
        .map(|(id, (label, tables, declared))| {
            let note = if id == INGESTION_TIME.to_lowercase() {
                "ingestion time — always present, but not your domain timestamp".to_string()
            } else if declared {
                format!("declared datetime, in {} table(s)", tables.len())
            } else {
                format!("timestamp-shaped, in {} table(s)", tables.len())
            };
            RoleCandidate {
                // `TimeGenerated` is in every table, so on reach alone it
                // would always win. Held back so a domain timestamp that
                // describes the step itself is offered first.
                score: if id == INGESTION_TIME.to_lowercase() {
                    0.5
                } else {
                    tables.len() as f32
                },
                note,
                id,
                label,
                tables,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.cmp(&b.label))
    });
    out
}

fn label_candidates(schemas: &[TableSchema]) -> Vec<RoleCandidate> {
    let mut by_key: BTreeMap<String, (String, Vec<String>, usize)> = BTreeMap::new();

    for schema in schemas {
        let path = schema.path();
        for field in &schema.fields {
            if is_system(&field.name)
                || field.is_declared_time()
                || !field.types.iter().any(|t| t == "string")
                || field.fill(schema.sampled_rows) < 0.5
            {
                continue;
            }
            // A useful step label is a small vocabulary describing what
            // happened, not a value unique to every row.
            if field.distinct < 2 || field.distinct > 25 {
                continue;
            }
            // Only hold repetition against a column once the sample is big
            // enough to expect it — in five rows even a real enum can come
            // out all-distinct.
            if field.seen_in >= 8 && field.distinct_ratio() > 0.8 {
                continue;
            }
            if field.values.iter().any(|v| v.len() > 60) {
                continue; // free text, not a label
            }
            if field.values.iter().filter(|v| id_shaped(v)).count() * 2 > field.values.len() {
                continue; // identifiers name nothing
            }
            let entry = by_key
                .entry(field.name.to_lowercase())
                .or_insert_with(|| (field.name.clone(), Vec::new(), 0));
            entry.1.push(path.clone());
            entry.2 = entry.2.max(field.distinct);
        }
    }

    let mut out: Vec<RoleCandidate> = by_key
        .into_iter()
        .map(|(id, (label, tables, distinct))| RoleCandidate {
            score: tables.len() as f32,
            note: format!("{distinct} distinct values, in {} table(s)", tables.len()),
            id,
            label,
            tables,
        })
        .collect();

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.cmp(&b.label))
    });
    out
}

// ── Shape tests ───────────────────────────────────────────────────────────

/// The correlation columns Azure Monitor and OpenTelemetry put in tables by
/// convention. Matching one is a real signal — but only a prior, since the
/// workspaces worth tracing are the ones where half the tables ignore it.
const WELL_KNOWN_KEYS: [&str; 6] = [
    "operationid",
    "operation_id",
    "parentid",
    "operation_parentid",
    "correlationid",
    "traceid",
];

fn is_well_known(lower: &str) -> bool {
    let leaf = lower.rsplit('.').next().unwrap_or(lower);
    // Diagnostic tables suffix by type: `correlationId_g`, `requestId_s`.
    let stem = leaf
        .strip_suffix("_g")
        .or_else(|| leaf.strip_suffix("_s"))
        .unwrap_or(leaf);
    WELL_KNOWN_KEYS.contains(&stem)
}

/// Whether a value looks like a machine-generated identifier: UUID, ULID,
/// hex digest, or similar. Deliberately shape-based, not format-specific.
fn id_shaped(v: &str) -> bool {
    let n = v.len();
    if n < 8 {
        return false;
    }
    let has_digit = v.chars().any(|c| c.is_ascii_digit());
    let hex_dashes = v.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let alnum = v.chars().all(|c| c.is_ascii_alphanumeric());
    (hex_dashes && has_digit && n >= 16) || (alnum && has_digit && n >= 12)
}

/// ISO-8601-ish dates, or epoch seconds/milliseconds in a plausible range.
fn time_shaped(v: &str) -> bool {
    let b = v.as_bytes();
    if b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[7] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
    {
        return true;
    }
    match v.parse::<i64>() {
        // ~2001-09-09 to ~2096 in seconds, same window in milliseconds.
        Ok(n) => {
            (1_000_000_000..=4_000_000_000).contains(&n)
                || (1_000_000_000_000..=4_000_000_000_000).contains(&n)
        }
        Err(_) => false,
    }
}

/// A weak, convention-based hint — never decisive on its own.
fn name_suggests_identifier(lower: &str) -> bool {
    const HINTS: [&str; 7] = [
        "correlation",
        "trace",
        "requestid",
        "conversation",
        "session",
        "flowid",
        "operation",
    ];
    HINTS.iter().any(|h| lower.contains(h))
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn join(names: &BTreeSet<&str>) -> String {
    names.iter().copied().collect::<Vec<_>>().join(", ")
}

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Dsu {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::schema::FieldInfo;

    fn field(name: &str, values: &[&str]) -> FieldInfo {
        let set: BTreeSet<String> = values.iter().map(|v| v.to_string()).collect();
        FieldInfo {
            name: name.into(),
            kind: String::new(),
            types: vec!["string".into()],
            seen_in: values.len(),
            distinct: set.len(),
            values: set,
        }
    }

    fn typed(name: &str, kind: &str, values: &[&str]) -> FieldInfo {
        FieldInfo {
            kind: kind.into(),
            ..field(name, values)
        }
    }

    fn table(name: &str, fields: Vec<FieldInfo>) -> TableSchema {
        let sampled = fields.iter().map(|f| f.seen_in).max().unwrap_or(0);
        TableSchema {
            table: name.into(),
            sampled_rows: sampled,
            rows_in_range: sampled,
            fields,
        }
    }

    fn uuids() -> Vec<&'static str> {
        vec![
            "9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f",
            "1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d",
            "2b3c4d5e-6f7a-8b9c-0d1e-2f3a4b5c6d7e",
            "3c4d5e6f-7a8b-9c0d-1e2f-3a4b5c6d7e8f",
        ]
    }

    /// The point of the whole module: an App Insights table and a custom
    /// table naming the same identifier differently are still recognised as
    /// one key, purely from the values.
    #[test]
    fn links_differently_named_columns_by_shared_values() {
        let ids = uuids();
        let schemas = vec![
            table(
                "AppRequests",
                vec![
                    field("OperationId", &ids),
                    field("Success", &["true", "true", "false", "false"]),
                ],
            ),
            table(
                "MyApp_CL",
                vec![
                    field("job_ref_g", &ids),
                    field("stage_s", &["created", "paid", "shipped", "paid"]),
                ],
            ),
        ];

        let insights = analyze(&schemas);
        let best = &insights.keys[0];

        assert!(best.cross_named, "expected the group to span both names");
        assert_eq!(best.shared_values, 4);
        assert_eq!(best.bindings.len(), 2);
        assert_eq!(
            best.binding_for("AppRequests").map(|b| b.field.as_str()),
            Some("OperationId")
        );
        assert_eq!(
            best.binding_for("MyApp_CL").map(|b| b.field.as_str()),
            Some("job_ref_g")
        );
    }

    /// An enum-ish column repeated across tables must not outrank a real
    /// identifier just because it is everywhere.
    #[test]
    fn low_cardinality_columns_rank_below_identifiers() {
        let ids = uuids();
        let schemas = vec![
            table(
                "AppRequests",
                vec![
                    field("OperationId", &ids),
                    field("Level", &["a", "a", "b", "b"]),
                ],
            ),
            table(
                "AppTraces",
                vec![
                    field("OperationId", &ids),
                    field("Level", &["a", "b", "b", "a"]),
                ],
            ),
        ];

        let insights = analyze(&schemas);
        let level_rank = insights.keys.iter().position(|c| c.label == "Level");
        assert_eq!(insights.keys[0].label, "OperationId");
        assert!(level_rank.is_some_and(|r| r > 0), "Level should not win");
    }

    /// Two columns inside one table that happen to share values are related,
    /// but they are not a single key.
    #[test]
    fn does_not_link_columns_within_one_table() {
        let ids = uuids();
        let schemas = vec![table(
            "AppDependencies",
            vec![field("OperationId", &ids), field("ParentId", &ids)],
        )];

        let insights = analyze(&schemas);
        assert!(
            insights.keys.iter().all(|c| c.bindings.len() == 1),
            "same-table columns must stay separate candidates"
        );
    }

    #[test]
    fn paths_inside_dynamic_columns_link_to_declared_columns() {
        let ids = uuids();
        let schemas = vec![
            table("AppRequests", vec![field("OperationId", &ids)]),
            table(
                "AzureDiagnostics",
                vec![field("Properties.OperationId", &ids)],
            ),
        ];

        let insights = analyze(&schemas);
        let best = &insights.keys[0];
        assert_eq!(best.bindings.len(), 2);
        assert_eq!(
            best.binding_for("AzureDiagnostics")
                .map(|b| b.field.as_str()),
            Some("Properties.OperationId")
        );
    }

    /// The convention prior helps, but a column that demonstrably links must
    /// still beat one that merely has the fashionable name.
    #[test]
    fn proof_of_linking_outranks_a_conventional_name() {
        let ids = uuids();
        let others = [
            "aaaaaaaa-1111-2222-3333-444444444444",
            "bbbbbbbb-1111-2222-3333-444444444444",
            "cccccccc-1111-2222-3333-444444444444",
            "dddddddd-1111-2222-3333-444444444444",
        ];
        let schemas = vec![
            table(
                "MyApp_CL",
                vec![field("job_ref_g", &ids), field("OperationId", &others)],
            ),
            table("Ingest_CL", vec![field("batch_ref_g", &ids)]),
        ];

        let insights = analyze(&schemas);
        assert_eq!(
            insights.keys[0].label, "job_ref_g",
            "a proven link must outrank an unlinked conventional name"
        );
        assert!(insights.keys[0].shared_values > 0);
    }

    #[test]
    fn a_standard_correlation_column_is_recognised_and_said_so() {
        let ids = uuids();
        let schemas = vec![
            table("AppRequests", vec![field("OperationId", &ids)]),
            table("AppTraces", vec![field("OperationId", &ids)]),
        ];
        let best = &analyze(&schemas).keys[0];
        assert!(best.well_known);
        assert!(
            best.evidence
                .iter()
                .any(|e| e.text.contains("standard Azure Monitor")),
            "the prior must be stated, not silently applied"
        );
    }

    /// Type-suffixed diagnostic columns are the same convention wearing a
    /// hat, and should be recognised through the suffix.
    #[test]
    fn type_suffixed_diagnostic_columns_count_as_well_known() {
        assert!(is_well_known("correlationid_g"));
        assert!(is_well_known("operationid_s"));
        assert!(is_well_known("properties.operationid"));
        assert!(!is_well_known("customerid_g"));
    }

    /// Declared types make the time role near-free: a datetime column is a
    /// time column even when the sample never showed a value.
    #[test]
    fn declared_datetime_columns_are_time_candidates_without_a_sample() {
        let schemas = vec![table(
            "AppRequests",
            vec![FieldInfo {
                kind: "datetime".into(),
                seen_in: 0,
                distinct: 0,
                values: BTreeSet::new(),
                ..field("EnqueuedAt", &[])
            }],
        )];
        let insights = analyze(&schemas);
        assert!(
            insights.times.iter().any(|c| c.label == "EnqueuedAt"),
            "a declared datetime needs no evidence to be offered"
        );
    }

    /// `TimeGenerated` is always right and always present, which is exactly
    /// why it must not crowd out the timestamp that describes the step.
    #[test]
    fn ingestion_time_is_offered_but_never_first() {
        let schemas = vec![
            table(
                "AppRequests",
                vec![
                    typed("TimeGenerated", "datetime", &["2026-01-04T10:00:00Z"]),
                    typed("EnqueuedAt", "datetime", &["2026-01-04T09:59:00Z"]),
                ],
            ),
            table(
                "AppTraces",
                vec![
                    typed("TimeGenerated", "datetime", &["2026-01-04T10:00:01Z"]),
                    typed("EnqueuedAt", "datetime", &["2026-01-04T09:59:30Z"]),
                ],
            ),
        ];
        let insights = analyze(&schemas);
        assert_eq!(insights.times[0].label, "EnqueuedAt");
        assert!(insights.times.iter().any(|c| c.id == "timegenerated"));
    }

    #[test]
    fn injected_system_columns_are_never_candidates() {
        let ids = uuids();
        let schemas = vec![
            table(
                "AppRequests",
                vec![field("TenantId", &ids), field("OperationId", &ids)],
            ),
            table(
                "AppTraces",
                vec![field("TenantId", &ids), field("OperationId", &ids)],
            ),
        ];
        let insights = analyze(&schemas);
        assert!(
            insights.keys.iter().all(|c| c.label != "TenantId"),
            "workspace-injected columns must never be offered as keys"
        );
        assert!(scalar_fields(&schemas).iter().all(|f| f.id != "tenantid"));
    }

    #[test]
    fn field_values_unions_across_tables_and_dedupes() {
        let schemas = vec![
            table(
                "AppTraces",
                vec![field("workflowName", &["Validate", "Invoice", "Validate"])],
            ),
            table(
                "MyApp_CL",
                vec![field("WorkflowName", &["Validate", "Archive"])],
            ),
        ];

        assert_eq!(
            field_values(&schemas, "workflowname"),
            vec!["Archive", "Invoice", "Validate"]
        );
        assert!(field_values(&schemas, "").is_empty());
        assert!(field_values(&schemas, "nosuchfield").is_empty());
    }

    /// The error-rule picker must offer numeric flags, which the ranked role
    /// lists filter out as too low-cardinality.
    #[test]
    fn scalar_fields_includes_numeric_flags_the_role_lists_reject() {
        let numeric = FieldInfo {
            name: "ResultCode".into(),
            kind: "int".into(),
            types: vec!["number".into()],
            seen_in: 20,
            distinct: 2,
            values: ["200", "500"].iter().map(|s| s.to_string()).collect(),
        };
        let schemas = vec![table(
            "AppRequests",
            vec![numeric, field("OperationId", &uuids())],
        )];

        let offered = scalar_fields(&schemas);
        let ids: Vec<&str> = offered.iter().map(|f| f.id.as_str()).collect();
        assert!(
            ids.contains(&"resultcode"),
            "numeric columns must be offerable as error flags, got {ids:?}"
        );
        assert_eq!(field_values(&schemas, "resultcode"), vec!["200", "500"]);
    }

    #[test]
    fn short_values_do_not_link_tables() {
        let schemas = vec![
            table("A_CL", vec![field("code", &["1", "2", "3", "4"])]),
            table("B_CL", vec![field("rank", &["1", "2", "3", "4"])]),
        ];

        let insights = analyze(&schemas);
        assert!(
            insights.keys.iter().all(|c| c.bindings.len() == 1),
            "short values are too collision-prone to link on"
        );
    }

    /// One shared value is coincidence; the threshold exists to require more.
    #[test]
    fn a_single_shared_value_does_not_link() {
        let schemas = vec![
            table(
                "A_CL",
                vec![field(
                    "left",
                    &["9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f", "unique-to-a-11111"],
                )],
            ),
            table(
                "B_CL",
                vec![field(
                    "right",
                    &["9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f", "unique-to-b-22222"],
                )],
            ),
        ];
        let insights = analyze(&schemas);
        assert!(
            insights.keys.iter().all(|c| c.bindings.len() == 1),
            "one shared value is not enough evidence to merge two columns"
        );
    }

    /// Boilerplate that turns up in dozens of columns is not an identifier,
    /// and must not drag the whole workspace into one giant group.
    ///
    /// Each table spells the column differently, so only the shared value
    /// could merge them — which is exactly the mechanism under test.
    #[test]
    fn values_shared_by_many_columns_do_not_link() {
        let boilerplate = "westeurope-prod-cluster-01";
        let schemas: Vec<TableSchema> = (0..LINK_MAX_FANOUT + 5)
            .map(|i| {
                table(
                    &format!("T{i}_CL"),
                    vec![field(&format!("region_{i}"), &[boilerplate, "x"])],
                )
            })
            .collect();

        let insights = analyze(&schemas);
        assert!(
            insights.keys.iter().all(|c| c.bindings.len() == 1),
            "a value appearing in every table links nothing"
        );
    }

    /// The same spelling in two tables is taken as the same key without
    /// needing value evidence — the other half of the linking rule, and the
    /// reason the fan-out cap above cannot be tested with a shared name.
    #[test]
    fn the_same_column_name_links_tables_on_its_own() {
        let schemas = vec![
            table(
                "A_CL",
                vec![field("job_ref", &["only-in-a-1111", "only-in-a-2222"])],
            ),
            table(
                "B_CL",
                vec![field("job_ref", &["only-in-b-1111", "only-in-b-2222"])],
            ),
        ];
        let best = &analyze(&schemas).keys[0];
        assert_eq!(best.bindings.len(), 2);
        // Named the same, but nothing proves they carry the same ids.
        assert_eq!(best.shared_values, 0);
        assert!(
            best.evidence
                .iter()
                .any(|e| !e.good && e.text.contains("unproven")),
            "an unproven link must say so"
        );
    }

    #[test]
    fn labels_are_detected_and_timestamps_excluded_from_them() {
        let schemas = vec![table(
            "AppTraces",
            vec![
                typed(
                    "TimeGenerated",
                    "datetime",
                    &[
                        "2026-01-04T10:00:00Z",
                        "2026-01-04T10:05:00Z",
                        "2026-01-05T09:00:00Z",
                    ],
                ),
                field("SeverityLevel", &["Information", "Warning", "Error"]),
            ],
        )];

        let insights = analyze(&schemas);
        assert!(insights.labels.iter().any(|c| c.label == "SeverityLevel"));
        assert!(
            insights.labels.iter().all(|c| c.label != "TimeGenerated"),
            "a timestamp is not a step label"
        );
    }
}
