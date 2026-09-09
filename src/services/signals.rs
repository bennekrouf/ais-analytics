//! Rate, failures and latency over a window — the "is something wrong right
//! now" entry point.
//!
//! This is deliberately not a dashboard. Every row it draws carries a
//! correlation value, because the question it exists to answer is *which
//! run should I look at*, and the answer is a click into the trace view.
//! A number with no way through to the flow behind it would be a worse
//! version of the portal.
//!
//! Nothing here names a column. The table, the timestamp, the duration and
//! the label all arrive as discovered roles, and what counts as a failure
//! comes from the user's own error rules — the same ones the trace view
//! paints cards red with. That is what keeps this working on a workspace
//! that has never heard of Application Insights.

use crate::services::loganalytics::{Client, TimeRange, column_ref, kql_string, table_ref};
use crate::services::trace::ErrorRule;
use serde_json::Value;

/// Operations listed before the table stops being readable.
const MAX_OPERATIONS: usize = 50;

/// Which columns play which part. Every field is a discovered role rather
/// than a name this module knows.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Spec {
    pub table: String,
    pub time_field: String,
    /// Empty when the workspace records no duration. The views degrade to
    /// rate and failures rather than refusing to draw.
    pub duration_field: String,
    /// What to call each operation. Empty falls back to the table.
    pub label_field: String,
    /// Carried on every row so a click can hand it to the trace view.
    pub key_field: String,
    /// The sampling multiplier, when the telemetry has one. Counting rows
    /// where it exists under-reports exactly the busiest operations, which
    /// are the ones worth finding.
    pub weight_field: String,
    /// Domain knowledge, supplied by the user. No rules means no failure
    /// line — an honest empty rather than a guess.
    pub rules: Vec<ErrorRule>,
}

impl Spec {
    pub fn is_usable(&self) -> bool {
        !self.table.is_empty() && !self.time_field.is_empty()
    }

    /// Whether a failure count can be computed at all.
    pub fn knows_failure(&self) -> bool {
        !self.rules.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// Epoch milliseconds.
    pub at: i64,
    pub total: f64,
    pub failed: f64,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub label: String,
    pub calls: f64,
    pub failures: f64,
    pub p95: Option<f64>,
    /// One correlation value from this group, for the pivot into the trace.
    pub sample_key: String,
}

impl Operation {
    pub fn failure_pct(&self) -> f64 {
        if self.calls <= 0.0 {
            0.0
        } else {
            100.0 * self.failures / self.calls
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Signals {
    pub series: Vec<Point>,
    pub operations: Vec<Operation>,
    /// What the duration numbers are in, so the axis can say so.
    pub unit: String,
}

impl Signals {
    pub fn is_empty(&self) -> bool {
        self.series.is_empty() && self.operations.is_empty()
    }

    pub fn total(&self) -> f64 {
        self.series.iter().map(|p| p.total).sum()
    }

    pub fn failed(&self) -> f64 {
        self.series.iter().map(|p| p.failed).sum()
    }
}

/// Bucket width for a window: enough points to show a shape, few enough that
/// each one holds a meaningful sample.
pub fn bucket(range: TimeRange) -> &'static str {
    match range {
        TimeRange::LastHour => "1m",
        TimeRange::Last2Hours => "2m",
        TimeRange::Last4Hours => "5m",
        TimeRange::LastDay => "15m",
        TimeRange::LastWeek => "1h",
        TimeRange::Last30Days => "6h",
    }
}

/// Column names that mean "this row stands for N occurrences".
///
/// Named rather than assumed: the multiplier is a telemetry-SDK convention,
/// not something every workspace has, and reading a column that is not one
/// would inflate every count on the screen.
const WEIGHT_HINTS: [&str; 3] = ["itemcount", "samplecount", "_count"];

/// The same numbers, grouped by what the call went *to* rather than by what
/// it was.
///
/// Dependencies are the other half of the latency question: a slow operation
/// is usually a fast operation waiting on something. So this deliberately
/// reuses everything above — the query, the sampling weight, the error rules
/// — and changes only what the rows are grouped by. A separate service here
/// would be the same code with one identifier renamed.
///
/// The table is where the key and a *target* meet, which is the outbound
/// table rather than the inbound one the signals view picks.
pub fn propose_dependencies(
    schemas: &[crate::services::schema::TableSchema],
    insights: &crate::services::discover::Insights,
    key: &crate::services::discover::KeyCandidate,
    time_id: &str,
    rules: &[ErrorRule],
) -> Spec {
    let Some(target) = insights.targets.first() else {
        return Spec::default();
    };
    let bound: Vec<&str> = key.bindings.iter().map(|b| b.table.as_str()).collect();
    let pool: Vec<&str> = bound
        .into_iter()
        .filter(|t| target.tables.iter().any(|tt| tt == t))
        .collect();
    let Some(table) = pool.into_iter().max_by_key(|t| rows_in(schemas, t)) else {
        return Spec::default();
    };

    let base = spec_for(schemas, insights, key, table, time_id, rules);
    Spec {
        label_field: target.label.clone(),
        ..base
    }
}

fn rows_in(schemas: &[crate::services::schema::TableSchema], name: &str) -> usize {
    schemas
        .iter()
        .find(|s| s.table == name)
        .map(|s| s.rows_in_range)
        .unwrap_or_default()
}

/// Everything about a spec that does not depend on how rows are grouped.
fn spec_for(
    schemas: &[crate::services::schema::TableSchema],
    insights: &crate::services::discover::Insights,
    key: &crate::services::discover::KeyCandidate,
    table: &str,
    time_id: &str,
    rules: &[ErrorRule],
) -> Spec {
    let has_column = |name: &str| {
        !name.is_empty()
            && schemas.iter().any(|s| {
                s.table == table && s.fields.iter().any(|f| f.name.eq_ignore_ascii_case(name))
            })
    };
    let field_named = |hints: &[&str]| -> String {
        schemas
            .iter()
            .filter(|s| s.table == table)
            .flat_map(|s| s.fields.iter())
            .find(|f| {
                let lower = f.name.to_lowercase();
                f.is_number() && hints.iter().any(|h| lower.contains(h))
            })
            .map(|f| f.name.clone())
            .unwrap_or_default()
    };

    Spec {
        table: table.to_string(),
        time_field: if has_column(time_id) {
            time_id.to_string()
        } else {
            crate::services::discover::INGESTION_TIME.to_string()
        },
        duration_field: insights
            .durations
            .iter()
            .find(|d| has_column(&d.label))
            .map(|d| d.label.clone())
            .unwrap_or_default(),
        label_field: String::new(),
        key_field: key
            .binding_for(table)
            .map(|b| b.field.clone())
            .unwrap_or_default(),
        weight_field: field_named(&WEIGHT_HINTS),
        rules: rules.to_vec(),
    }
}

/// Picks the table and columns this view should read, from what discovery
/// already worked out.
///
/// The choice is "the table where the key and a duration meet, with the most
/// rows behind it". A workspace without any duration still gets a spec —
/// rate and failures are worth having on their own — and one without the key
/// gets nothing, because a row nobody can trace is not worth drawing.
pub fn propose(
    schemas: &[crate::services::schema::TableSchema],
    insights: &crate::services::discover::Insights,
    key: &crate::services::discover::KeyCandidate,
    time_id: &str,
    label_id: &str,
    rules: &[ErrorRule],
) -> Spec {
    let duration = insights.durations.first();
    let bound: Vec<&str> = key.bindings.iter().map(|b| b.table.as_str()).collect();

    // Prefer a table that has both roles; fall back to any the key reaches.
    let with_duration: Vec<&str> = match duration {
        Some(d) => bound
            .iter()
            .copied()
            .filter(|t| d.tables.iter().any(|dt| dt == t))
            .collect(),
        None => Vec::new(),
    };
    let pool = if with_duration.is_empty() {
        bound
    } else {
        with_duration
    };
    let Some(table) = pool.into_iter().max_by_key(|t| rows_in(schemas, t)) else {
        return Spec::default();
    };

    let base = spec_for(schemas, insights, key, table, time_id, rules);
    let has_label = schemas.iter().any(|s| {
        s.table == table
            && s.fields
                .iter()
                .any(|f| f.name.eq_ignore_ascii_case(label_id))
    });
    Spec {
        label_field: if has_label && !label_id.is_empty() {
            label_id.to_string()
        } else {
            String::new()
        },
        ..base
    }
}

pub async fn load(
    client: &Client,
    workspace_id: &str,
    range: TimeRange,
    spec: &Spec,
) -> Result<Signals, String> {
    if !spec.is_usable() {
        return Ok(Signals::default());
    }
    let series = client
        .query(workspace_id, &series_query(spec, bucket(range)), range)
        .await?;
    let operations = client
        .query(workspace_id, &operations_query(spec), range)
        .await?;

    Ok(Signals {
        series: series.iter().filter_map(build_point).collect(),
        operations: operations.iter().map(build_operation).collect(),
        unit: if spec.duration_field.is_empty() {
            String::new()
        } else {
            crate::services::discover::unit_of(&spec.duration_field).to_string()
        },
    })
}

/// `countif` over the user's error rules, OR'd together.
///
/// Rules are compared as text and case-insensitively, exactly as
/// [`trace::is_error`] does in memory — a `ResultCode` of `500` must match
/// whether the column is an int, a string, or a path inside a dynamic blob.
fn failure_expr(rules: &[ErrorRule]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let clauses: Vec<String> = rules
        .iter()
        .map(|rule| {
            format!(
                "tostring({}) =~ {}",
                column_ref(&rule.field),
                kql_string(&rule.value)
            )
        })
        .collect();
    Some(clauses.join(" or "))
}

/// How one row is counted. `sum(ItemCount)` where the telemetry samples,
/// plain `count()` where it does not.
fn weigh(spec: &Spec) -> String {
    if spec.weight_field.is_empty() {
        "count()".to_string()
    } else {
        format!("sum({})", column_ref(&spec.weight_field))
    }
}

fn failed_expr(spec: &Spec) -> String {
    match failure_expr(&spec.rules) {
        None => "0".to_string(),
        Some(pred) if spec.weight_field.is_empty() => format!("countif({pred})"),
        Some(pred) => format!("sumif({}, {pred})", column_ref(&spec.weight_field)),
    }
}

/// Percentile clauses, or nothing when the workspace records no duration.
fn percentiles(spec: &Spec) -> String {
    if spec.duration_field.is_empty() {
        return String::new();
    }
    let d = column_ref(&spec.duration_field);
    format!(
        ",\n\x20           P50 = percentile(todouble({d}), 50),\n\
         \x20           P95 = percentile(todouble({d}), 95),\n\
         \x20           P99 = percentile(todouble({d}), 99)"
    )
}

fn series_query(spec: &Spec, bucket: &str) -> String {
    // No `where TimeGenerated > ago(...)`: the window is the request's
    // `timespan`, the same as every other query in this app.
    format!(
        "{}\n\
         | summarize Total = {}, Failed = {}{}\n\
         \x20   by At = bin({}, {bucket})\n\
         | order by At asc",
        table_ref(&spec.table),
        weigh(spec),
        failed_expr(spec),
        percentiles(spec),
        column_ref(&spec.time_field),
    )
}

fn operations_query(spec: &Spec) -> String {
    let p95 = if spec.duration_field.is_empty() {
        String::new()
    } else {
        format!(
            ",\n\x20           P95 = percentile(todouble({}), 95)",
            column_ref(&spec.duration_field)
        )
    };
    // The label can be absent; grouping on the table name then yields the
    // single row that is still the honest answer.
    let group = if spec.label_field.is_empty() {
        format!("Label = {}", kql_string(&spec.table))
    } else {
        format!("Label = tostring({})", column_ref(&spec.label_field))
    };
    format!(
        "{}\n\
         | summarize Calls = {}, Failures = {}{}\n\
         \x20           , SampleKey = any(tostring({}))\n\
         \x20   by {group}\n\
         | order by Failures desc, Calls desc\n\
         | take {MAX_OPERATIONS}",
        table_ref(&spec.table),
        weigh(spec),
        failed_expr(spec),
        p95,
        column_ref(&spec.key_field),
    )
}

fn number(row: &Value, field: &str) -> Option<f64> {
    match row.get(field)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn text(row: &Value, field: &str) -> String {
    match row.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn build_point(row: &Value) -> Option<Point> {
    Some(Point {
        at: crate::services::trace::parse_time(row.get("At")?)?,
        total: number(row, "Total").unwrap_or_default(),
        failed: number(row, "Failed").unwrap_or_default(),
        p50: number(row, "P50"),
        p95: number(row, "P95"),
        p99: number(row, "P99"),
    })
}

fn build_operation(row: &Value) -> Operation {
    Operation {
        label: {
            let l = text(row, "Label");
            if l.is_empty() {
                "(unnamed)".to_string()
            } else {
                l
            }
        },
        calls: number(row, "Calls").unwrap_or_default(),
        failures: number(row, "Failures").unwrap_or_default(),
        p95: number(row, "P95"),
        sample_key: text(row, "SampleKey"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> Spec {
        Spec {
            table: "AppRequests".into(),
            time_field: "TimeGenerated".into(),
            duration_field: "DurationMs".into(),
            label_field: "Name".into(),
            key_field: "OperationId".into(),
            weight_field: "ItemCount".into(),
            rules: vec![ErrorRule {
                field: "success".into(),
                display: "Success".into(),
                value: "false".into(),
            }],
        }
    }

    /// Sampled telemetry stores one row per N occurrences. Counting rows
    /// under-reports the busiest operations, which are the ones worth
    /// finding, so the multiplier is used wherever it exists.
    #[test]
    fn sampling_is_accounted_for_when_the_table_carries_a_multiplier() {
        let kql = series_query(&spec(), "5m");
        assert!(kql.contains("Total = sum(['ItemCount'])"), "got: {kql}");
        assert!(kql.contains("sumif(['ItemCount']"), "got: {kql}");

        let plain = Spec {
            weight_field: String::new(),
            ..spec()
        };
        let kql = series_query(&plain, "5m");
        assert!(kql.contains("Total = count()"), "got: {kql}");
        assert!(kql.contains("countif("), "got: {kql}");
    }

    /// Failure is domain knowledge, never a guess. With no rules the line is
    /// flat zero rather than an invented definition of "bad".
    #[test]
    fn without_error_rules_nothing_is_called_a_failure() {
        let bare = Spec {
            rules: Vec::new(),
            ..spec()
        };
        assert!(!bare.knows_failure());
        assert!(series_query(&bare, "5m").contains("Failed = 0"));
    }

    /// A workspace that records no duration still gets rate and failures.
    #[test]
    fn a_missing_duration_drops_the_percentiles_rather_than_the_view() {
        let no_duration = Spec {
            duration_field: String::new(),
            ..spec()
        };
        let kql = series_query(&no_duration, "5m");
        assert!(!kql.contains("percentile"), "got: {kql}");
        assert!(kql.contains("Total = "), "got: {kql}");
    }

    /// Every operation row has to carry a way through to the flow behind it,
    /// or the view is just a prettier portal.
    #[test]
    fn every_operation_row_carries_a_value_to_trace() {
        let kql = operations_query(&spec());
        assert!(
            kql.contains("SampleKey = any(tostring(['OperationId']))"),
            "got: {kql}"
        );
    }

    /// Rule values reach the query as literals and must not be able to close
    /// the string and continue it.
    #[test]
    fn a_hostile_rule_value_cannot_escape_the_query() {
        let hostile = Spec {
            rules: vec![ErrorRule {
                field: "resultcode".into(),
                display: "ResultCode".into(),
                value: r#"x" or 1==1 //"#.into(),
            }],
            ..spec()
        };
        let kql = series_query(&hostile, "5m");
        // Escaped and still inside the literal: the injected pipe never
        // becomes an operator.
        assert!(kql.contains(r#""x\" or 1==1 //""#), "got: {kql}");
    }

    /// Comparison is textual and case-insensitive, so a rule written for an
    /// int column still matches when the value arrives as a string.
    #[test]
    fn rules_compare_as_text_so_the_declared_type_does_not_matter() {
        let kql = series_query(&spec(), "5m");
        assert!(
            kql.contains(r#"tostring(['success']) =~ "false""#),
            "got: {kql}"
        );
    }

    #[test]
    fn rows_become_points_and_operations() {
        let point = build_point(&json!({
            "At": "2026-09-08T11:00:00Z",
            "Total": 120, "Failed": 3, "P50": 12.5, "P95": 340.0, "P99": 900.0
        }))
        .expect("a row with a timestamp is a point");
        assert_eq!(point.total, 120.0);
        assert_eq!(point.p95, Some(340.0));

        let op = build_operation(&json!({
            "Label": "GET /orders", "Calls": 50, "Failures": 5,
            "P95": 210.0, "SampleKey": "abc123"
        }));
        assert_eq!(op.failure_pct(), 10.0);
        assert_eq!(op.sample_key, "abc123");
    }

    /// A group with no label is still a real group; it must not vanish.
    #[test]
    fn an_unlabelled_operation_is_named_rather_than_blank() {
        let op = build_operation(&json!({ "Calls": 1, "Failures": 0 }));
        assert_eq!(op.label, "(unnamed)");
    }

    mod proposing {
        use super::*;
        use crate::services::discover;
        use crate::services::schema::{FieldInfo, TableSchema};
        use std::collections::BTreeSet;

        fn num(name: &str) -> FieldInfo {
            FieldInfo {
                name: name.into(),
                kind: "real".into(),
                types: vec!["number".into()],
                seen_in: 1,
                distinct: 1,
                values: BTreeSet::new(),
            }
        }

        fn text_field(name: &str, values: &[&str]) -> FieldInfo {
            let set: BTreeSet<String> = values.iter().map(|v| v.to_string()).collect();
            FieldInfo {
                name: name.into(),
                kind: "string".into(),
                types: vec!["string".into()],
                seen_in: values.len(),
                distinct: set.len(),
                values: set,
            }
        }

        fn table(name: &str, rows: usize, fields: Vec<FieldInfo>) -> TableSchema {
            TableSchema {
                table: name.into(),
                sampled_rows: 20,
                rows_in_range: rows,
                unread: false,
                fields,
            }
        }

        fn ids() -> Vec<&'static str> {
            vec![
                "2430dcbe991462aa4a1f0b2c3d4e5f60",
                "859356a0819103bb5b2f1c3d4e5f6071",
                "b28b3eb5e214b9cc6c3f2d4e5f607182",
            ]
        }

        fn fixture() -> (Vec<TableSchema>, discover::Insights) {
            let schemas = vec![
                table(
                    "AppRequests",
                    500,
                    vec![
                        text_field("OperationId", &ids()),
                        num("DurationMs"),
                        num("ItemCount"),
                        text_field("Name", &["GET /orders"]),
                    ],
                ),
                table(
                    "AppTraces",
                    9000,
                    vec![
                        text_field("OperationId", &ids()),
                        text_field("Message", &["hi"]),
                    ],
                ),
            ];
            let insights = discover::analyze(&schemas);
            (schemas, insights)
        }

        /// The busiest table is not the right one. `AppTraces` has twenty
        /// times the rows, but no duration — the view would lose latency
        /// entirely by following volume alone.
        #[test]
        fn the_table_where_the_key_and_a_duration_meet_wins_over_the_busiest() {
            let (schemas, insights) = fixture();
            let key = insights.keys.first().expect("a key was found");
            let spec = propose(&schemas, &insights, key, "TimeGenerated", "Name", &[]);

            assert_eq!(spec.table, "AppRequests");
            assert_eq!(spec.duration_field, "DurationMs");
            assert_eq!(spec.key_field, "OperationId");
        }

        /// Sampled telemetry names its multiplier; unsampled data has none.
        /// Reading a column that is not one would inflate every count drawn.
        #[test]
        fn the_sampling_multiplier_is_found_by_name_not_assumed() {
            let (schemas, insights) = fixture();
            let key = insights.keys.first().expect("a key was found");
            let spec = propose(&schemas, &insights, key, "TimeGenerated", "Name", &[]);
            assert_eq!(spec.weight_field, "ItemCount");

            let plain = vec![table(
                "Orders_CL",
                10,
                vec![text_field("job_ref", &ids()), num("elapsed_ms")],
            )];
            let insights = discover::analyze(&plain);
            let key = insights.keys.first().expect("a key was found");
            let spec = propose(&plain, &insights, key, "TimeGenerated", "", &[]);
            assert_eq!(spec.weight_field, "", "no multiplier means plain counting");
            assert_eq!(spec.duration_field, "elapsed_ms");
        }

        /// A workspace with no duration anywhere still gets rate and
        /// failures rather than an empty screen.
        #[test]
        fn a_workspace_without_any_duration_still_gets_a_usable_spec() {
            let schemas = vec![table(
                "AppTraces",
                100,
                vec![
                    text_field("OperationId", &ids()),
                    text_field("Message", &["hi"]),
                ],
            )];
            let insights = discover::analyze(&schemas);
            let key = insights.keys.first().expect("a key was found");
            let spec = propose(&schemas, &insights, key, "TimeGenerated", "", &[]);

            assert!(spec.is_usable());
            assert_eq!(spec.duration_field, "");
        }

        /// The dependency view is the same query grouped differently, and
        /// it must land on the *outbound* table rather than the inbound one
        /// the signals view picks.
        #[test]
        fn dependencies_group_by_target_on_the_table_that_has_one() {
            let schemas = vec![
                table(
                    "AppRequests",
                    500,
                    vec![text_field("OperationId", &ids()), num("DurationMs")],
                ),
                table(
                    "AppDependencies",
                    300,
                    vec![
                        text_field("OperationId", &ids()),
                        num("DurationMs"),
                        text_field("Target", &["blob.core.windows.net", "api.example"]),
                    ],
                ),
            ];
            let insights = discover::analyze(&schemas);
            let key = insights.keys.first().expect("a key was found");

            let deps = propose_dependencies(&schemas, &insights, key, "TimeGenerated", &[]);
            assert_eq!(deps.table, "AppDependencies");
            assert_eq!(deps.label_field, "Target");
            assert_eq!(deps.duration_field, "DurationMs");

            // And the signals view still picks the busier inbound table.
            let sig = propose(&schemas, &insights, key, "TimeGenerated", "", &[]);
            assert_eq!(sig.table, "AppRequests");
        }

        /// No target anywhere means no dependency view, rather than a view
        /// grouped on something that is not a dependency.
        #[test]
        fn without_a_target_there_is_no_dependency_spec() {
            let (schemas, insights) = fixture();
            let key = insights.keys.first().expect("a key was found");
            let deps = propose_dependencies(&schemas, &insights, key, "TimeGenerated", &[]);
            assert!(!deps.is_usable());
        }

        /// A timestamp the chosen table does not have would make every query
        /// fail; ingestion time is always there and always correct.
        #[test]
        fn a_time_column_the_table_lacks_falls_back_to_ingestion_time() {
            let (schemas, insights) = fixture();
            let key = insights.keys.first().expect("a key was found");
            let spec = propose(&schemas, &insights, key, "EnqueuedAt", "Name", &[]);
            assert_eq!(spec.time_field, "TimeGenerated");
        }
    }
}
