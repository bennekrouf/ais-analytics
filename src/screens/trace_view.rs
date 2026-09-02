//! The instance view: one key value, one lane per stage, time on the x axis.
//! What counts as a stage is decided upstream — a table by default, or the
//! values of a chosen column.
//!
//! Deliberately not a force-directed graph. The layout is arithmetic, so the
//! same flow renders identically every time and two values can be compared
//! side by side — and an empty lane keeps its place instead of being pushed
//! around by a simulation. That also means no d3 and no JS bridge: the
//! positions are computed here and rendered as ordinary elements.

use crate::services::trace::{self, Block, Lane, LaneState, Trace};
use dioxus::prelude::*;

const CARD_W: f64 = 158.0;
const CARD_GAP: f64 = 10.0;
const PAD: f64 = 20.0;
/// Width the timeline gets when nothing forces it wider.
const BASE_TRACK: f64 = 900.0;
const TICKS: usize = 5;

#[derive(Props, Clone, PartialEq)]
pub struct TraceViewProps {
    pub trace: Trace,
    /// Field values that mark a step as failed.
    pub rules: Vec<trace::ErrorRule>,
    /// Currently selected lane column; empty means tables.
    pub lane_id: String,
    /// Selectable lane fields as `(id, label)`.
    pub lane_options: Vec<(String, String)>,
    pub on_lane: EventHandler<String>,
    /// Id of the card whose row is open in the detail panel.
    pub selected: Option<String>,
    /// Fires with the clicked card's id; the parent decides whether that
    /// opens or closes the panel.
    pub on_select: EventHandler<String>,
    /// Narrow an ambiguous fragment to one exact correlation value.
    pub on_pick_value: EventHandler<String>,
}

#[component]
pub fn TraceView(props: TraceViewProps) -> Element {
    let trace = &props.trace;
    let (lanes, track_w) = place(trace);
    let off_path: Vec<&Lane> = trace
        .lanes
        .iter()
        .filter(|l| l.state == LaneState::OffPath)
        .collect();
    let ticks = axis_ticks(trace, track_w);
    // A red card can sit far off-screen on a wide timeline, so the count has
    // to be visible without scrolling to find it.
    let errored = trace
        .lanes
        .iter()
        .flat_map(|l| l.blocks.iter())
        .filter(|b| trace::is_error(&b.doc, &props.rules))
        .count();

    rsx! {
        div { class: "panel trace-panel",
            div { class: "panel-head",
                // On a fragment hit, show the value actually found rather than
                // what was typed — they differ, and the real one is the answer.
                h3 {
                    match (trace.partial, trace.matches.as_slice()) {
                        (true, [(found, _)]) => format!("{} = {found}", trace.key_label),
                        _ => format!("{} = {}", trace.key_label, trace.value),
                    }
                }
                if trace.partial {
                    span { class: "chip warn", "matched “{trace.value}” as a fragment" }
                }
                span { class: "chip ok", "{trace.reached()} reached" }
                if trace.awaiting() > 0 {
                    span { class: "chip warn", "{trace.awaiting()} awaiting" }
                }
                if errored > 0 {
                    span { class: "chip bad", "{errored} errored" }
                }
                span { class: "chip muted", "{trace.blocks_found} rows" }
                div { class: "spacer" }
                // The axis control lives next to the axis it changes. Setting
                // it from a distant panel made a no-op indistinguishable from
                // a setting that hadn't been applied.
                label { class: "rows-pick",
                    "rows:"
                    select {
                        onchange: move |evt| props.on_lane.call(evt.value()),
                        option {
                            value: "",
                            selected: props.lane_id.is_empty(),
                            "tables"
                        }
                        for (id, label) in props.lane_options.iter() {
                            option {
                                value: "{id}",
                                selected: props.lane_id == *id,
                                "{label}"
                            }
                        }
                    }
                }
            }

            if trace.blocks_found == 0 {
                div { class: "az-hint",
                    "No row anywhere carries this value, in full or in part. "
                    "Check the value, or the key."
                }
            } else if trace.matches.len() > 1 {
                // Drawing these together would silently interleave unrelated
                // flows on one timeline, which is worse than asking.
                div { class: "az-hint",
                    "“{trace.value}” matches {trace.matches.len()} different "
                    "{trace.key_label} values. Pick the one to follow."
                }
                div { class: "match-list",
                    for (value, count) in trace.matches.iter() {
                        button {
                            class: "match-row",
                            onclick: {
                                let value = value.clone();
                                move |_| props.on_pick_value.call(value.clone())
                            },
                            span { class: "match-value", "{value}" }
                            span { class: "match-count", "{count} rows" }
                        }
                    }
                }
            } else {
                div { class: "trace-scroll",
                    div { class: "trace-track", style: "width:{track_w}px;",
                        if !ticks.is_empty() {
                            div { class: "trace-axis",
                                for (x, text) in ticks.iter() {
                                    div { class: "tick", style: "left:{x}px;", "{text}" }
                                }
                            }
                        }

                        for placed in lanes.iter() {
                            LaneRow {
                                lane: placed.clone(),
                                rules: props.rules.clone(),
                                start: trace.span.map(|(lo, _)| lo),
                                selected: props.selected.clone(),
                                on_select: props.on_select,
                            }
                        }
                    }
                }
            }

            if !off_path.is_empty() {
                p { class: "meta off-path",
                    "Not on this key's path — "
                    code { "{trace.key_label}" }
                    " does not exist in: "
                    for (i, l) in off_path.iter().enumerate() {
                        if i > 0 { ", " }
                        "{l.name}"
                    }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
struct PlacedLane {
    lane: Lane,
    blocks: Vec<(f64, Block)>,
}

#[derive(Props, Clone, PartialEq)]
struct LaneRowProps {
    lane: PlacedLane,
    rules: Vec<trace::ErrorRule>,
    start: Option<i64>,
    selected: Option<String>,
    on_select: EventHandler<String>,
}

#[component]
fn LaneRow(props: LaneRowProps) -> Element {
    let lane = &props.lane.lane;
    let class = match lane.state {
        LaneState::Reached => "lane reached",
        LaneState::Awaiting => "lane awaiting",
        LaneState::Failed(_) => "lane failed",
        LaneState::OffPath => "lane off-path",
    };
    // How long after the first event anywhere this stage saw the value.
    let gap = match (props.start, lane.first_at()) {
        (Some(start), Some(at)) if at > start => Some(trace::format_gap(at - start)),
        _ => None,
    };

    rsx! {
        div { class: "{class}",
            div { class: "lane-head",
                span { class: "lane-name", title: "{lane.name}", "{lane.name}" }
                if let Some(field) = &lane.detail {
                    span { class: "lane-field", "{field}" }
                }
                if let Some(gap) = gap {
                    span { class: "lane-gap", "{gap}" }
                }
            }
            div { class: "lane-body",
                match lane.state {
                    LaneState::Awaiting => rsx! {
                        div { class: "lane-note", "awaiting — no row with this value" }
                    },
                    LaneState::Failed(_) => rsx! {
                        div { class: "lane-note error",
                            "query failed: {lane.error.clone().unwrap_or_default()}"
                        }
                    },
                    _ => rsx! {
                        for (x, block) in props.lane.blocks.iter() {
                            div {
                                class: {
                                    let mut c = String::from("block");
                                    if trace::is_error(&block.doc, &props.rules) {
                                        c.push_str(" errored");
                                    }
                                    if props.selected.as_deref() == Some(block.id.as_str()) {
                                        c.push_str(" selected");
                                    }
                                    c
                                },
                                style: "left:{x}px; width:{CARD_W}px;",
                                title: "{block.label}",
                                onclick: {
                                    let id = block.id.clone();
                                    move |_| props.on_select.call(id.clone())
                                },
                                div { class: "block-label", "{block.label}" }
                                if !block.at_text.is_empty() {
                                    div { class: "block-time", "{block.at_text}" }
                                }
                                for (name, value) in block.facts.iter() {
                                    div { class: "block-fact",
                                        span { class: "fact-name", "{name}" }
                                        span { class: "fact-value", "{value}" }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Positions every block on a shared time scale, then nudges overlapping
/// cards apart within their lane. Blocks with no usable timestamp fall back
/// to sequence order after the ones that have times.
fn place(trace: &Trace) -> (Vec<PlacedLane>, f64) {
    let usable = BASE_TRACK - CARD_W - 2.0 * PAD;
    let mut out = Vec::new();
    let mut widest = BASE_TRACK;

    for lane in trace.lanes.iter().filter(|l| l.state != LaneState::OffPath) {
        let mut placed: Vec<(f64, Block)> = Vec::new();
        let mut untimed = 0usize;

        for block in &lane.blocks {
            let x = match (block.at, trace.span) {
                (Some(at), Some((lo, hi))) if hi > lo => {
                    PAD + ((at - lo) as f64 / (hi - lo) as f64) * usable
                }
                (Some(_), _) => PAD,
                (None, _) => {
                    untimed += 1;
                    PAD + (untimed as f64 - 1.0) * (CARD_W + CARD_GAP)
                }
            };
            placed.push((x, block.clone()));
        }

        // Cards are wide; two events milliseconds apart would sit on top of
        // each other. Keep chronological order and push right just enough.
        placed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut cursor = f64::NEG_INFINITY;
        for (x, _) in placed.iter_mut() {
            if *x < cursor {
                *x = cursor;
            }
            cursor = *x + CARD_W + CARD_GAP;
        }

        if let Some((x, _)) = placed.last() {
            widest = widest.max(x + CARD_W + PAD);
        }
        out.push(PlacedLane {
            lane: lane.clone(),
            blocks: placed,
        });
    }

    (out, widest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(at: Option<i64>) -> Block {
        Block {
            id: format!("b{}", at.unwrap_or(-1)),
            label: "step".into(),
            at,
            at_text: String::new(),
            facts: vec![],
            table: "AppTraces".into(),
            key_value: "abc".into(),
            doc: serde_json::Value::Null,
        }
    }

    fn lane(name: &str, state: LaneState, blocks: Vec<Block>) -> Lane {
        Lane {
            name: name.into(),
            detail: Some("OperationId".into()),
            blocks,
            state,
            error: None,
        }
    }

    fn trace_of(lanes: Vec<Lane>) -> Trace {
        let times: Vec<i64> = lanes
            .iter()
            .flat_map(|l| l.blocks.iter().filter_map(|b| b.at))
            .collect();
        Trace {
            value: "abc".into(),
            key_label: "correlationId".into(),
            blocks_found: lanes.iter().map(|l| l.blocks.len()).sum(),
            partial: false,
            matches: vec![],
            span: times.iter().min().copied().zip(times.iter().max().copied()),
            lanes,
        }
    }

    #[test]
    fn blocks_sit_on_a_shared_time_scale() {
        let t = trace_of(vec![
            lane("db/a", LaneState::Reached, vec![block(Some(0))]),
            lane("db/b", LaneState::Reached, vec![block(Some(10_000))]),
        ]);
        let (placed, _) = place(&t);

        // First event pinned at the left pad, last at the far end of the
        // usable width — the same scale in both lanes.
        assert_eq!(placed[0].blocks[0].0, PAD);
        assert_eq!(placed[1].blocks[0].0, BASE_TRACK - CARD_W - PAD);
    }

    #[test]
    fn overlapping_cards_are_pushed_apart_without_reordering() {
        // Three events a millisecond apart would land on the same pixel.
        let t = trace_of(vec![
            lane(
                "db/a",
                LaneState::Reached,
                vec![block(Some(0)), block(Some(1)), block(Some(2))],
            ),
            lane("db/b", LaneState::Reached, vec![block(Some(600_000))]),
        ]);
        let (placed, width) = place(&t);
        let xs: Vec<f64> = placed[0].blocks.iter().map(|(x, _)| *x).collect();

        for pair in xs.windows(2) {
            assert!(
                pair[1] - pair[0] >= CARD_W,
                "cards must not overlap: {pair:?}"
            );
        }
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "order must be preserved");
        assert!(width >= BASE_TRACK);
    }

    #[test]
    fn the_track_widens_when_cards_run_past_the_base_width() {
        let blocks: Vec<Block> = (0..12).map(|_| block(Some(0))).collect();
        let t = trace_of(vec![lane("db/a", LaneState::Reached, blocks)]);
        let (_, width) = place(&t);
        assert!(
            width > BASE_TRACK,
            "twelve stacked cards need more than the base width, got {width}"
        );
    }

    #[test]
    fn untimed_blocks_fall_back_to_sequence_order() {
        let t = trace_of(vec![lane(
            "db/a",
            LaneState::Reached,
            vec![block(None), block(None)],
        )]);
        let (placed, _) = place(&t);
        assert_eq!(placed[0].blocks[0].0, PAD);
        assert_eq!(placed[0].blocks[1].0, PAD + CARD_W + CARD_GAP);
    }

    /// Empty lanes are the point of the view — they must keep their row.
    #[test]
    fn awaiting_lanes_are_laid_out_but_off_path_lanes_are_not() {
        let t = trace_of(vec![
            lane("db/a", LaneState::Reached, vec![block(Some(0))]),
            lane("db/b", LaneState::Awaiting, vec![]),
            lane("db/c", LaneState::OffPath, vec![]),
        ]);
        let (placed, _) = place(&t);
        let names: Vec<&str> = placed.iter().map(|p| p.lane.name.as_str()).collect();
        assert_eq!(names, vec!["db/a", "db/b"]);
    }

    #[test]
    fn a_single_instant_does_not_divide_by_zero() {
        let t = trace_of(vec![lane("db/a", LaneState::Reached, vec![block(Some(7))])]);
        let (placed, _) = place(&t);
        assert_eq!(placed[0].blocks[0].0, PAD);
        assert_eq!(axis_ticks(&t, BASE_TRACK).len(), 1);
    }

    #[test]
    fn axis_ticks_span_the_track() {
        let t = trace_of(vec![
            lane("db/a", LaneState::Reached, vec![block(Some(0))]),
            lane("db/b", LaneState::Reached, vec![block(Some(60_000))]),
        ]);
        let ticks = axis_ticks(&t, BASE_TRACK);
        assert_eq!(ticks.len(), TICKS);
        assert_eq!(ticks[0].0, PAD);
        assert_eq!(ticks[TICKS - 1].1, "+1.0min");
    }
}

fn axis_ticks(trace: &Trace, track_w: f64) -> Vec<(f64, String)> {
    let Some((lo, hi)) = trace.span else {
        return Vec::new();
    };
    if hi <= lo {
        return vec![(PAD, trace::format_time(lo))];
    }
    let usable = track_w - CARD_W - 2.0 * PAD;
    (0..TICKS)
        .map(|i| {
            let frac = i as f64 / (TICKS - 1) as f64;
            let at = lo + ((hi - lo) as f64 * frac) as i64;
            let text = if i == 0 {
                trace::format_time(at)
            } else {
                trace::format_gap(at - lo)
            };
            (PAD + frac * usable, text)
        })
        .collect()
}
