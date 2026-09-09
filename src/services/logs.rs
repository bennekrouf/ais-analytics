//! The log stream, filtered — and every line traceable.
//!
//! This is the tab you reach for when you know roughly *when* but not
//! *which*: something went wrong around two o'clock, and the id you need is
//! in here somewhere. So the point of the view is not to read logs
//! comfortably — the portal does that — but to get from a line to the flow
//! it belongs to. Every row carries its correlation value for that reason.
//!
//! As everywhere else in this app, no column is named here. The table, the
//! timestamp, the message and the severity all arrive as discovered roles,
//! so a workspace whose log column is called `body` and whose severity is
//! `criticality` works exactly the same.

use crate::services::discover::{Insights, KeyCandidate};
use crate::services::loganalytics::{Client, TimeRange, column_ref, kql_string, table_ref};
use crate::services::schema::TableSchema;
use serde_json::Value;

/// Lines pulled back before the view stops being readable. A log tab that
/// tries to be exhaustive is a slow way to render the portal.
const MAX_LINES: usize = 300;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Spec {
    pub table: String,
    pub time_field: String,
    pub message_field: String,
    /// Empty when nothing here reads as a severity; the filter then hides.
    pub severity_field: String,
    pub key_field: String,
}

impl Spec {
    pub fn is_usable(&self) -> bool {
        !self.table.is_empty() && !self.message_field.is_empty()
    }
}

/// What the user narrowed to. Both parts are optional and compose.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Filter {
    pub text: String,
    pub severity: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Epoch milliseconds, when the timestamp parsed.
    pub at: Option<i64>,
    pub at_text: String,
    pub severity: String,
    pub message: String,
    /// The value to follow. Empty when the line carries none.
    pub key_value: String,
}

/// Picks the table and columns the log view should read.
///
/// "Where the key and free text meet, with the most rows behind it" — which
/// on a typical workspace is the trace table, and is deliberately allowed to
/// be the noisiest one. Unlike the signals view, volume here is the point.
pub fn propose(
    schemas: &[TableSchema],
    insights: &Insights,
    key: &KeyCandidate,
    time_id: &str,
) -> Spec {
    let bound: Vec<&str> = key.bindings.iter().map(|b| b.table.as_str()).collect();
    let message = insights.messages.first();

    let with_text: Vec<&str> = match message {
        Some(m) => bound
            .iter()
            .copied()
            .filter(|t| m.tables.iter().any(|mt| mt == t))
            .collect(),
        None => Vec::new(),
    };
    let Some(table) = with_text
        .iter()
        .copied()
        .max_by_key(|t| rows_of(schemas, t))
    else {
        return Spec::default();
    };

    let has = |name: &str| has_column(schemas, table, name);

    Spec {
        table: table.to_string(),
        time_field: if has(time_id) {
            time_id.to_string()
        } else {
            crate::services::discover::INGESTION_TIME.to_string()
        },
        message_field: message.map(|m| m.label.clone()).unwrap_or_default(),
        severity_field: insights
            .severities
            .iter()
            .find(|s| has(&s.label))
            .map(|s| s.label.clone())
            .unwrap_or_default(),
        key_field: key
            .binding_for(table)
            .map(|b| b.field.clone())
            .unwrap_or_default(),
    }
}

/// The severity values actually seen, for the filter to offer.
///
/// Read off the scan rather than queried: the sample already holds them, and
/// a round-trip to populate a dropdown is a round-trip the user waits for.
pub fn severities(schemas: &[TableSchema], spec: &Spec) -> Vec<String> {
    if spec.severity_field.is_empty() {
        return Vec::new();
    }
    schemas
        .iter()
        .filter(|s| s.table == spec.table)
        .flat_map(|s| s.fields.iter())
        .find(|f| f.name.eq_ignore_ascii_case(&spec.severity_field))
        .map(|f| f.values.iter().cloned().collect())
        .unwrap_or_default()
}

pub async fn load(
    client: &Client,
    workspace_id: &str,
    range: TimeRange,
    spec: &Spec,
    filter: &Filter,
) -> Result<Vec<Entry>, String> {
    if !spec.is_usable() {
        return Ok(Vec::new());
    }
    let rows = client
        .query(workspace_id, &query(spec, filter), range)
        .await?;
    Ok(rows.iter().map(build).collect())
}

fn query(spec: &Spec, filter: &Filter) -> String {
    let mut clauses = String::new();

    if !filter.text.trim().is_empty() {
        // `contains` rather than `has`: a user typing part of a token means
        // the substring, not the whole word.
        clauses.push_str(&format!(
            "| where tostring({}) contains {}\n",
            column_ref(&spec.message_field),
            kql_string(filter.text.trim())
        ));
    }
    if !filter.severity.is_empty() && !spec.severity_field.is_empty() {
        clauses.push_str(&format!(
            "| where tostring({}) =~ {}\n",
            column_ref(&spec.severity_field),
            kql_string(&filter.severity)
        ));
    }

    let severity = if spec.severity_field.is_empty() {
        "\x20   Severity = \"\",\n".to_string()
    } else {
        format!(
            "\x20   Severity = tostring({}),\n",
            column_ref(&spec.severity_field)
        )
    };
    let key = if spec.key_field.is_empty() {
        "\x20   Key = \"\"".to_string()
    } else {
        format!("\x20   Key = tostring({})", column_ref(&spec.key_field))
    };

    format!(
        "{}\n{clauses}| project\n\x20   At = {},\n{severity}\x20   Message = tostring({}),\n{key}\n\
         | order by At desc\n| take {MAX_LINES}",
        table_ref(&spec.table),
        column_ref(&spec.time_field),
        column_ref(&spec.message_field),
    )
}

fn rows_of(schemas: &[TableSchema], name: &str) -> usize {
    schemas
        .iter()
        .find(|s| s.table == name)
        .map(|s| s.rows_in_range)
        .unwrap_or_default()
}

fn has_column(schemas: &[TableSchema], table: &str, name: &str) -> bool {
    !name.is_empty()
        && schemas
            .iter()
            .any(|s| s.table == table && s.fields.iter().any(|f| f.name.eq_ignore_ascii_case(name)))
}

fn text_of(row: &Value, field: &str) -> String {
    match row.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn build(row: &Value) -> Entry {
    let raw = row.get("At");
    Entry {
        at: raw.and_then(crate::services::trace::parse_time),
        at_text: text_of(row, "At"),
        severity: text_of(row, "Severity"),
        message: text_of(row, "Message"),
        key_value: text_of(row, "Key"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> Spec {
        Spec {
            table: "AppTraces".into(),
            time_field: "TimeGenerated".into(),
            message_field: "Message".into(),
            severity_field: "SeverityLevel".into(),
            key_field: "OperationId".into(),
        }
    }

    /// The whole reason this tab exists: a line you cannot follow is a line
    /// the portal already showed you.
    #[test]
    fn every_line_carries_the_value_to_follow() {
        let kql = query(&spec(), &Filter::default());
        assert!(
            kql.contains("Key = tostring(['OperationId'])"),
            "got: {kql}"
        );
    }

    /// Both filters are optional and compose into the same query.
    #[test]
    fn filters_are_optional_and_stack() {
        let none = query(&spec(), &Filter::default());
        assert!(!none.contains("| where"), "got: {none}");

        let both = query(
            &spec(),
            &Filter {
                text: "timed out".into(),
                severity: "Error".into(),
            },
        );
        assert!(both.contains(r#"contains "timed out""#), "got: {both}");
        assert!(both.contains(r#"=~ "Error""#), "got: {both}");
    }

    /// Typing part of a token means the substring. `has` would silently drop
    /// every partial word the user tried.
    #[test]
    fn a_text_filter_matches_substrings_not_whole_words() {
        let kql = query(
            &spec(),
            &Filter {
                text: "counterpart".into(),
                ..Filter::default()
            },
        );
        assert!(kql.contains("contains"), "got: {kql}");
        assert!(
            !kql.contains("| where tostring(['Message']) has "),
            "got: {kql}"
        );
    }

    /// A workspace with no severity column still gets its log lines.
    #[test]
    fn a_missing_severity_leaves_the_column_blank_rather_than_failing() {
        let bare = Spec {
            severity_field: String::new(),
            ..spec()
        };
        let kql = query(&bare, &Filter::default());
        assert!(kql.contains(r#"Severity = """#), "got: {kql}");
        assert!(kql.contains("Message = tostring"), "got: {kql}");
    }

    /// A severity filter with nothing to filter on must not reach the query.
    #[test]
    fn a_severity_filter_is_dropped_when_the_table_has_no_severity() {
        let bare = Spec {
            severity_field: String::new(),
            ..spec()
        };
        let kql = query(
            &bare,
            &Filter {
                severity: "Error".into(),
                ..Filter::default()
            },
        );
        assert!(!kql.contains("=~"), "got: {kql}");
    }

    /// Search text is user input and reaches the query as a literal.
    #[test]
    fn a_hostile_search_cannot_escape_the_query() {
        let kql = query(
            &spec(),
            &Filter {
                text: r#"x" | take 1 //"#.into(),
                ..Filter::default()
            },
        );
        assert!(kql.contains(r#""x\" | take 1 //""#), "got: {kql}");
    }

    #[test]
    fn rows_become_entries() {
        let e = build(&json!({
            "At": "2026-09-08T11:00:00Z",
            "Severity": "Error",
            "Message": "Connection timed out",
            "Key": "82cd9fa7"
        }));
        assert_eq!(e.severity, "Error");
        assert_eq!(e.key_value, "82cd9fa7");
        assert!(e.at.is_some());
    }
}
