//! The log stream, with the two filters that actually narrow it.
//!
//! Severity and a substring, nothing more. A log view grows filters
//! indefinitely if you let it, and every one costs a query; these two are
//! the ones that turn three hundred lines into the five you wanted.
//!
//! Each line ends in the same pivot as everywhere else. That is the whole
//! reason to read logs here instead of in the portal: the portal shows you
//! the line, this shows you the run it came from.

use crate::services::logs::{Entry, Filter};
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct LogsViewProps {
    pub entries: Vec<Entry>,
    pub loading: bool,
    pub error: Option<String>,
    pub table: String,
    /// Severity values seen in the scan. Empty hides the picker entirely.
    pub severities: Vec<String>,
    pub filter: Filter,
    pub on_filter: EventHandler<Filter>,
    pub on_trace: EventHandler<String>,
}

#[component]
pub fn LogsView(props: LogsViewProps) -> Element {
    // Declared before any early return: hooks must run on every render, and
    // this component bails out early on two different states.
    let mut draft = use_signal(|| props.filter.text.clone());

    if let Some(e) = props.error.clone() {
        return rsx! { div { class: "panel", div { class: "az-error", "{e}" } } };
    }
    if props.table.is_empty() {
        return rsx! {
            div { class: "panel",
                div { class: "az-hint",
                    "No table here holds both free text and the correlation key. "
                    "Pick a different key in Setup."
                }
            }
        };
    }

    let filter = props.filter.clone();
    let entries = props.entries.clone();

    rsx! {
        div {
            div { class: "panel",
                div { class: "panel-head",
                    h3 { "Logs" }
                    span { class: "chip muted", "{props.table}" }
                    if !props.loading {
                        span { class: "chip muted", "{entries.len()} lines" }
                    }
                }

                div { class: "log-filters",
                    input {
                        class: "log-search",
                        r#type: "text",
                        placeholder: "contains… then press Enter",
                        value: "{draft}",
                        oninput: move |evt| draft.set(evt.value()),
                        // Applied on Enter rather than per keystroke: every
                        // change is a query, and a search-as-you-type here
                        // bills the workspace for each letter.
                        onkeydown: {
                            let filter = filter.clone();
                            move |evt: KeyboardEvent| {
                                if evt.key() == Key::Enter {
                                    props.on_filter.call(Filter {
                                        text: draft.peek().clone(),
                                        ..filter.clone()
                                    });
                                }
                            }
                        },
                    }
                    if !props.severities.is_empty() {
                        select {
                            class: "log-severity",
                            onchange: {
                                let filter = filter.clone();
                                move |evt: FormEvent| {
                                    props.on_filter.call(Filter {
                                        severity: evt.value(),
                                        ..filter.clone()
                                    });
                                }
                            },
                            option { value: "", selected: filter.severity.is_empty(), "any severity" }
                            for s in props.severities.iter() {
                                option { value: "{s}", selected: filter.severity == *s, "{s}" }
                            }
                        }
                    }
                    if !filter.text.is_empty() || !filter.severity.is_empty() {
                        button {
                            class: "btn btn-small",
                            onclick: move |_| {
                                draft.set(String::new());
                                props.on_filter.call(Filter::default());
                            },
                            "clear"
                        }
                    }
                }

                if props.loading {
                    p { class: "meta", "Reading…" }
                } else if entries.is_empty() {
                    div { class: "az-hint",
                        "Nothing matches over this window."
                    }
                } else {
                    div { class: "log-list",
                        for (i, entry) in entries.iter().enumerate() {
                            LogRow {
                                key: "{i}",
                                entry: entry.clone(),
                                on_trace: props.on_trace,
                            }
                        }
                    }
                    if entries.len() >= 300 {
                        p { class: "meta",
                            "Capped at 300 lines — narrow the window or the search to see the rest."
                        }
                    }
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct LogRowProps {
    entry: Entry,
    on_trace: EventHandler<String>,
}

#[component]
fn LogRow(props: LogRowProps) -> Element {
    let e = &props.entry;
    let class = match severity_rank(&e.severity) {
        Rank::Bad => "log-row bad",
        Rank::Warn => "log-row warn",
        Rank::Plain => "log-row",
    };

    rsx! {
        div { class: "{class}",
            span { class: "log-at", "{short_time(&e.at_text)}" }
            if !e.severity.is_empty() {
                span { class: "log-sev", "{e.severity}" }
            }
            span { class: "log-msg", title: "{e.message}", "{e.message}" }
            if e.key_value.is_empty() {
                span { class: "op-trace muted", "no id" }
            } else {
                button {
                    class: "op-trace",
                    title: "Follow {e.key_value}",
                    onclick: {
                        let value = e.key_value.clone();
                        move |_| props.on_trace.call(value.clone())
                    },
                    "trace →"
                }
            }
        }
    }
}

enum Rank {
    Bad,
    Warn,
    Plain,
}

/// Severity is workspace vocabulary, not a fixed enum: it arrives as
/// `Error`, as `3`, as `Critical`, as whatever the emitter chose. So this
/// recognises the ones it knows and leaves everything else unstyled rather
/// than guessing a colour.
fn severity_rank(value: &str) -> Rank {
    let v = value.trim().to_lowercase();
    if matches!(v.as_str(), "error" | "critical" | "fatal" | "3" | "4") {
        Rank::Bad
    } else if matches!(v.as_str(), "warning" | "warn" | "2") {
        Rank::Warn
    } else {
        Rank::Plain
    }
}

/// Log Analytics returns full ISO timestamps; the date is the same for every
/// line on screen, so only the time carries information.
fn short_time(raw: &str) -> String {
    match (raw.find('T'), raw.find('.')) {
        (Some(t), Some(dot)) if dot > t => raw[t + 1..dot].to_string(),
        (Some(t), None) => raw[t + 1..].trim_end_matches('Z').to_string(),
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_time_of_day_is_shown() {
        assert_eq!(short_time("2026-09-08T13:31:38.224Z"), "13:31:38");
        assert_eq!(short_time("2026-09-08T13:31:38Z"), "13:31:38");
        assert_eq!(short_time("not a timestamp"), "not a timestamp");
    }

    /// Severity vocabulary is the workspace's, not ours. Numeric levels and
    /// words both appear, and anything unrecognised must stay unstyled
    /// rather than be coloured on a guess.
    #[test]
    fn known_severities_are_ranked_and_the_rest_left_alone() {
        assert!(matches!(severity_rank("Error"), Rank::Bad));
        assert!(matches!(severity_rank("3"), Rank::Bad));
        assert!(matches!(severity_rank("warning"), Rank::Warn));
        assert!(matches!(severity_rank("Information"), Rank::Plain));
        assert!(matches!(severity_rank("Verbose"), Rank::Plain));
    }
}
