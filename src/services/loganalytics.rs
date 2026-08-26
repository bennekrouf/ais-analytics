//! Data-plane Log Analytics access.
//!
//! There is no official Rust SDK for Azure Monitor Query — `azure_core` and
//! `azure_identity` are GA, but the Monitor data plane is not among the
//! published crates. So this is a hand-rolled client over the REST API. It
//! stays deliberately small: authenticate, POST a KQL query, and turn the
//! column-oriented response into row objects the rest of the app can treat
//! like documents.
//!
//! Auth reuses the same `az login` session as the ARM calls in `az.rs`, via
//! `DeveloperToolsCredential` — no keys, no separate sign-in.
//!
//! The one genuinely sharp edge here is that the query API has **no
//! parameter binding**. Cosmos had `Query::with_parameter`; KQL over REST
//! does not, so any user-supplied value has to be escaped into the query
//! text by `let_literal`. That function is the security boundary of this
//! module and is tested accordingly.

use azure_core::credentials::TokenCredential;
use azure_identity::DeveloperToolsCredential;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

/// The audience for Log Analytics data-plane tokens. Note this is
/// `api.loganalytics.io` even though requests go to `.azure.com`.
const SCOPE: &str = "https://api.loganalytics.io/.default";
const ENDPOINT: &str = "https://api.loganalytics.azure.com/v1";

/// Ceiling on rows pulled back from any one query. The service allows far
/// more (500k), but nothing in this app can usefully display it, and an
/// accidental unbounded query on a busy workspace should cost seconds, not
/// minutes.
pub const MAX_ROWS: usize = 5_000;

// ── Time range ────────────────────────────────────────────────────────────

/// How far back to look.
///
/// Unlike Cosmos, every Log Analytics query is bounded by a timespan — there
/// is no "just query everything". That makes the range a first-class part of
/// every request rather than an optional filter, so it is modelled as such
/// and threaded through the scan, the type-ahead, and the trace alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeRange {
    LastHour,
    Last2Hours,
    Last4Hours,
    LastDay,
    LastWeek,
    Last30Days,
}

impl Default for TimeRange {
    fn default() -> Self {
        TimeRange::LastDay
    }
}

impl TimeRange {
    /// ISO-8601 duration, the form the `timespan` body field takes.
    pub fn iso(self) -> &'static str {
        match self {
            TimeRange::LastHour => "PT1H",
            TimeRange::Last2Hours => "PT2H",
            TimeRange::Last4Hours => "PT4H",
            TimeRange::LastDay => "P1D",
            TimeRange::LastWeek => "P7D",
            TimeRange::Last30Days => "P30D",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TimeRange::LastHour => "Last hour",
            TimeRange::Last2Hours => "Last 2 hours",
            TimeRange::Last4Hours => "Last 4 hours",
            TimeRange::LastDay => "Last 24 hours",
            TimeRange::LastWeek => "Last 7 days",
            TimeRange::Last30Days => "Last 30 days",
        }
    }

    pub fn all() -> [TimeRange; 6] {
        [
            TimeRange::LastHour,
            TimeRange::Last2Hours,
            TimeRange::Last4Hours,
            TimeRange::LastDay,
            TimeRange::LastWeek,
            TimeRange::Last30Days,
        ]
    }
}

// ── Table metadata ────────────────────────────────────────────────────────

/// A column as the workspace itself declares it.
///
/// This is the headline difference from Cosmos: the schema is published, not
/// guessed. `kind` here is the KQL type (`string`, `datetime`, `dynamic`, …)
/// rather than a type inferred from sampled values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
}

impl ColumnMeta {
    /// Dynamic columns hold nested JSON — App Insights `Properties`, custom
    /// dimensions, and the like. They are the only place a Log Analytics row
    /// is not flat, so they are the only place path-flattening still matters.
    pub fn is_dynamic(&self) -> bool {
        self.kind == "dynamic"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableMeta {
    pub name: String,
    pub columns: Vec<ColumnMeta>,
}

// ── Client ────────────────────────────────────────────────────────────────

/// One client for a whole run of work.
///
/// Building this resolves credentials, which shells out to `az` on a cache
/// miss — hundreds of milliseconds at best. Build it once and thread it
/// through, exactly as the Cosmos client was.
pub struct Client {
    http: reqwest::Client,
    credential: Arc<dyn TokenCredential>,
}

impl Client {
    pub fn connect() -> Result<Client, String> {
        let credential = DeveloperToolsCredential::new(None)
            .map_err(|e| format!("failed to build credential: {e}"))?;
        let http = reqwest::Client::builder()
            // A query the service will kill at 10 minutes should not hang the
            // app for longer than that.
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Client {
            http,
            credential: credential as Arc<_>,
        })
    }

    async fn bearer(&self) -> Result<String, String> {
        let token = self
            .credential
            .get_token(&[SCOPE], None)
            .await
            .map_err(|e| format!("could not get an Azure token ({e}) — is `az login` current?"))?;
        Ok(token.token.secret().to_string())
    }

    /// Runs a KQL query and returns the primary result table as row objects.
    ///
    /// The API answers column-oriented (`{columns:[…], rows:[[…]]}`); every
    /// consumer in this app wants documents, so the zip happens here once
    /// rather than at each call site.
    pub async fn query(
        &self,
        workspace_id: &str,
        kql: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, String> {
        let token = self.bearer().await?;
        let url = format!("{ENDPOINT}/workspaces/{workspace_id}/query");

        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&serde_json::json!({
                "query": kql,
                "timespan": range.iso(),
            }))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        let status = response.status();
        // Throttling carries a server-supplied backoff; surfacing it is more
        // useful than a bare 429.
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = response
            .text()
            .await
            .map_err(|e| format!("could not read response: {e}"))?;

        if !status.is_success() {
            return Err(describe_failure(status, &body, retry_after.as_deref()));
        }

        let parsed: Value =
            serde_json::from_str(&body).map_err(|e| format!("malformed response: {e}"))?;

        // A 200 can still carry a partial failure alongside partial data.
        if let Some(error) = parsed.get("error") {
            return Err(api_error_message(error));
        }

        // The service will happily return far more than anything here can
        // display; truncating is cheaper than rendering it.
        let mut rows = rows_of(&parsed);
        rows.truncate(MAX_ROWS);
        Ok(rows)
    }

    /// Every table in the workspace, with its declared columns.
    ///
    /// This replaces the sample-and-guess scan the Cosmos version needed. It
    /// is authoritative: a table that has no correlation column genuinely
    /// does not have one, rather than merely not having shown it in twenty
    /// sampled documents.
    pub async fn tables(&self, workspace_id: &str) -> Result<Vec<TableMeta>, String> {
        let token = self.bearer().await?;
        let url = format!("{ENDPOINT}/workspaces/{workspace_id}/metadata");

        let response = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| format!("could not read response: {e}"))?;
        if !status.is_success() {
            return Err(describe_failure(status, &body, None));
        }

        let parsed: Value =
            serde_json::from_str(&body).map_err(|e| format!("malformed metadata: {e}"))?;
        Ok(tables_of(&parsed))
    }
}

// ── Response shaping ──────────────────────────────────────────────────────

/// Zips the primary result table's columns against its rows.
///
/// `PrimaryResult` is the conventional name; when it is absent (some
/// responses name the single table differently) the first table is used, so
/// a rename upstream degrades to the obvious behaviour rather than to zero
/// rows.
fn rows_of(body: &Value) -> Vec<Value> {
    let tables = body.get("tables").and_then(Value::as_array);
    let Some(tables) = tables else {
        return Vec::new();
    };
    let table = tables
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("PrimaryResult"))
        .or_else(|| tables.first());
    let Some(table) = table else {
        return Vec::new();
    };

    let names: Vec<&str> = table
        .get("columns")
        .and_then(Value::as_array)
        .map(|cols| {
            cols.iter()
                .map(|c| c.get("name").and_then(Value::as_str).unwrap_or(""))
                .collect()
        })
        .unwrap_or_default();

    table
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let cells = row.as_array().map(Vec::as_slice).unwrap_or(&[]);
                    let mut object = Map::new();
                    for (name, cell) in names.iter().zip(cells) {
                        if name.is_empty() || cell.is_null() {
                            continue;
                        }
                        object.insert((*name).to_string(), unwrap_dynamic(cell));
                    }
                    Value::Object(object)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Dynamic columns sometimes arrive as a JSON *string* holding JSON rather
/// than as a nested object. Parsing it here means `Properties.foo` is a real
/// path everywhere downstream, instead of working on some tables and not
/// others.
fn unwrap_dynamic(cell: &Value) -> Value {
    let Value::String(text) = cell else {
        return cell.clone();
    };
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return cell.clone();
    }
    serde_json::from_str(text).unwrap_or_else(|_| cell.clone())
}

fn tables_of(body: &Value) -> Vec<TableMeta> {
    body.get("tables")
        .and_then(Value::as_array)
        .map(|tables| {
            tables
                .iter()
                .filter_map(|t| {
                    let name = t.get("name").and_then(Value::as_str)?;
                    let columns = t
                        .get("columns")
                        .and_then(Value::as_array)
                        .map(|cols| {
                            cols.iter()
                                .filter_map(|c| {
                                    Some(ColumnMeta {
                                        name: c.get("name").and_then(Value::as_str)?.to_string(),
                                        kind: c
                                            .get("type")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default()
                                            .to_string(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(TableMeta {
                        name: name.to_string(),
                        columns,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Errors ────────────────────────────────────────────────────────────────

/// Turns a failed response into something a user can act on.
///
/// The three that actually happen in practice — no data-plane role, KQL the
/// service rejected, and throttling — each get their own wording, because
/// the remedy for each is completely different.
fn describe_failure(status: reqwest::StatusCode, body: &str, retry_after: Option<&str>) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").map(api_error_message))
        .unwrap_or_else(|| body.trim().chars().take(300).collect());

    match status.as_u16() {
        401 => format!("not authenticated ({detail}) — try `az login` again"),
        403 => format!(
            "access denied ({detail}) — the signed-in user needs the \
             Log Analytics Reader role on this workspace"
        ),
        400 => format!("the workspace rejected the query: {detail}"),
        404 => format!("workspace not found ({detail}) — check the workspace id"),
        429 => match retry_after {
            Some(secs) => format!("throttled by Azure — retry in {secs}s"),
            None => format!("throttled by Azure ({detail})"),
        },
        504 => "the query timed out — narrow the time range and try again".to_string(),
        other => format!("query failed ({other}): {detail}"),
    }
}

/// Flattens the API's nested `{code, message, innererror:{…}}` into one line.
/// The innermost message is nearly always the useful one.
fn api_error_message(error: &Value) -> String {
    let mut current = error;
    let mut best = String::new();
    loop {
        if let Some(message) = current.get("message").and_then(Value::as_str) {
            if !message.is_empty() {
                best = message.to_string();
            }
        }
        match current
            .get("innererror")
            .or_else(|| current.get("innerError"))
        {
            Some(inner) => current = inner,
            None => break,
        }
    }
    if best.is_empty() {
        "unknown error".to_string()
    } else {
        best
    }
}

// ── KQL construction ──────────────────────────────────────────────────────

/// Binds a user-supplied value to a KQL identifier.
///
/// This exists because the query API has no parameter binding: the only way
/// to get a value into a query is to write it into the text. Every
/// user-supplied string in this app goes through here, so the escaping is
/// the thing standing between a pasted correlation id and query injection.
///
/// KQL string literals take C-style backslash escapes, so escaping the
/// backslash and the quote is sufficient; control characters are escaped too
/// rather than emitted raw into the query text.
pub fn let_literal(name: &str, value: &str) -> String {
    format!("let {name} = {};", kql_string(value))
}

/// A value as a quoted, escaped KQL string literal.
pub fn kql_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Anything else non-printable would otherwise land in the query
            // text verbatim; drop it rather than guess an escape.
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A column reference safe to paste into a query.
///
/// Table and column names come from workspace metadata rather than from the
/// user, but they routinely contain characters that are not bare identifiers
/// (`Properties_s`, `duration (ms)`), so they are always bracket-quoted. A
/// dotted path indexes into a dynamic column: `Properties.correlationId`
/// becomes `['Properties']['correlationId']`.
pub fn column_ref(path: &str) -> String {
    path.split('.')
        .map(|part| format!("['{}']", part.replace('\\', "\\\\").replace('\'', "\\'")))
        .collect::<Vec<_>>()
        .concat()
}

/// A table name safe to paste into a query.
pub fn table_ref(name: &str) -> String {
    format!("['{}']", name.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn column_oriented_responses_become_row_objects() {
        let body = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    {"name": "TimeGenerated", "type": "datetime"},
                    {"name": "Message", "type": "string"},
                    {"name": "Missing", "type": "string"}
                ],
                "rows": [
                    ["2026-01-04T10:00:00Z", "started", null],
                    ["2026-01-04T10:00:01Z", "finished", "here"]
                ]
            }]
        });

        let rows = rows_of(&body);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["Message"], json!("started"));
        // A null cell is absent rather than present-and-null, so the
        // fill-rate maths downstream counts it as "not set".
        assert!(rows[0].get("Missing").is_none());
        assert_eq!(rows[1]["Missing"], json!("here"));
    }

    #[test]
    fn dynamic_columns_arriving_as_json_text_are_parsed() {
        let body = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [{"name": "Properties", "type": "dynamic"}],
                "rows": [[r#"{"correlationId":"abc-123"}"#]]
            }]
        });

        let rows = rows_of(&body);
        // Must be a real nested object, so `Properties.correlationId`
        // resolves the same way it would on a table that sent an object.
        assert_eq!(rows[0]["Properties"]["correlationId"], json!("abc-123"));
    }

    #[test]
    fn strings_that_merely_look_like_json_are_left_alone() {
        let body = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [{"name": "Message", "type": "string"}],
                "rows": [["{not actually json"]]
            }]
        });
        assert_eq!(rows_of(&body)[0]["Message"], json!("{not actually json"));
    }

    #[test]
    fn a_named_result_table_is_preferred_over_position() {
        let body = json!({
            "tables": [
                {"name": "Other", "columns": [{"name":"a","type":"string"}], "rows": [["wrong"]]},
                {"name": "PrimaryResult", "columns": [{"name":"a","type":"string"}], "rows": [["right"]]}
            ]
        });
        assert_eq!(rows_of(&body)[0]["a"], json!("right"));
    }

    /// The security boundary of this module: no user-supplied text may end
    /// the literal and start being read as query syntax.
    #[test]
    fn quotes_and_backslashes_cannot_escape_a_literal() {
        assert_eq!(kql_string("abc"), r#""abc""#);
        assert_eq!(kql_string(r#"a"b"#), r#""a\"b""#);
        assert_eq!(kql_string(r"a\b"), r#""a\\b""#);
        // The obvious injection: close the string, then append an operator.
        assert_eq!(
            kql_string(r#"x" | where 1==1 | take 1 //"#),
            r#""x\" | where 1==1 | take 1 //""#
        );
        // A trailing backslash must not escape the closing quote.
        assert_eq!(kql_string(r"trailing\"), r#""trailing\\""#);
        assert_eq!(kql_string("line\nbreak"), r#""line\nbreak""#);
    }

    #[test]
    fn let_bindings_are_escaped_statements() {
        assert_eq!(let_literal("v", "abc"), r#"let v = "abc";"#);
        assert_eq!(let_literal("v", r#"a"b"#), r#"let v = "a\"b";"#);
    }

    #[test]
    fn identifiers_are_bracket_quoted_and_paths_index_dynamics() {
        assert_eq!(column_ref("OperationId"), "['OperationId']");
        assert_eq!(
            column_ref("Properties.correlationId"),
            "['Properties']['correlationId']"
        );
        assert_eq!(column_ref("duration (ms)"), "['duration (ms)']");
        assert_eq!(table_ref("MyApp_CL"), "['MyApp_CL']");
        assert_eq!(table_ref("odd'name"), r"['odd\'name']");
    }

    #[test]
    fn metadata_becomes_typed_tables() {
        let body = json!({
            "tables": [{
                "name": "AppTraces",
                "columns": [
                    {"name": "TimeGenerated", "type": "datetime"},
                    {"name": "Properties", "type": "dynamic"},
                    {"name": "Message", "type": "string"}
                ]
            }]
        });

        let tables = tables_of(&body);
        assert_eq!(tables.len(), 1);
        let column = |name: &str| {
            tables[0]
                .columns
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("no column {name}"))
        };
        assert_eq!(column("TimeGenerated").kind, "datetime");
        assert!(column("Properties").is_dynamic());
        assert!(!column("Message").is_dynamic());
    }

    #[test]
    fn failures_explain_the_remedy_not_just_the_code() {
        let denied = json!({"error": {"code": "InsufficientAccessError",
                                      "message": "does not have access"}})
        .to_string();
        let text = describe_failure(reqwest::StatusCode::FORBIDDEN, &denied, None);
        assert!(text.contains("Log Analytics Reader"), "got: {text}");

        let throttled = describe_failure(reqwest::StatusCode::TOO_MANY_REQUESTS, "{}", Some("42"));
        assert!(throttled.contains("42"), "got: {throttled}");
    }

    #[test]
    fn the_innermost_error_message_wins() {
        let error = json!({
            "code": "BadArgumentError",
            "message": "outer",
            "innererror": {
                "code": "SyntaxError",
                "message": "the innermost and only useful message"
            }
        });
        assert_eq!(
            api_error_message(&error),
            "the innermost and only useful message"
        );
    }
}
