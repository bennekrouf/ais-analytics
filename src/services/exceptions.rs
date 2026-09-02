//! Recurring exceptions, and what they usually mean.
//!
//! A one-off exception is noise. The same exception firing on a steady
//! cadence is a stuck listener, a dropped connection, or config that never
//! resolved — something that will keep failing until someone changes
//! something. This module finds the second kind and says what it looks like.
//!
//! Two things here are deliberately not what a first pass would do:
//!
//! * **Counts come from `sum(ItemCount)`, not `count()`.** Application
//!   Insights samples: at the default settings a busy app sends one row per
//!   N occurrences and records N in `ItemCount`. Counting rows would
//!   under-report exactly the loudest problems, which are the ones worth
//!   finding.
//! * **Cadence is measured from the timestamps, not inferred from
//!   min/max/count.** Those three numbers give an average interval, and an
//!   average cannot tell a listener retrying every 90s from a burst of
//!   forty failures during one deploy. Since "fires on a fixed interval" is
//!   the whole signal, the actual spacing has to be looked at.

use crate::services::loganalytics::{Client, TimeRange};
use serde_json::Value;

/// Groups pulled back before the list stops being readable.
const MAX_GROUPS: usize = 100;
/// Timestamps sampled per group for the cadence test. Enough to characterise
/// spacing without dragging the whole table back.
const MAX_TIMES: usize = 200;
/// Default floor for "recurring". Two could be coincidence; above this a
/// pattern is a pattern.
pub const DEFAULT_MIN_COUNT: u64 = 3;
/// Shortest gap that can plausibly be a retry timer. Anything faster is a hot
/// loop or a burst inside one bad moment — evenly spaced, but not the thing
/// this view is looking for.
const MIN_RETRY_SECS: f64 = 5.0;

/// The table Application Insights writes exceptions to on a workspace-based
/// component. A workspace with no App Insights simply will not have it.
pub const TABLE: &str = "AppExceptions";

/// The column the correlation ids on a group come from. The trace pivot needs
/// it too: the ids only mean anything against the key that carries them.
pub const CORRELATION_FIELD: &str = "OperationId";

/// Why a group is worth looking at. Kept apart from the raw message because
/// the message is what the framework said, and this is what it means.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    pub kind: HintKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintKind {
    /// A known signature with a known cause.
    Cause,
    /// Evidence about how it is firing.
    Cadence,
    /// Evidence that several symptoms are one problem.
    Shared,
}

/// How regularly a group fires.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cadence {
    /// Typical gap between occurrences, in seconds.
    pub median_secs: f64,
    /// Spread of the gaps relative to the median. Near zero means a timer.
    pub spread: f64,
    /// Regular enough to be something retrying rather than something failing.
    pub regular: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub exception_type: String,
    pub outer_message: String,
    /// Estimated true occurrences, sampling accounted for.
    pub count: u64,
    /// Rows actually stored. Lower than `count` when sampling is on.
    pub events: u64,
    /// Epoch milliseconds.
    pub first: Option<i64>,
    pub last: Option<i64>,
    pub cadence: Option<Cadence>,
    /// The apps or functions it was seen under.
    pub roles: Vec<String>,
    /// The operations (function / workflow names) it was seen under.
    pub operations: Vec<String>,
    /// Correlation ids, so a group can be opened in the trace view.
    pub correlation_ids: Vec<String>,
    pub hints: Vec<Hint>,
}

impl Group {
    /// Stable across refreshes, so an expanded row stays expanded.
    pub fn id(&self) -> String {
        format!("{}\u{1}{}", self.exception_type, self.outer_message)
    }

    /// True when sampling means the stored rows understate the problem.
    pub fn sampled(&self) -> bool {
        self.count > self.events
    }
}

/// Finds exception groups that fired more than `min_count` times in `range`.
pub async fn recurring(
    client: &Client,
    workspace_id: &str,
    range: TimeRange,
    min_count: u64,
) -> Result<Vec<Group>, String> {
    let rows = client.query(workspace_id, &query(min_count), range).await?;
    let mut groups: Vec<Group> = rows.iter().map(build).collect();
    // The service ordered by count; re-sort so anything on a fixed cadence
    // floats up regardless of volume. A listener retrying every two minutes
    // matters more than a noisier one-off spike.
    groups.sort_by(|a, b| {
        let regular = |g: &Group| g.cadence.is_some_and(|c| c.regular);
        regular(b).cmp(&regular(a)).then(b.count.cmp(&a.count))
    });
    Ok(groups)
}

/// Whether the workspace has an exceptions table at all.
///
/// Worth asking separately: "no Application Insights here" and "Application
/// Insights with nothing failing" are very different answers, and an empty
/// result cannot tell them apart. The scan already knows every table name,
/// so this is a lookup rather than another round-trip.
pub fn has_table_named(names: &[String]) -> bool {
    names.iter().any(|n| n == TABLE)
}

fn query(min_count: u64) -> String {
    // No `where TimeGenerated > ago(...)`: the window is the request's
    // `timespan`, the same as every other query in this app.
    format!(
        "{TABLE}\n\
         | summarize Count = sum(ItemCount), Events = count(),\n\
         \x20           First = min(TimeGenerated), Last = max(TimeGenerated),\n\
         \x20           Times = make_list(TimeGenerated, {MAX_TIMES}),\n\
         \x20           Roles = make_set(AppRoleName, 8),\n\
         \x20           Operations = make_set(OperationName, 8),\n\
         \x20           Correlations = make_set({CORRELATION_FIELD}, 5)\n\
         \x20   by ExceptionType, OuterMessage\n\
         | where Count > {min_count}\n\
         | order by Count desc\n\
         | take {MAX_GROUPS}"
    )
}

fn build(row: &Value) -> Group {
    let text = |k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let num = |k: &str| row.get(k).and_then(Value::as_u64).unwrap_or_default();
    let list = |k: &str| {
        row.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let time = |k: &str| row.get(k).and_then(crate::services::trace::parse_time);

    let times: Vec<i64> = row
        .get("Times")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(crate::services::trace::parse_time)
                .collect()
        })
        .unwrap_or_default();

    let mut group = Group {
        exception_type: text("ExceptionType"),
        outer_message: text("OuterMessage"),
        count: num("Count"),
        events: num("Events"),
        first: time("First"),
        last: time("Last"),
        cadence: cadence(&times),
        roles: list("Roles"),
        operations: list("Operations"),
        correlation_ids: list("Correlations"),
        hints: Vec::new(),
    };
    group.hints = classify(&group);
    group
}

/// How regularly a set of occurrences fires.
///
/// Spread is the median absolute deviation of the gaps over the median gap —
/// a scale-free measure that a couple of outliers cannot wreck, unlike a
/// standard deviation. A timer retrying on a schedule lands near zero; a
/// burst during one bad deploy does not.
pub fn cadence(times: &[i64]) -> Option<Cadence> {
    // Four occurrences give three gaps: the fewest that can show a pattern
    // rather than an accident.
    if times.len() < 4 {
        return None;
    }
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    let gaps: Vec<f64> = sorted
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64 / 1000.0)
        .collect();

    let median_secs = median(&gaps);
    if median_secs <= 0.0 {
        return None;
    }
    let deviations: Vec<f64> = gaps.iter().map(|g| (g - median_secs).abs()).collect();
    let spread = median(&deviations) / median_secs;

    Some(Cadence {
        median_secs,
        spread,
        // Two conditions, because evenness alone is not the signal. A quarter
        // of the median is loose enough for real-world timer jitter and tight
        // enough to exclude anything ragged — but a tight burst is also
        // perfectly even, so a floor is needed as well. Nothing retrying on a
        // schedule fires faster than every few seconds; below that it is a hot
        // loop or one bad moment, neither of which is what this view is for.
        regular: spread < 0.25 && median_secs >= MIN_RETRY_SECS,
    })
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    if v.len() % 2 == 0 {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

/// Turns a group into the things worth saying about it.
///
/// Signature matching is deliberately narrow. A hint that is right most of
/// the time is worse than no hint, because it sends someone to the wrong
/// place with confidence.
pub fn classify(group: &Group) -> Vec<Hint> {
    let mut hints = Vec::new();
    let message = group.outer_message.to_lowercase();
    let kind = group.exception_type.as_str();

    // A trigger that cannot start never processes anything, so this is
    // usually a total outage of one path rather than a degraded one.
    if message.contains("unable to start") {
        hints.push(Hint {
            kind: HintKind::Cause,
            text: "A trigger or listener failed to initialise — usually a bad \
                   connection string or config that never resolved. Nothing on \
                   this path is running."
                .into(),
        });
    }

    // The framework reports the symptom (it could not parse the string) and
    // never the cause (the string was empty because a reference failed).
    if kind == "System.ArgumentException" && message.contains("dbconnectionoptions") {
        hints.push(Hint {
            kind: HintKind::Cause,
            text: "Malformed or empty SQL connection string — almost always an \
                   unresolved Key Vault reference rather than a bad literal. \
                   Check the app's Key Vault references below."
                .into(),
        });
    }

    // The strongest signal in the whole view: the same failure under several
    // names is one broken thing, not several.
    let names = group.roles.len().max(group.operations.len());
    if names > 1 {
        let where_ = if group.roles.len() > 1 {
            format!("{} apps", group.roles.len())
        } else {
            format!("{} operations", group.operations.len())
        };
        hints.push(Hint {
            kind: HintKind::Shared,
            text: format!(
                "Same failure across {where_} — one shared cause (a single app \
                 setting or connection), not {names} separate bugs."
            ),
        });
    }

    if let Some(c) = group.cadence {
        if c.regular {
            hints.push(Hint {
                kind: HintKind::Cadence,
                text: format!(
                    "Fires every ~{} on a fixed interval — something is retrying \
                     on a timer. That is a stuck listener or a dropped \
                     connection, not a transient.",
                    human_gap(c.median_secs)
                ),
            });
        }
    }

    hints
}

/// "90s", "2m 30s", "1h 5m" — the gap as someone would say it.
pub fn human_gap(secs: f64) -> String {
    let s = secs.round() as i64;
    match s {
        s if s < 90 => format!("{s}s"),
        s if s < 3600 => {
            let (m, r) = (s / 60, s % 60);
            if r == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {r}s")
            }
        }
        s => {
            let (h, m) = (s / 3600, (s % 3600) / 60);
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn group(kind: &str, message: &str) -> Group {
        Group {
            exception_type: kind.into(),
            outer_message: message.into(),
            count: 10,
            events: 10,
            first: None,
            last: None,
            cadence: None,
            roles: vec!["fn-orders".into()],
            operations: vec!["ProcessOrder".into()],
            correlation_ids: vec![],
            hints: vec![],
        }
    }

    #[test]
    fn the_query_counts_occurrences_not_stored_rows() {
        let kql = query(3);
        // count() would under-report every sampled app, which is exactly the
        // set of apps whose exceptions matter most.
        assert!(kql.contains("Count = sum(ItemCount)"), "got: {kql}");
        assert!(kql.contains("Events = count()"));
        assert!(kql.contains("by ExceptionType, OuterMessage"));
        assert!(kql.contains("| where Count > 3"));
        // Timestamps have to come back for the cadence test to mean anything.
        assert!(kql.contains("Times = make_list(TimeGenerated, 200)"));
        // The window is the request timespan, not a hardcoded ago().
        assert!(!kql.contains("ago("), "the range belongs to the caller");
    }

    #[test]
    fn a_failed_listener_is_named_as_such() {
        let hints = classify(&group(
            "System.InvalidOperationException",
            "The listener for function 'X' was unable to start.",
        ));
        assert_eq!(hints[0].kind, HintKind::Cause);
        assert!(hints[0].text.contains("failed to initialise"));
    }

    /// The framework says it could not parse a connection string; the cause
    /// is nearly always that the string arrived empty.
    #[test]
    fn a_broken_connection_string_points_at_key_vault() {
        let hints = classify(&group(
            "System.ArgumentException",
            "Format of the initialization string does not conform to specification \
             starting at index 0. (Parameter 'DbConnectionOptions')",
        ));
        assert!(
            hints.iter().any(|h| h.text.contains("Key Vault")),
            "{hints:?}"
        );
    }

    #[test]
    fn the_same_signature_under_another_type_is_not_matched() {
        // Same message shape, different exception type: the Key Vault story
        // is specific to ArgumentException and must not be asserted here.
        let hints = classify(&group("System.FormatException", "bad DbConnectionOptions"));
        assert!(!hints.iter().any(|h| h.text.contains("Key Vault")));
    }

    #[test]
    fn one_failure_under_many_names_is_reported_as_one_cause() {
        let mut g = group("System.ArgumentException", "boom");
        g.roles = vec!["fn-orders".into(), "fn-billing".into(), "fn-audit".into()];
        let hints = classify(&g);
        let shared = hints.iter().find(|h| h.kind == HintKind::Shared).unwrap();
        assert!(shared.text.contains("3 apps"), "{}", shared.text);
        assert!(
            shared.text.contains("not 3 separate bugs"),
            "{}",
            shared.text
        );
    }

    #[test]
    fn a_single_app_gets_no_shared_cause_hint() {
        let hints = classify(&group("System.ArgumentException", "boom"));
        assert!(hints.iter().all(|h| h.kind != HintKind::Shared));
    }

    /// The distinction the whole tab rests on: a timer looks nothing like a
    /// burst, and min/max/count cannot tell them apart.
    #[test]
    fn a_steady_retry_is_regular_and_a_burst_is_not() {
        let every_90s: Vec<i64> = (0..20).map(|i| i * 90_000).collect();
        let c = cadence(&every_90s).unwrap();
        assert!(c.regular);
        assert!((c.median_secs - 90.0).abs() < 0.001);

        // Ragged: retries that are nothing like a schedule.
        let ragged = [0i64, 2_000, 45_000, 47_500, 600_000, 601_000, 900_000];
        assert!(!cadence(&ragged).unwrap().regular);
    }

    /// A burst is perfectly evenly spaced too, so evenness alone would call
    /// a hot loop a retry timer. It is the speed that gives it away.
    #[test]
    fn an_evenly_spaced_burst_is_not_a_retry_timer() {
        let burst: Vec<i64> = (0..20).map(|i| i * 500).collect();
        let c = cadence(&burst).unwrap();
        // Even — the spread test alone would have passed it.
        assert!(c.spread < 0.25);
        // But half a second apart is not something on a schedule.
        assert!(!c.regular, "a 0.5s hammer must not read as a retry timer");
    }

    #[test]
    fn real_world_jitter_still_counts_as_regular() {
        // A 60s timer that drifts by a second or two, as they all do.
        let jittery: Vec<i64> = [0, 61_000, 119_000, 181_000, 240_000, 302_000]
            .into_iter()
            .collect();
        assert!(cadence(&jittery).unwrap().regular);
    }

    #[test]
    fn too_few_occurrences_to_call_it_a_cadence() {
        assert!(cadence(&[0, 60_000, 120_000]).is_none());
        assert!(cadence(&[]).is_none());
        // Identical timestamps give a zero median gap and no usable answer.
        assert!(cadence(&[5, 5, 5, 5]).is_none());
    }

    #[test]
    fn rows_become_groups_with_sampling_made_visible() {
        let row = json!({
            "ExceptionType": "System.ArgumentException",
            "OuterMessage": "bad DbConnectionOptions",
            "Count": 400,
            "Events": 4,
            "First": "2026-08-24T10:00:00Z",
            "Last":  "2026-08-24T10:04:30Z",
            "Times": ["2026-08-24T10:00:00Z","2026-08-24T10:01:30Z",
                      "2026-08-24T10:03:00Z","2026-08-24T10:04:30Z"],
            "Roles": ["fn-orders","fn-billing"],
            "Operations": ["ProcessOrder"],
            "Correlations": ["abc-123","def-456"]
        });
        let g = build(&row);
        assert_eq!(g.count, 400);
        assert_eq!(g.events, 4);
        // 400 occurrences behind 4 stored rows — the view has to say so.
        assert!(g.sampled());
        assert!(g.first.is_some() && g.last.is_some());
        assert_eq!(g.correlation_ids.len(), 2);
        let c = g.cadence.unwrap();
        assert!(c.regular);
        assert!((c.median_secs - 90.0).abs() < 0.001);
        // Both the signature and the spread should have been recognised.
        assert!(g.hints.iter().any(|h| h.kind == HintKind::Cause));
        assert!(g.hints.iter().any(|h| h.kind == HintKind::Shared));
        assert!(g.hints.iter().any(|h| h.kind == HintKind::Cadence));
    }

    #[test]
    fn an_unsampled_group_does_not_claim_to_be_sampled() {
        let g = group("X", "y");
        assert!(!g.sampled());
    }

    #[test]
    fn gaps_read_the_way_someone_would_say_them() {
        assert_eq!(human_gap(45.0), "45s");
        assert_eq!(human_gap(89.4), "89s");
        assert_eq!(human_gap(120.0), "2m");
        assert_eq!(human_gap(150.0), "2m 30s");
        assert_eq!(human_gap(3600.0), "1h");
        assert_eq!(human_gap(3900.0), "1h 5m");
    }

    #[test]
    fn a_workspace_without_app_insights_is_recognised() {
        assert!(has_table_named(&[
            "AppTraces".into(),
            "AppExceptions".into()
        ]));
        // No App Insights: the view must say so rather than show "nothing
        // is failing", which would be a very different claim.
        assert!(!has_table_named(&["Syslog".into(), "Heartbeat".into()]));
        assert!(!has_table_named(&[]));
    }

    #[test]
    fn groups_are_identified_by_their_signature() {
        let a = group("System.ArgumentException", "boom");
        let b = group("System.ArgumentException", "different");
        assert_ne!(a.id(), b.id());
        assert_eq!(a.id(), group("System.ArgumentException", "boom").id());
    }
}
