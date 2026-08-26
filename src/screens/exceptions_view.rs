//! Recurring exceptions, with what they usually mean.
//!
//! The list is ordered by cadence first and volume second, because a
//! listener retrying every ninety seconds matters more than a noisier thing
//! that failed once during a deploy. Each group leads with the hint rather
//! than the raw message: the message is what the framework said, and it is
//! rarely what went wrong.

use crate::services::az::KeyVaultRef;
use crate::services::exceptions::{Group, HintKind};
use crate::services::trace;
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct ExceptionsViewProps {
    pub groups: Vec<Group>,
    pub loading: bool,
    pub error: Option<String>,
    /// True when the workspace has no `AppExceptions` table at all — a very
    /// different thing from having one with nothing in it.
    pub has_table: bool,
    pub unresolved: Vec<KeyVaultRef>,
    pub checking_config: bool,
    /// Set when a group is expanded.
    pub open: Option<String>,
    pub on_toggle: EventHandler<String>,
    /// Pivot into the trace view on one correlation id.
    pub on_trace: EventHandler<String>,
}

#[component]
pub fn ExceptionsView(props: ExceptionsViewProps) -> Element {
    if !props.has_table {
        return rsx! {
            div { class: "panel",
                div { class: "az-hint",
                    "This workspace has no AppExceptions table, so there is nothing to read. "
                    "That table arrives with a workspace-based Application Insights component."
                }
            }
        };
    }
    if props.loading {
        return rsx! { div { class: "panel", "Looking for recurring exceptions..." } };
    }
    if let Some(e) = props.error.clone() {
        return rsx! { div { class: "panel", div { class: "az-error", "{e}" } } };
    }

    let groups = props.groups.clone();
    let recurring = groups.iter().filter(|g| g.cadence.is_some_and(|c| c.regular)).count();

    rsx! {
        div {
            // Root cause before symptom: an unresolved reference explains a
            // whole screen of exceptions, so it goes above them.
            if !props.unresolved.is_empty() {
                div { class: "panel kv-panel",
                    div { class: "panel-head",
                        h3 { "Unresolved Key Vault references" }
                        span { class: "chip bad", "{props.unresolved.len()}" }
                    }
                    p { class: "meta",
                        "These settings are empty at runtime, not missing — the app starts, "
                        "reads a blank value, and throws something that never mentions Key Vault."
                    }
                    div { class: "kv-list",
                        for r in props.unresolved.iter() {
                            div { class: "kv-row",
                                span { class: "kv-app", "{r.app}" }
                                code { "{r.setting}" }
                                span { class: "chip bad", "{r.status}" }
                                if !r.details.is_empty() {
                                    span { class: "kv-detail", "{r.details}" }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "panel",
                div { class: "panel-head",
                    h3 { "Recurring exceptions" }
                    span { class: "chip muted", "{groups.len()} groups" }
                    if recurring > 0 {
                        span { class: "chip bad", "{recurring} on a fixed interval" }
                    }
                    if props.checking_config {
                        span { class: "scan-state", span { class: "dot pulse" } "checking config…" }
                    }
                }

                if groups.is_empty() {
                    div { class: "az-hint",
                        "Nothing repeated in this window. One-off exceptions are filtered out — "
                        "widen the range if you are looking for something rarer."
                    }
                }

                div { class: "exc-list",
                    for g in groups.iter() {
                        ExceptionRow {
                            group: g.clone(),
                            open: props.open.as_deref() == Some(g.id().as_str()),
                            on_toggle: props.on_toggle,
                            on_trace: props.on_trace,
                        }
                    }
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct ExceptionRowProps {
    group: Group,
    open: bool,
    on_toggle: EventHandler<String>,
    on_trace: EventHandler<String>,
}

#[component]
fn ExceptionRow(props: ExceptionRowProps) -> Element {
    let g = &props.group;
    let id = g.id();
    let regular = g.cadence.is_some_and(|c| c.regular);
    let window = match (g.first, g.last) {
        (Some(a), Some(b)) => format!("{} → {}", trace::format_time(a), trace::format_time(b)),
        _ => String::new(),
    };

    rsx! {
        div { class: if regular { "exc-row recurring" } else { "exc-row" },
            div {
                class: "exc-head",
                onclick: {
                    let id = id.clone();
                    move |_| props.on_toggle.call(id.clone())
                },
                span { class: "caret", if props.open { "▾" } else { "▸" } }
                span { class: "exc-type", "{g.exception_type}" }
                div { class: "spacer" }
                if let Some(c) = g.cadence {
                    if c.regular {
                        span { class: "chip bad",
                            "every ~{crate::services::exceptions::human_gap(c.median_secs)}"
                        }
                    }
                }
                span { class: "exc-count", "{g.count}×" }
            }

            p { class: "exc-message", "{g.outer_message}" }

            // The hints are the reason this view exists, so they sit above
            // the fold rather than inside the expanded detail.
            if !g.hints.is_empty() {
                div { class: "exc-hints",
                    for h in g.hints.iter() {
                        div {
                            class: match h.kind {
                                HintKind::Cause  => "exc-hint cause",
                                HintKind::Shared => "exc-hint shared",
                                HintKind::Cadence => "exc-hint cadence",
                            },
                            "{h.text}"
                        }
                    }
                }
            }

            if props.open {
                div { class: "exc-detail",
                    if !window.is_empty() {
                        p { class: "meta", "{window}" }
                    }
                    if g.sampled() {
                        p { class: "meta",
                            "{g.count} occurrences behind {g.events} stored rows — "
                            "Application Insights sampling is on for this app."
                        }
                    }
                    if !g.roles.is_empty() {
                        p { class: "meta", "apps: {g.roles.join(\", \")}" }
                    }
                    if !g.operations.is_empty() {
                        p { class: "meta", "operations: {g.operations.join(\", \")}" }
                    }

                    if g.correlation_ids.is_empty() {
                        p { class: "meta", "No correlation id recorded on these rows." }
                    } else {
                        div { class: "exc-trace",
                            span { class: "meta", "trace one:" }
                            for cid in g.correlation_ids.iter() {
                                button {
                                    class: "value-chip",
                                    title: "Follow {cid} in the Trace view",
                                    onclick: {
                                        let cid = cid.clone();
                                        move |_| props.on_trace.call(cid.clone())
                                    },
                                    "{shorten(cid)}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Correlation ids are too long for a chip; keep both ends, since the tail is
/// what distinguishes two of them at a glance.
fn shorten(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 18 {
        return value.to_string();
    }
    let head: String = chars[..8].iter().collect();
    let tail: String = chars[chars.len() - 6..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::shorten;

    #[test]
    fn short_ids_are_left_alone() {
        assert_eq!(shorten("abc-123"), "abc-123");
    }

    #[test]
    fn long_ids_keep_both_ends() {
        assert_eq!(shorten("9f1c2d3e4a5b6c7d8e9f0a1b"), "9f1c2d3e…9f0a1b");
    }

    /// Slicing by byte would panic; ids come from log data.
    #[test]
    fn multibyte_ids_do_not_panic() {
        assert_eq!(shorten(&"→".repeat(40)).chars().count(), 15);
    }
}
