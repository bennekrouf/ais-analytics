//! What a workspace's tables actually contain.
//!
//! Log Analytics publishes a schema, which is the headline difference from
//! the Cosmos version of this app: column names and types are read from the
//! `metadata` endpoint rather than guessed from a sample. But metadata has
//! no *values*, and `discover.rs` links correlation keys by value, not by
//! name — so a sample is still needed on top of the declared schema.
//!
//! The sample is one query per batch of tables rather than one per table:
//! KQL can union and group server-side, so the whole scan is a handful of
//! round-trips instead of one per entity.

use crate::services::loganalytics::{Client, ColumnMeta, TableMeta, TimeRange, table_ref};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const SAMPLE_SIZE: usize = 20;
/// How far to descend into dynamic columns. App Insights buries the
/// interesting identifiers one or two levels inside `Properties`, and a
/// tracer that only looked at declared columns would miss every one.
const MAX_DEPTH: usize = 3;
/// Ceiling on retained values per field, so a wide table can't blow up memory.
const VALUE_CAP: usize = 256;
/// Tables unioned per query. Bounds the response size on workspaces holding
/// both hundreds of tables and very wide ones like `AzureDiagnostics`.
const TABLES_PER_QUERY: usize = 25;

/// Column added by `union withsource=`; it names the table, and is not part
/// of any table's own schema.
const SOURCE_COLUMN: &str = "SourceTable";

/// A field observed in the sampled rows of a table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldInfo {
    /// Dotted path — `OperationId`, or `Properties.correlationId` when it
    /// lives inside a dynamic column.
    pub name: String,
    /// The type the workspace declares for this column, when it is a
    /// declared column. Paths inside a dynamic column have no declared type,
    /// so this is empty for them.
    #[serde(default)]
    pub kind: String,
    /// JSON value types actually seen. A dynamic column's contents are not
    /// declared anywhere, so for those this is the only type information.
    pub types: Vec<String>,
    pub seen_in: usize,
    /// Distinct scalar values among the sampled rows.
    pub distinct: usize,
    /// The scalar values themselves, capped at `VALUE_CAP`.
    pub values: BTreeSet<String>,
}

impl FieldInfo {
    /// Fraction of sampled rows carrying this field.
    pub fn fill(&self, sampled_rows: usize) -> f32 {
        if sampled_rows == 0 {
            0.0
        } else {
            self.seen_in as f32 / sampled_rows as f32
        }
    }

    /// How selective the field is: 1.0 means a different value in every row
    /// it appears in, near 0.0 means a handful of repeated values.
    pub fn distinct_ratio(&self) -> f32 {
        if self.seen_in == 0 {
            0.0
        } else {
            self.distinct as f32 / self.seen_in as f32
        }
    }

    pub fn is_scalar(&self) -> bool {
        self.types.iter().any(|t| t == "string" || t == "number")
    }

    /// Whether the workspace declares this column as a timestamp.
    ///
    /// This is the part the Cosmos version had to infer from the shape of
    /// sampled values. Here it is simply stated, so a datetime column with
    /// an unlucky sample is still recognised.
    pub fn is_declared_time(&self) -> bool {
        self.kind == "datetime"
    }
}

/// One table, as declared plus as sampled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableSchema {
    pub table: String,
    pub sampled_rows: usize,
    /// Rows seen in the scanned window. Zero means the table exists in the
    /// workspace schema but held nothing in range.
    #[serde(default)]
    pub rows_in_range: usize,
    pub fields: Vec<FieldInfo>,
}

impl TableSchema {
    /// The lane identity. Log Analytics has one flat namespace, so unlike
    /// the Cosmos `database/container` this is just the table name — but
    /// callers still go through `path()` so the view layer stays unaware.
    pub fn path(&self) -> String {
        self.table.clone()
    }
}

/// Lists the tables, then samples the ones with data in range.
///
/// Tables that exist in the workspace schema but hold nothing in the window
/// are kept, with `rows_in_range: 0`. That distinction matters downstream:
/// an empty table with the correlation column is "nothing arrived yet",
/// which is a different statement from "not on this path".
pub async fn scan(
    client: &Client,
    workspace_id: &str,
    range: TimeRange,
) -> Result<Vec<TableSchema>, String> {
    let declared = client.tables(workspace_id).await?;
    if declared.is_empty() {
        return Ok(Vec::new());
    }

    let by_name: BTreeMap<&str, &TableMeta> =
        declared.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut out: Vec<TableSchema> = Vec::new();

    for chunk in declared.chunks(TABLES_PER_QUERY) {
        let names: Vec<&str> = chunk.iter().map(|t| t.name.as_str()).collect();
        // A single failing chunk shouldn't lose the rest of the workspace —
        // one retired table or one Basic-tier table that rejects `union` is
        // not a reason to show the user nothing.
        let Ok(rows) = client
            .query(workspace_id, &sample_query(&names), range)
            .await
        else {
            continue;
        };

        for row in rows {
            let Some(table) = row.get(SOURCE_COLUMN).and_then(Value::as_str) else {
                continue;
            };
            let rows_in_range =
                row.get("Rows").and_then(Value::as_u64).unwrap_or_default() as usize;
            let sample = row
                .get("Sample")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let meta = by_name.get(table).copied();
            out.push(summarise(table, rows_in_range, &sample, meta));
        }
    }

    // Tables the sample query returned nothing for: they exist, they are
    // just empty in this window. Carry them with their declared columns so
    // the view can still say whether they are on the path.
    let sampled: BTreeSet<String> = out.iter().map(|s| s.table.clone()).collect();
    for meta in &declared {
        if !sampled.contains(&meta.name) {
            out.push(summarise(&meta.name, 0, &[], Some(meta)));
        }
    }

    out.sort_by(|a, b| {
        b.rows_in_range
            .cmp(&a.rows_in_range)
            .then(a.table.cmp(&b.table))
    });
    Ok(out)
}

/// Counts and samples a batch of tables in one round-trip.
///
/// `isfuzzy=true` is what makes a batch safe: a table listed in metadata
/// that can no longer be queried is skipped rather than failing the whole
/// union. `make_list(pack_all(), n)` returns each sampled row as an object,
/// which is the shape the flattener below wants.
fn sample_query(tables: &[&str]) -> String {
    let refs = tables
        .iter()
        .map(|t| table_ref(t))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "union withsource={SOURCE_COLUMN} isfuzzy=true {refs}\n\
         | summarize Rows = count(), Sample = make_list(pack_all(), {SAMPLE_SIZE}) \
         by {SOURCE_COLUMN}"
    )
}

/// Folds sampled rows into per-field statistics, then annotates each field
/// with the type the workspace declares for it.
fn summarise(
    table: &str,
    rows_in_range: usize,
    sample: &[Value],
    meta: Option<&TableMeta>,
) -> TableSchema {
    let mut observed: BTreeMap<String, FieldInfo> = BTreeMap::new();
    let mut sampled = 0usize;

    for row in sample.iter().take(SAMPLE_SIZE) {
        sampled += 1;
        let mut leaves = Vec::new();
        flatten("", row, 0, &mut leaves);
        for (path, value) in leaves {
            // `union withsource=` injects this; it is the same for every row
            // of a table and would rank as a perfectly-filled constant.
            if path == SOURCE_COLUMN {
                continue;
            }
            let entry = observed.entry(path.clone()).or_insert_with(|| FieldInfo {
                name: path,
                kind: String::new(),
                types: Vec::new(),
                seen_in: 0,
                distinct: 0,
                values: BTreeSet::new(),
            });
            let ty = json_type_name(&value);
            if !entry.types.contains(&ty) {
                entry.types.push(ty);
            }
            entry.seen_in += 1;
            if let Some(scalar) = scalar_repr(&value) {
                if entry.values.len() < VALUE_CAP && entry.values.insert(scalar) {
                    entry.distinct += 1;
                }
            }
        }
    }

    // Declared columns the sample never showed a value for still belong in
    // the schema: they are why a lane can be "awaiting" rather than "off
    // path". They carry no values, so they can never win a key candidacy.
    if let Some(meta) = meta {
        for column in &meta.columns {
            if column.name == SOURCE_COLUMN {
                continue;
            }
            let entry = observed
                .entry(column.name.clone())
                .or_insert_with(|| FieldInfo {
                    name: column.name.clone(),
                    kind: String::new(),
                    types: Vec::new(),
                    seen_in: 0,
                    distinct: 0,
                    values: BTreeSet::new(),
                });
            entry.kind = column.kind.clone();
            if entry.types.is_empty() {
                entry.types.push(declared_json_type(column));
            }
        }
    }

    let mut fields: Vec<FieldInfo> = observed.into_values().collect();
    fields.sort_by(|a, b| b.seen_in.cmp(&a.seen_in).then(a.name.cmp(&b.name)));

    TableSchema {
        table: table.to_string(),
        sampled_rows: sampled,
        rows_in_range,
        fields,
    }
}

/// The JSON shape a declared KQL type arrives as, for columns the sample
/// never populated.
fn declared_json_type(column: &ColumnMeta) -> String {
    match column.kind.as_str() {
        "int" | "long" | "real" | "decimal" => "number",
        "bool" => "bool",
        _ if column.is_dynamic() => "object",
        // `datetime`, `guid`, `string`, `timespan` all arrive as text.
        _ => "string",
    }
    .to_string()
}

/// Walks a row into `(dotted path, value)` pairs. Nested objects — which in
/// Log Analytics means the contents of dynamic columns — are recorded in
/// their own right *and* descended into, up to `MAX_DEPTH`. Arrays are
/// recorded but not descended into: an element index is not a stable path.
fn flatten(prefix: &str, value: &Value, depth: usize, out: &mut Vec<(String, Value)>) {
    let Value::Object(map) = value else { return };
    for (key, child) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match child {
            Value::Object(_) if depth + 1 < MAX_DEPTH => {
                out.push((path.clone(), Value::Object(serde_json::Map::new())));
                flatten(&path, child, depth + 1, out);
            }
            other => out.push((path, other.clone())),
        }
    }
}

/// The comparable form of a scalar value, or `None` for shapes that can't
/// serve as an identifier or a label.
fn scalar_repr(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn json_type_name(v: &Value) -> String {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Case-insensitive column lookup, for assertions.
    fn field<'a>(schema: &'a TableSchema, lower: &str) -> &'a FieldInfo {
        schema
            .fields
            .iter()
            .find(|f| f.name.to_lowercase() == lower)
            .unwrap_or_else(|| panic!("no field {lower}"))
    }

    fn meta(name: &str, columns: &[(&str, &str)]) -> TableMeta {
        TableMeta {
            name: name.into(),
            columns: columns
                .iter()
                .map(|(n, k)| ColumnMeta {
                    name: (*n).into(),
                    kind: (*k).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn every_table_is_named_and_fuzzed_in_the_sample_query() {
        let kql = sample_query(&["AppTraces", "MyApp_CL"]);
        assert!(kql.contains("['AppTraces']"));
        assert!(kql.contains("['MyApp_CL']"));
        // Without isfuzzy a single unqueryable table fails the whole batch.
        assert!(kql.contains("isfuzzy=true"));
        assert!(kql.contains("make_list(pack_all(), 20)"));
    }

    #[test]
    fn dynamic_columns_are_flattened_into_dotted_paths() {
        let sample = vec![json!({
            "TimeGenerated": "2026-01-04T10:00:00Z",
            "Properties": {"correlationId": "abc-123", "nested": {"deep": "v"}}
        })];
        let schema = summarise("AppTraces", 1, &sample, None);
        let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"Properties.correlationId"), "got {names:?}");
        assert!(names.contains(&"Properties.nested.deep"), "got {names:?}");
    }

    #[test]
    fn the_union_source_column_is_not_treated_as_a_field() {
        let sample = vec![json!({"SourceTable": "AppTraces", "Message": "hello"})];
        let schema = summarise("AppTraces", 1, &sample, None);
        assert!(
            schema.fields.iter().all(|f| f.name != SOURCE_COLUMN),
            "the injected source column must not become a candidate field"
        );
    }

    #[test]
    fn declared_types_annotate_sampled_fields() {
        let sample = vec![json!({"TimeGenerated": "2026-01-04T10:00:00Z", "OperationId": "a"})];
        let schema = summarise(
            "AppTraces",
            1,
            &sample,
            Some(&meta(
                "AppTraces",
                &[("TimeGenerated", "datetime"), ("OperationId", "string")],
            )),
        );
        assert!(field(&schema, "timegenerated").is_declared_time());
        assert!(!field(&schema, "operationid").is_declared_time());
    }

    /// A column the sample never populated still has to exist in the schema:
    /// it is the difference between "nothing arrived here yet" and "this
    /// table was never on the path".
    #[test]
    fn declared_columns_survive_an_empty_sample() {
        let schema = summarise(
            "AppRequests",
            0,
            &[],
            Some(&meta(
                "AppRequests",
                &[("OperationId", "string"), ("TimeGenerated", "datetime")],
            )),
        );
        assert_eq!(schema.sampled_rows, 0);
        assert_eq!(schema.rows_in_range, 0);
        let field = field(&schema, "operationid");
        assert_eq!(field.seen_in, 0);
        assert!(field.values.is_empty());
        // Present in the schema, but with no evidence behind it.
        assert_eq!(field.fill(0), 0.0);
    }

    #[test]
    fn fill_and_selectivity_read_off_the_sample() {
        let sample = vec![
            json!({"OperationId": "a", "Level": "Info"}),
            json!({"OperationId": "b", "Level": "Info"}),
            json!({"Level": "Error"}),
        ];
        let schema = summarise("AppTraces", 3, &sample, None);
        let op = field(&schema, "operationid");
        assert_eq!(op.seen_in, 2);
        assert!((op.fill(3) - 0.666).abs() < 0.01);
        assert_eq!(op.distinct_ratio(), 1.0);

        let level = field(&schema, "level");
        assert_eq!(level.seen_in, 3);
        assert_eq!(level.fill(3), 1.0);
        // Two distinct values across three rows — a status, not an identity.
        assert!(level.distinct_ratio() < 0.7);
    }

    #[test]
    fn a_log_analytics_lane_is_just_the_table_name() {
        let schema = summarise("MyApp_CL", 0, &[], None);
        assert_eq!(schema.path(), "MyApp_CL");
    }
}
