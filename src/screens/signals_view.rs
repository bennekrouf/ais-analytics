//! Rate, failures and latency — and a way out of every row.
//!
//! The shape of the window comes first, because a number without its
//! trend says nothing: 88 failures is a catastrophe or a Tuesday depending
//! entirely on what the last six hours looked like. Underneath, the
//! operations are ranked by failures and each one leads into the trace.
//!
//! Two absences are stated rather than hidden. With no error rules defined
//! there is no failure line at all — what counts as a failure is domain
//! knowledge this app refuses to guess. With no duration column there are no
//! percentiles. Both say so on screen, next to the control that fixes them.

use crate::services::signals::{Operation, Signals};
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct SignalsViewProps {
    pub signals: Signals,
    pub loading: bool,
    pub error: Option<String>,
    /// The table these numbers came from, named so the reader knows.
    pub table: String,
    /// What the window panel is called. The dependency tab reuses this whole
    /// view and only changes what the rows are grouped by, so the headings
    /// have to come from outside.
    pub title: String,
    /// What the grouped rows are called — operations, or call targets.
    pub group_title: String,
    /// Why there is nothing to show. The two tabs sharing this view fail for
    /// different reasons, and "no table matched" without saying which
    /// requirement went unmet is not worth printing.
    pub empty_hint: String,
    /// False when the user has defined no error rules.
    pub knows_failure: bool,
    pub has_duration: bool,
    /// Pivot into the trace view on one correlation value.
    pub on_trace: EventHandler<String>,
}

#[component]
pub fn SignalsView(props: SignalsViewProps) -> Element {
    if props.loading {
        return rsx! { div { class: "panel", "Reading rate, failures and latency..." } };
    }
    if let Some(e) = props.error.clone() {
        return rsx! { div { class: "panel", div { class: "az-error", "{e}" } } };
    }
    if props.table.is_empty() {
        return rsx! {
            div { class: "panel",
                div { class: "az-hint", "{props.empty_hint}" }
            }
        };
    }
    if props.signals.is_empty() {
        return rsx! {
            div { class: "panel",
                div { class: "az-hint", "Nothing in {props.table} over this window." }
            }
        };
    }

    let signals = props.signals.clone();
    let total = signals.total();
    let failed = signals.failed();
    let pct = if total > 0.0 {
        100.0 * failed / total
    } else {
        0.0
    };
    // One scale for both series, so the failure bars read as a proportion of
    // the traffic rather than as their own chart.
    let peak = signals
        .series
        .iter()
        .map(|p| p.total)
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let worst_p95 = signals
        .series
        .iter()
        .filter_map(|p| p.p95)
        .fold(0.0_f64, f64::max);

    rsx! {
        div {
            div { class: "panel",
                div { class: "panel-head",
                    h3 { "{props.title}" }
                    span { class: "chip muted", "{props.table}" }
                }

                div { class: "sig-stats",
                    div { class: "sig-stat",
                        span { class: "sig-value", "{fmt_count(total)}" }
                        span { class: "sig-label", "total" }
                    }
                    if props.knows_failure {
                        div { class: "sig-stat",
                            span { class: if failed > 0.0 { "sig-value bad" } else { "sig-value" },
                                "{fmt_count(failed)}"
                            }
                            span { class: "sig-label", "failed — {pct:.1}%" }
                        }
                    }
                    if props.has_duration && worst_p95 > 0.0 {
                        div { class: "sig-stat",
                            span { class: "sig-value", "{fmt_ms(worst_p95)}" }
                            span { class: "sig-label", "worst p95 ({signals.unit})" }
                        }
                    }
                }

                div { class: "sig-bars",
                    for point in signals.series.iter() {
                        div {
                            class: "sig-bar",
                            title: "{fmt_count(point.total)} total, {fmt_count(point.failed)} failed",
                            div {
                                class: "sig-bar-total",
                                style: "height:{(100.0 * point.total / peak).max(1.0)}%;",
                            }
                            if point.failed > 0.0 {
                                div {
                                    class: "sig-bar-failed",
                                    style: "height:{(100.0 * point.failed / peak).max(1.0)}%;",
                                }
                            }
                        }
                    }
                }

                if !props.knows_failure {
                    p { class: "meta",
                        "No failure line: nothing here knows what counts as a failure yet. "
                        "Add an error rule in Setup — "
                        code { "ResultCode = 500" }
                        ", "
                        code { "Success = false" }
                        " — and it applies to this view and the trace cards alike."
                    }
                }
                if !props.has_duration {
                    p { class: "meta",
                        "No percentiles: no column in this table reads as a duration."
                    }
                }
            }

            div { class: "panel",
                div { class: "panel-head",
                    h3 { "{props.group_title}" }
                    span { class: "chip muted", "{signals.operations.len()}" }
                }
                p { class: "meta",
                    "Ranked by failures, then volume. Follow one to see what happened to it."
                }
                if signals.operations.len() == 1 && signals.operations[0].label == props.table {
                    p { class: "meta",
                        "One row: nothing in this table reads as a name to group by. "
                        "Pick a step label in Setup to break it up."
                    }
                }
                div { class: "op-table",
                    for (i, op) in signals.operations.iter().enumerate() {
                        OperationRow {
                            key: "{i}",
                            op: op.clone(),
                            has_duration: props.has_duration,
                            knows_failure: props.knows_failure,
                            on_trace: props.on_trace,
                        }
                    }
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct OperationRowProps {
    op: Operation,
    has_duration: bool,
    knows_failure: bool,
    on_trace: EventHandler<String>,
}

#[component]
fn OperationRow(props: OperationRowProps) -> Element {
    let op = &props.op;
    let failing = op.failures > 0.0;

    rsx! {
        div { class: if failing { "op-row failing" } else { "op-row" },
            span { class: "op-label", "{op.label}" }
            span { class: "op-num", "{fmt_count(op.calls)}" }
            if props.knows_failure {
                span { class: if failing { "op-num bad" } else { "op-num" },
                    if failing { "{fmt_count(op.failures)} ({op.failure_pct():.0}%)" } else { "—" }
                }
            }
            if props.has_duration {
                span { class: "op-num",
                    if let Some(p95) = op.p95 { "{fmt_ms(p95)}" } else { "—" }
                }
            }
            // Without a value there is nothing to follow, and an enabled
            // button that does nothing is worse than no button.
            if op.sample_key.is_empty() {
                span { class: "op-trace muted", "no id" }
            } else {
                button {
                    class: "op-trace",
                    title: "Follow {op.sample_key}",
                    onclick: {
                        let value = op.sample_key.clone();
                        move |_| props.on_trace.call(value.clone())
                    },
                    "trace →"
                }
            }
        }
    }
}

/// Counts are estimates once sampling is on, so beyond a thousand the extra
/// digits are false precision.
fn fmt_count(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1}k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

fn fmt_ms(v: f64) -> String {
    if v >= 1_000.0 {
        format!("{:.1}s", v / 1_000.0)
    } else {
        format!("{v:.0}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sampling makes every count an estimate, so the digits past three
    /// significant figures are invented.
    #[test]
    fn counts_shorten_rather_than_claim_precision_they_do_not_have() {
        assert_eq!(fmt_count(88.0), "88");
        assert_eq!(fmt_count(2_620.0), "2.6k");
        assert_eq!(fmt_count(1_500_000.0), "1.5M");
    }

    #[test]
    fn durations_switch_units_where_milliseconds_stop_reading() {
        assert_eq!(fmt_ms(340.0), "340ms");
        assert_eq!(fmt_ms(1_500.0), "1.5s");
    }
}
