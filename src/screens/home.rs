use crate::screens::exceptions_view::ExceptionsView;
use crate::screens::trace_view::TraceView;
use crate::services::loganalytics::{Client, TimeRange};
use crate::services::{az, az::Workspace, cache, discover, exceptions, history, schema, trace};
use dioxus::prelude::*;
use std::collections::BTreeSet;

/// Top-level sections. Setup answers "what are we tracing and where could it
/// be"; Trace answers "where did this one value actually go"; Issues answers
/// "what is failing repeatedly right now" — the question you have before you
/// have a correlation id to paste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Setup,
    Trace,
    Issues,
}

#[derive(Clone, Debug, PartialEq)]
enum LoadState {
    Idle,
    Loading,
    Done,
    Failed(String),
}

#[derive(Props, Clone, PartialEq)]
pub struct HomeProps {
    pub workspace: Workspace,
    /// Owned by the root App so the theme also applies to Welcome.
    pub is_light: Signal<bool>,
    pub on_back: EventHandler<()>,
}

#[component]
pub fn Home(props: HomeProps) -> Element {
    let workspace = props.workspace.clone();
    let workspace_id = workspace.customer_id.clone();
    let mut is_light = props.is_light;
    // Tracing is the job; setup is the thing you do once. Land on the work.
    let mut tab = use_signal(|| Tab::Trace);
    let mut state = use_signal(|| LoadState::Idle);
    let mut schemas = use_signal(Vec::<schema::TableSchema>::new);
    // Every Log Analytics query is bounded by a window — there is no
    // "all of it" — so the range is app state rather than a query detail,
    // and both the scan and every trace run inside it.
    let mut range = use_signal(TimeRange::default);
    let mut granting = use_signal(|| false);
    // A refresh running behind cached data, and how old that data is.
    let mut refreshing = use_signal(|| false);
    let mut refresh_error = use_signal(|| Option::<String>::None);
    let mut scanned_at = use_signal(|| Option::<i64>::None);

    // What the user is tracing on: the key that links steps, the field that
    // orders them, the field that names them.
    let mut key_id = use_signal(String::new);
    let mut time_id = use_signal(String::new);
    let mut label_id = use_signal(String::new);
    // Empty = one lane per table, the physical default.
    let mut lane_id = use_signal(String::new);

    // Issues tab. It reads a different window from the rest of the app on
    // purpose: spotting a listener that retries every ninety seconds needs a
    // tight window, while a trace lookup usually wants a wide one.
    let mut issues_range = use_signal(|| TimeRange::Last2Hours);
    let mut issues = use_signal(Vec::<exceptions::Group>::new);
    let mut issues_state = use_signal(|| LoadState::Idle);
    let mut issues_open = use_signal(|| Option::<String>::None);
    let mut unresolved = use_signal(Vec::<az::KeyVaultRef>::new);
    let mut checking_config = use_signal(|| false);

    let traced = use_signal(|| Option::<trace::Trace>::None);
    let tracing = use_signal(|| false);
    let mut selected_block = use_signal(|| Option::<String>::None);
    let mut recent = use_signal({
        let id = workspace_id.clone();
        move || history::load(&id)
    });
    let mut rules = use_signal({
        let id = workspace_id.clone();
        move || history::load_rules(&id)
    });

    let insights = use_memo(move || discover::analyze(&schemas.read()));

    // `background` means we already have cached schemas on screen: refresh
    // without blanking them, and keep them if the refresh fails.
    let run_scan = {
        let id = workspace_id.clone();
        move |background: bool| {
            let id = id.clone();
            let range = *range.peek();
            if !background {
                state.set(LoadState::Loading);
                schemas.set(Vec::new());
            }
            refreshing.set(true);
            refresh_error.set(None);
            spawn(async move {
                match scan_workspace(&id, range).await {
                    Ok(found) => {
                        cache::save(&id, range, &found);
                        scanned_at.set(Some(chrono::Utc::now().timestamp()));
                        schemas.set(found);
                        state.set(LoadState::Done);
                    }
                    Err(e) => {
                        if background {
                            // Cached data is still on screen and still useful;
                            // say the refresh failed rather than discarding it.
                            refresh_error.set(Some(e));
                        } else {
                            state.set(LoadState::Failed(e));
                        }
                    }
                }
                refreshing.set(false);
            });
        }
    };

    // Open on the last scan if we have one, so the window is usable
    // immediately, then refresh behind it. Otherwise this is a cold start.
    use_effect({
        let id = workspace_id.clone();
        let mut run_scan = run_scan.clone();
        move || {
            let cached = cache::load(&id, *range.peek());
            let warm = cached.is_some();
            if let Some(cached) = cached {
                scanned_at.set(Some(cached.scanned_at));
                schemas.set(cached.schemas);
                state.set(LoadState::Done);
            }
            run_scan(warm);
        }
    });

    // Propose the best-evidenced choice for each role, but only as a default:
    // every one stays a plain selection the user can override. A rescan that
    // no longer turns up the chosen field clears it rather than leaving a
    // selection nothing matches.
    use_effect(move || {
        let insights = insights.read();
        reconcile(&mut key_id, insights.keys.iter().map(|c| c.id.as_str()));
        reconcile(&mut time_id, insights.times.iter().map(|c| c.id.as_str()));
        reconcile(&mut label_id, insights.labels.iter().map(|c| c.id.as_str()));
        // Lanes default to tables, so this one only ever needs clearing.
        let lane = lane_id.peek().clone();
        if !lane.is_empty() && !insights.labels.iter().any(|c| c.id == lane) {
            lane_id.set(String::new());
        }
    });

    let follow = Follow {
        insights,
        key_id,
        time_id,
        label_id,
        recent,
        traced,
        tracing,
        selected: selected_block,
    };

    // Type-ahead over correlation values. Each lookup is a cross-table scan
    // billed by the data it touches, so it is debounced and only runs past a
    // minimum length — firing one per keystroke would be genuinely expensive.
    let mut typed = use_signal(String::new);
    let mut suggestions = use_signal(Vec::<String>::new);
    let mut suggesting = use_signal(|| false);
    use_effect({
        let id = workspace_id.clone();
        move || {
            let text = typed.read().trim().to_string();
            if text.len() < trace::MIN_FRAGMENT {
                suggestions.set(Vec::new());
                suggesting.set(false);
                return;
            }
            let Some(key) = insights
                .peek()
                .keys
                .iter()
                .find(|c| c.id == *key_id.peek())
                .cloned()
            else {
                return;
            };
            let tables = insights.peek().tables.clone();
            let id = id.clone();
            let range = *range.peek();
            suggesting.set(true);
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                // Superseded while waiting — drop it without querying.
                if *typed.peek() != text {
                    return;
                }
                let Ok(client) = Client::connect() else {
                    suggesting.set(false);
                    return;
                };
                let found = trace::suggest(&client, &id, &tables, &key, &text, range).await;
                // And again on return: a slow query must not overwrite the
                // results of a newer, faster one.
                if *typed.peek() != text {
                    return;
                }
                suggestions.set(found);
                suggesting.set(false);
            });
        }
    });

    // Land on something rather than an empty box: once the scan has settled on
    // a key, replay the value traced last. Guarded so it happens once per
    // session — re-running it on every rescan would fight the user.
    let mut autoloaded = use_signal(|| false);
    use_effect({
        let id = workspace_id.clone();
        move || {
            let ready = matches!(&*state.read(), LoadState::Done) && !key_id.read().is_empty();
            if !ready || *autoloaded.peek() {
                return;
            }
            // `peek` deliberately: `run` writes to `recent`, and subscribing
            // here would loop.
            let Some(last) = recent.peek().first().cloned() else {
                autoloaded.set(true);
                return;
            };
            autoloaded.set(true);
            follow.run(&id, last.value, *range.peek());
        }
    });

    // Finds what is failing repeatedly, then — separately and in the
    // background — asks ARM whether any Key Vault reference on the implicated
    // apps failed to resolve. The second answer is the cause of a good share
    // of the first, but it is a control-plane call and must never hold up the
    // list of exceptions.
    let load_issues = {
        let id = workspace_id.clone();
        let subscription = workspace.subscription_id.clone();
        move |_: ()| {
            let id = id.clone();
            let subscription = subscription.clone();
            let range = *issues_range.peek();
            issues_state.set(LoadState::Loading);
            unresolved.set(Vec::new());
            spawn(async move {
                let client = match Client::connect() {
                    Ok(c) => c,
                    Err(e) => {
                        issues_state.set(LoadState::Failed(e));
                        return;
                    }
                };
                match exceptions::recurring(
                    &client, &id, range, exceptions::DEFAULT_MIN_COUNT,
                ).await {
                    Ok(found) => {
                        // Only the apps that actually appear in a failure are
                        // worth a control-plane round-trip.
                        let apps: BTreeSet<String> = found
                            .iter()
                            .flat_map(|g| g.roles.iter().cloned())
                            .collect();
                        issues.set(found);
                        issues_state.set(LoadState::Done);

                        if !apps.is_empty() && !subscription.is_empty() {
                            checking_config.set(true);
                            let apps: Vec<String> = apps.into_iter().collect();
                            let found = tokio::task::spawn_blocking(move || {
                                az::unresolved_key_vault_refs(&subscription, &apps)
                            })
                            .await
                            .unwrap_or_default();
                            unresolved.set(found);
                            checking_config.set(false);
                        }
                    }
                    Err(e) => issues_state.set(LoadState::Failed(e)),
                }
            });
        }
    };

    let is_forbidden = matches!(&*state.read(), LoadState::Failed(e) if e.contains("access denied"));

    let on_setup = *tab.read() == Tab::Setup;
    let on_issues = *tab.read() == Tab::Issues;

    // The lane axis is a view of the rows already fetched, so changing it
    // re-renders rather than re-queries — and takes effect immediately on the
    // trace that's on screen.
    let shown = use_memo(move || {
        let lane_field = lane_id.read().clone();
        let expected = discover::field_values(&schemas.read(), &lane_field);
        traced
            .read()
            .as_ref()
            .map(|base| trace::relane(base, &lane_field, &expected))
    });
    let lane_options: Vec<(String, String)> = insights
        .read()
        .labels
        .iter()
        .map(|c| (c.id.clone(), c.label.clone()))
        .collect();

    // The open document, resolved from the selected card each render so it
    // can never drift out of step with the trace behind it. It belongs to the
    // Trace tab, so it folds away with the cards it came from.
    let open_doc = selected_block
        .read()
        .clone()
        .filter(|_| *tab.read() == Tab::Trace)
        .and_then(|id| {
        traced
            .read()
            .as_ref()
            // The card's own table, not the lane — once lanes are column
            // values the two are different things.
            .and_then(|t| t.find_block(&id).map(|(_, b)| (b.table.clone(), b.clone())))
        });

    rsx! {
        div { class: "app-shell",
        div { class: "topbar",
            // Leftmost, same as ais-monitor: back is navigation, not an action
            // on the current view.
            button {
                class: "btn btn-back",
                onclick: move |_| props.on_back.call(()),
                "‹ Back"
            }
            h1 { "ais-analytics" }
            span { class: "account-tag", "{workspace.name}  ({workspace.resource_group})" }

            div { class: "topbar-tabs",
                div { class: "topbar-group",
                    button {
                        class: if on_setup { "topbar-tab active" } else { "topbar-tab" },
                        title: "Setup — what we trace on, and the tables we found",
                        onclick: move |_| tab.set(Tab::Setup),
                        "⚙"
                    }
                    button {
                        class: if *tab.read() == Tab::Trace { "topbar-tab active" } else { "topbar-tab" },
                        title: "Trace — follow one key value across the tables",
                        onclick: move |_| tab.set(Tab::Trace),
                        "🔎"
                    }
                    button {
                        class: if on_issues { "topbar-tab active" } else { "topbar-tab" },
                        title: "Issues — what is failing repeatedly right now",
                        onclick: {
                            let mut load_issues = load_issues.clone();
                            move |_| {
                                tab.set(Tab::Issues);
                                // First visit only; refreshing is explicit.
                                if matches!(*issues_state.peek(), LoadState::Idle) {
                                    load_issues(());
                                }
                            }
                        },
                        "⚠"
                    }
                }
            }

            div { class: "spacer" }

            // The window everything is read through. Changing it changes
            // which tables even have data, so it rescans rather than just
            // re-filtering what is already on screen.
            select {
                class: "range-picker",
                title: "How far back to read",
                onchange: {
                    let mut run_scan = run_scan.clone();
                    move |evt: FormEvent| {
                        let picked = TimeRange::all()
                            .into_iter()
                            .find(|r| r.iso() == evt.value());
                        if let Some(picked) = picked {
                            if picked != *range.peek() {
                                range.set(picked);
                                run_scan(true);
                            }
                        }
                    }
                },
                for r in TimeRange::all() {
                    option {
                        value: "{r.iso()}",
                        selected: r == *range.read(),
                        "{r.label()}"
                    }
                }
            }

            // Say where the data came from and how old it is, so cached
            // content is never mistaken for a fresh read.
            if *refreshing.read() {
                span { class: "scan-state", span { class: "dot pulse" } "refreshing…" }
            } else if let Some(e) = refresh_error.read().clone() {
                span { class: "scan-state stale", title: "{e}",
                    span { class: "dot error" }
                    "refresh failed — showing cached"
                }
            } else if let Some(at) = *scanned_at.read() {
                span { class: "scan-state", "sampled {cache::age(at)}" }
            }

            button {
                class: "btn",
                disabled: *refreshing.read(),
                onclick: {
                    let mut run_scan = run_scan.clone();
                    // Explicit rescan keeps the cached view up while it runs —
                    // blanking the screen on a manual refresh is a regression
                    // from just leaving it there.
                    move |_| run_scan(true)
                },
                "↻ Rescan"
            }

            // Far right, after Rescan. The glyph shows what you'd switch *to*,
            // not what you're in.
            button {
                class: "btn-theme",
                title: if *is_light.read() { "Switch to dark mode" } else { "Switch to light mode" },
                onclick: move |_| {
                    let next = !*is_light.peek();
                    is_light.set(next);
                },
                if *is_light.read() { "🌙" } else { "☀️" }
            }
        }

        div { class: "screen-split",
        div { class: "main-screen",
            match &*state.read() {
                LoadState::Idle => rsx! {},
                LoadState::Loading => rsx! {
                    div { class: "panel", "Reading the workspace schema and sampling tables..." }
                },
                LoadState::Failed(e) => {
                    let e = e.clone();
                    rsx! {
                        div { class: "panel",
                            div { class: "az-error", "Scan failed: {e}" }
                            if is_forbidden {
                                p { style: "font-size:12px; color:var(--text2); margin-top:8px;",
                                    "Reading a workspace needs the Log Analytics Reader role. Granting it "
                                    "is itself a privileged operation, so this only works if you can already "
                                    "assign roles on the workspace."
                                }
                                button {
                                    class: "btn",
                                    style: "margin-top:8px;",
                                    disabled: *granting.read(),
                                    onclick: {
                                        let workspace = workspace.clone();
                                        let run_scan = run_scan.clone();
                                        move |_| {
                                            let workspace = workspace.clone();
                                            let mut run_scan = run_scan.clone();
                                            granting.set(true);
                                            spawn(async move {
                                                let result = tokio::task::spawn_blocking(move || {
                                                    crate::services::az::grant_self_log_analytics_reader(&workspace)
                                                }).await;
                                                granting.set(false);
                                                match result {
                                                    Ok(Ok(())) => {
                                                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                                        run_scan(true);
                                                    }
                                                    Ok(Err(e)) => state.set(LoadState::Failed(format!("Grant failed: {e}"))),
                                                    Err(e) => state.set(LoadState::Failed(format!("Grant failed: {e}"))),
                                                }
                                            });
                                        }
                                    },
                                    if *granting.read() { "Granting..." } else { "Grant myself Log Analytics Reader" }
                                }
                            }
                        }
                    }
                },
                LoadState::Done => {
                    let insights = insights.read().clone();
                    let key = insights
                        .keys
                        .iter()
                        .find(|c| c.id == *key_id.read())
                        .cloned();
                    rsx! {
                        div {
                            if on_issues {
                                div { class: "issues-bar",
                                    label { class: "rows-pick",
                                        "window:"
                                        select {
                                            onchange: {
                                                let mut load_issues = load_issues.clone();
                                                move |evt: FormEvent| {
                                                    let picked = TimeRange::all()
                                                        .into_iter()
                                                        .find(|r| r.iso() == evt.value());
                                                    if let Some(picked) = picked {
                                                        issues_range.set(picked);
                                                        load_issues(());
                                                    }
                                                }
                                            },
                                            for r in TimeRange::all() {
                                                option {
                                                    value: "{r.iso()}",
                                                    selected: r == *issues_range.read(),
                                                    "{r.label()}"
                                                }
                                            }
                                        }
                                    }
                                    div { class: "spacer" }
                                    button {
                                        class: "btn",
                                        disabled: matches!(*issues_state.read(), LoadState::Loading),
                                        onclick: {
                                            let mut load_issues = load_issues.clone();
                                            move |_| load_issues(())
                                        },
                                        "↻ Refresh"
                                    }
                                }

                                ExceptionsView {
                                    groups: issues.read().clone(),
                                    loading: matches!(*issues_state.read(), LoadState::Loading),
                                    error: match &*issues_state.read() {
                                        LoadState::Failed(e) => Some(e.clone()),
                                        _ => None,
                                    },
                                    has_table: exceptions::has_table_named(
                                        &schemas.read().iter().map(|t| t.table.clone()).collect::<Vec<_>>()
                                    ),
                                    unresolved: unresolved.read().clone(),
                                    checking_config: *checking_config.read(),
                                    open: issues_open.read().clone(),
                                    on_toggle: move |id: String| {
                                        let same = issues_open.peek().as_deref() == Some(id.as_str());
                                        issues_open.set(if same { None } else { Some(id) });
                                    },
                                    // Pivot: hand the correlation id to the
                                    // existing trace path and switch tabs, so
                                    // "what is broken" leads into "what
                                    // happened to this one run".
                                    on_trace: {
                                        let id = workspace_id.clone();
                                        move |cid: String| {
                                            tab.set(Tab::Trace);
                                            follow.run(&id, cid, *range.peek());
                                        }
                                    },
                                }
                            } else if on_setup {
                                if !schemas.read().is_empty() {
                                    TraceSetup {
                                        insights: insights.clone(),
                                        trace_key: key.clone(),
                                        time_id: time_id.read().clone(),
                                        label_id: label_id.read().clone(),
                                        lane_id: lane_id.read().clone(),
                                        on_key: move |v: String| key_id.set(v),
                                        on_time: move |v: String| time_id.set(v),
                                        on_label: move |v: String| label_id.set(v),
                                        on_lane: move |v: String| lane_id.set(v),
                                    }
                                }

                                ErrorRules {
                                    fields: discover::scalar_fields(&schemas.read()),
                                    schemas: schemas.read().clone(),
                                    rules: rules.read().clone(),
                                    on_change: {
                                        let id = workspace_id.clone();
                                        move |next: Vec<trace::ErrorRule>| {
                                            history::save_rules(&id, &next);
                                            rules.set(next);
                                        }
                                    },
                                }

                                TableList {
                                    schemas: schemas.read().clone(),
                                    trace_key: key.clone(),
                                }
                            } else if let Some(key) = key.clone() {
                                FollowValue {
                                    disabled: *tracing.read(),
                                    key_label: key.label.clone(),
                                    recent: recent.read().clone(),
                                    suggestions: suggestions.read().clone(),
                                    suggesting: *suggesting.read(),
                                    on_typed: move |v: String| typed.set(v),
                                    on_clear: {
                                        let id = workspace_id.clone();
                                        move |_| recent.set(history::clear(&id))
                                    },
                                    on_follow: {
                                        let id = workspace_id.clone();
                                        move |value: String| follow.run(&id, value, *range.peek())
                                    },
                                }
                            }

                            if *tab.read() == Tab::Trace {
                                if *tracing.read() {
                                    div { class: "panel", "Following the value across tables..." }
                                } else if let Some(t) = shown.read().clone() {
                                    TraceView {
                                        trace: t,
                                        rules: rules.read().clone(),
                                        on_pick_value: {
                                            let id = workspace_id.clone();
                                            move |v: String| follow.run(&id, v, *range.peek())
                                        },
                                        lane_id: lane_id.read().clone(),
                                        lane_options: lane_options.clone(),
                                        on_lane: move |v: String| lane_id.set(v),
                                        selected: selected_block.read().clone(),
                                        // Clicking the open card closes the panel.
                                        on_select: move |id: String| {
                                            let same = selected_block.peek().as_deref() == Some(id.as_str());
                                            selected_block.set(if same { None } else { Some(id) });
                                        },
                                    }
                                } else if key.is_none() {
                                    div { class: "az-hint",
                                        "No correlation key chosen yet — pick one in Setup ⚙ first."
                                    }
                                }
                            }

                        }
                    }
                },
            }
        }

        if let Some((table, block)) = open_doc {
            DocPanel {
                table,
                block,
                on_close: move |_| selected_block.set(None),
            }
        }
        }
        }
    }
}

/// Everything needed to start a trace, bundled so the button and the
/// auto-load on startup share one code path instead of drifting apart.
/// Signals and memos are `Copy`, so this is too.
#[derive(Clone, Copy)]
struct Follow {
    insights: Memo<discover::Insights>,
    key_id: Signal<String>,
    time_id: Signal<String>,
    label_id: Signal<String>,
    recent: Signal<Vec<history::Entry>>,
    traced: Signal<Option<trace::Trace>>,
    tracing: Signal<bool>,
    selected: Signal<Option<String>>,
}

impl Follow {
    fn run(mut self, workspace_id: &str, value: String, range: TimeRange) {
        let (key, tables, facts) = {
            let insights = self.insights.peek();
            let Some(key) = insights
                .keys
                .iter()
                .find(|c| c.id == *self.key_id.peek())
                .cloned()
            else {
                return;
            };
            let facts = insights.labels.iter().take(6).map(|c| c.id.clone()).collect();
            (key, insights.tables.clone(), facts)
        };

        self.recent.set(history::record(
            workspace_id,
            history::Entry {
                value: value.clone(),
                key: key.label.clone(),
            },
        ));

        let spec = trace::TraceSpec {
            key,
            value,
            time_field: self.time_id.peek().clone(),
            label_field: self.label_id.peek().clone(),
            fact_fields: facts,
            range,
        };

        let workspace_id = workspace_id.to_string();
        self.tracing.set(true);
        self.selected.set(None);
        spawn(async move {
            // One client for the whole trace: building it resolves
            // credentials, which can shell out to `az`.
            match Client::connect() {
                Ok(client) => {
                    let result = trace::run(&client, &workspace_id, &tables, &spec).await;
                    self.traced.set(Some(result));
                }
                Err(e) => {
                    // Every lane failed for the same reason; say so on each
                    // rather than leaving a blank timeline.
                    self.traced.set(Some(trace::Trace {
                        value: spec.value.clone(),
                        key_label: spec.key.label.clone(),
                        blocks_found: 0,
                        span: None,
                        partial: false,
                        matches: Vec::new(),
                        lanes: tables
                            .iter()
                            .map(|name| trace::Lane {
                                name: name.clone(),
                                detail: None,
                                blocks: Vec::new(),
                                state: trace::LaneState::Failed(0),
                                error: Some(e.clone()),
                            })
                            .collect(),
                    }));
                }
            }
            self.tracing.set(false);
        });
    }
}

/// Keeps a role selection valid across rescans: default to the top-ranked
/// option, drop it if it no longer exists.
fn reconcile<'a>(selection: &mut Signal<String>, mut options: impl Iterator<Item = &'a str>) {
    let current = selection.peek().clone();
    if current.is_empty() {
        if let Some(best) = options.next() {
            selection.set(best.to_string());
        }
    } else if !options.any(|id| id == current) {
        selection.set(String::new());
    }
}

/// One row of a `FieldPicker`.
#[derive(Clone, PartialEq)]
struct PickOption {
    id: String,
    label: String,
    note: String,
    /// Lowercased text the filter matches against. Built by the caller, so a
    /// correlation key can be found by any of the names it goes by rather
    /// than only the one it happens to be labelled with.
    haystack: String,
}

/// Whether an option survives the current filter.
///
/// The chosen option always survives. If filtering could hide it, the
/// selection would still be live while nothing on screen said so — the list
/// would be quietly lying about what is selected.
fn shows(option: &PickOption, needle: &str, selected: &str) -> bool {
    needle.is_empty() || option.haystack.contains(needle) || option.id == selected
}

#[derive(Props, Clone, PartialEq)]
struct FieldPickerProps {
    label: String,
    /// The row shown for the empty selection — "none", or the default the
    /// empty string stands for. `None` means this picker cannot be cleared.
    none_label: Option<String>,
    options: Vec<PickOption>,
    selected: String,
    filter_hint: String,
    on_pick: EventHandler<String>,
}

/// A filterable list of columns.
///
/// A workspace presents far more columns than a dropdown can serve: the
/// ranked lists here run to hundreds on anything with `AzureDiagnostics` in
/// it, and scrolling an alphabetical menu to find a name you can already
/// type is the wrong interaction.
///
/// An inline list rather than a native `select` for the reason the error
/// rules block already found: a native select stays shut while you type in a
/// separate box, so the filtering would be invisible at the moment it
/// matters.
#[component]
fn FieldPicker(props: FieldPickerProps) -> Element {
    let mut filter = use_signal(String::new);
    let needle = filter.read().trim().to_lowercase();
    let selected = props.selected.clone();

    let matches: Vec<&PickOption> = props
        .options
        .iter()
        .filter(|o| shows(o, &needle, &selected))
        .collect();

    let total = props.options.len();
    let shown = matches.len();
    let cleared = selected.is_empty();

    rsx! {
        div { class: "az-field",
            label { "{props.label}" }
            input {
                class: "field-filter",
                r#type: "text",
                placeholder: "{props.filter_hint}",
                value: "{filter}",
                oninput: move |e| filter.set(e.value()),
            }
            div { class: "field-list",
                // The "none" row is never filtered away: clearing a choice
                // must not depend on what is typed in the filter.
                if let Some(none_label) = props.none_label.clone() {
                    button {
                        class: if cleared { "field-option none on" } else { "field-option none" },
                        onclick: move |_| props.on_pick.call(String::new()),
                        span { class: "field-option-name", "{none_label}" }
                    }
                }
                if matches.is_empty() {
                    div { class: "field-empty", "no column matches “{needle}”" }
                }
                for o in matches.iter() {
                    button {
                        class: if selected == o.id { "field-option on" } else { "field-option" },
                        title: "{o.label}",
                        onclick: {
                            let id = o.id.clone();
                            move |_| props.on_pick.call(id.clone())
                        },
                        span { class: "field-option-name", "{o.label}" }
                        span { class: "field-option-note", "{o.note}" }
                    }
                }
            }
            span { class: "meta",
                if needle.is_empty() {
                    "{total} options"
                } else {
                    "{shown} of {total}"
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct TraceSetupProps {
    insights: discover::Insights,
    trace_key: Option<discover::KeyCandidate>,
    time_id: String,
    label_id: String,
    lane_id: String,
    on_key: EventHandler<String>,
    on_time: EventHandler<String>,
    on_label: EventHandler<String>,
    on_lane: EventHandler<String>,
}

/// Asks — right after the scan, off the sampled data — what to trace on.
/// Every option and every ranking comes from the rows themselves, so this
/// reads the same on a stock App Insights workspace and on one full of
/// custom tables that follow no convention.
#[component]
fn TraceSetup(props: TraceSetupProps) -> Element {
    let total = props.insights.tables.len();
    let key = props.trace_key.clone();

    // A key group can span several spellings across several tables, and any
    // one of them is a reasonable thing to search for — so all of them go
    // into the haystack, not just the label the group ended up with.
    let key_options: Vec<PickOption> = props
        .insights
        .keys
        .iter()
        .map(|c| {
            let mut haystack = c.label.to_lowercase();
            for b in &c.bindings {
                haystack.push(' ');
                haystack.push_str(&b.field.to_lowercase());
                haystack.push(' ');
                haystack.push_str(&b.table.to_lowercase());
            }
            PickOption {
                id: c.id.clone(),
                label: c.label.clone(),
                note: format!(
                    "{}/{} tables{}",
                    c.bindings.len(),
                    total,
                    if c.shared_values > 0 { ", values match" } else { "" },
                ),
                haystack,
            }
        })
        .collect();

    let role_options = |candidates: &[discover::RoleCandidate]| -> Vec<PickOption> {
        candidates
            .iter()
            .map(|c| PickOption {
                // Role ids are already the lowercased column path, which is
                // exactly what someone types to find one.
                haystack: c.id.clone(),
                id: c.id.clone(),
                label: c.label.clone(),
                note: c.note.clone(),
            })
            .collect()
    };
    let time_options = role_options(&props.insights.times);
    let label_options = role_options(&props.insights.labels);

    rsx! {
        div { class: "panel key-panel",
            div { class: "panel-head",
                h3 { "What are we tracing?" }
                span { class: "chip muted", "{total} tables sampled" }
            }

            div { class: "role-grid",
                FieldPicker {
                    label: "Correlation key — links the steps",
                    none_label: Some("-- choose a column --".to_string()),
                    options: key_options,
                    selected: key.as_ref().map(|k| k.id.clone()).unwrap_or_default(),
                    filter_hint: "filter… e.g. operation",
                    on_pick: move |v: String| props.on_key.call(v),
                }

                FieldPicker {
                    label: "Order by — sequences the steps",
                    none_label: Some("-- none --".to_string()),
                    options: time_options,
                    selected: props.time_id.clone(),
                    filter_hint: "filter… e.g. time",
                    on_pick: move |v: String| props.on_time.call(v),
                }

                FieldPicker {
                    label: "Rows (left index) — one lane per…",
                    none_label: Some("table (default)".to_string()),
                    options: label_options.clone(),
                    selected: props.lane_id.clone(),
                    filter_hint: "filter… e.g. stage",
                    on_pick: move |v: String| props.on_lane.call(v),
                }

                FieldPicker {
                    label: "Step label — names each step",
                    none_label: Some("-- none --".to_string()),
                    options: label_options,
                    selected: props.label_id.clone(),
                    filter_hint: "filter… e.g. message",
                    on_pick: move |v: String| props.on_label.call(v),
                }
            }

            match props.trace_key.as_ref() {
                Some(c) => rsx! {
                    div { class: "key-summary",
                        div { class: "evidence",
                            for e in c.evidence.iter() {
                                span {
                                    class: if e.good { "chip ok" } else { "chip warn" },
                                    "{e.text}"
                                }
                            }
                        }
                        p { class: "meta",
                            "Reads as: "
                            for (i, b) in c.bindings.iter().enumerate() {
                                if i > 0 { ", " }
                                code { "{b.table}.{b.field}" }
                            }
                        }
                        if !c.missing.is_empty() {
                            p { class: "meta", "Not sampled in: {c.missing.join(\", \")}" }
                        }
                    }
                },
                None => rsx! {
                    div { class: "az-hint",
                        "Pick the column whose value is the same across the steps of one flow. "
                        "Nothing in the sample linked tables on its own."
                    }
                },
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct DocPanelProps {
    table: String,
    block: trace::Block,
    on_close: EventHandler<()>,
}

/// The full row behind a card. A card can only carry three facts; this is
/// everything else — dynamic columns expanded in place, so what you read is
/// what the workspace returned.
#[component]
fn DocPanel(props: DocPanelProps) -> Element {
    let pretty = serde_json::to_string_pretty(&props.block.doc)
        .unwrap_or_else(|e| format!("could not render document: {e}"));
    let lines = pretty.lines().count();
    let bytes = pretty.len();
    let mut copied = use_signal(|| false);

    rsx! {
        aside { class: "doc-panel",
            div { class: "doc-head",
                div { style: "min-width:0;",
                    h3 { "{props.block.label}" }
                    span { class: "doc-source", "{props.table}" }
                }
                button {
                    class: "btn",
                    onclick: move |_| props.on_close.call(()),
                    "✕"
                }
            }
            if !props.block.at_text.is_empty() {
                p { class: "meta", "{props.block.at_text}" }
            }
            // The button is a sibling of the scroller, not inside it, so it
            // stays pinned to the corner as the document scrolls.
            div { class: "doc-body",
                pre { class: "doc-json", "{pretty}" }
                button {
                    class: if *copied.read() { "doc-copy done" } else { "doc-copy" },
                    title: "Copy the row as JSON",
                    onclick: {
                        let pretty = pretty.clone();
                        move |_| {
                            // Best-effort: a clipboard we can't reach is not
                            // worth interrupting the user over.
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                if clipboard.set_text(pretty.clone()).is_ok() {
                                    copied.set(true);
                                    spawn(async move {
                                        tokio::time::sleep(
                                            std::time::Duration::from_millis(1400),
                                        )
                                        .await;
                                        copied.set(false);
                                    });
                                }
                            }
                        }
                    },
                    if *copied.read() { "copied" } else { "copy" }
                }
            }
            p { class: "meta doc-foot", "{lines} lines · {bytes} bytes" }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct FollowValueProps {
    key_label: String,
    disabled: bool,
    recent: Vec<history::Entry>,
    /// Correlation values containing what's been typed so far.
    suggestions: Vec<String>,
    suggesting: bool,
    on_follow: EventHandler<String>,
    on_typed: EventHandler<String>,
    on_clear: EventHandler<()>,
}

/// The value side of the question: the setup panel chose which field links a
/// flow, this asks which flow.
#[component]
fn FollowValue(props: FollowValueProps) -> Element {
    // Prefilled with the most recent value, which is also the one auto-traced
    // on launch — an empty box above a populated timeline reads as a mismatch.
    let mut value = use_signal(|| {
        props
            .recent
            .first()
            .map(|e| e.value.clone())
            .unwrap_or_default()
    });
    let ready = !value.read().trim().is_empty() && !props.disabled;
    // Suppress the list once the box already holds one of its own suggestions
    // — otherwise picking one leaves a dropdown offering the thing you picked.
    let exact_already = props
        .suggestions
        .iter()
        .any(|s| *s == *value.read().trim());

    let submit = move |_| {
        let v = value.read().trim().to_string();
        if !v.is_empty() {
            props.on_follow.call(v);
        }
    };

    rsx! {
        div { class: "panel follow-panel",
            div { class: "follow-row",
                div { class: "az-field",
                    label { "Follow one value of {props.key_label}" }
                    input {
                        r#type: "text",
                        placeholder: "paste a value, or type part of one…",
                        value: "{value}",
                        oninput: move |e| {
                            value.set(e.value());
                            props.on_typed.call(e.value());
                        },
                        onkeydown: move |e| {
                            if e.key() == Key::Enter && ready {
                                let v = value.read().trim().to_string();
                                if !v.is_empty() {
                                    props.on_follow.call(v);
                                }
                            }
                        },
                    }

                    // Only worth showing while the box holds a fragment: once
                    // it holds a full id, the list is just the id again.
                    if !exact_already && (props.suggesting || !props.suggestions.is_empty()) {
                        div { class: "suggest-list",
                            if props.suggesting && props.suggestions.is_empty() {
                                div { class: "suggest-empty", "searching…" }
                            }
                            for s in props.suggestions.iter() {
                                button {
                                    class: "suggest-row",
                                    onclick: {
                                        let s = s.clone();
                                        move |_| {
                                            value.set(s.clone());
                                            props.on_follow.call(s.clone());
                                        }
                                    },
                                    "{s}"
                                }
                            }
                        }
                    }
                }
                button {
                    class: "btn-primary",
                    disabled: !ready,
                    onclick: submit,
                    if props.disabled { "Following..." } else { "Follow →" }
                }
            }

            if !props.recent.is_empty() {
                div { class: "recent-row",
                    span { class: "recent-label", "Recent" }
                    for e in props.recent.iter() {
                        button {
                            class: "recent-chip",
                            title: "{e.value}\ntraced on {e.key}",
                            disabled: props.disabled,
                            onclick: {
                                let v = e.value.clone();
                                move |_| {
                                    value.set(v.clone());
                                    props.on_follow.call(v.clone());
                                }
                            },
                            "{shorten(&e.value)}"
                        }
                    }
                    div { class: "spacer" }
                    button {
                        class: "recent-clear",
                        title: "Forget these values",
                        onclick: move |_| props.on_clear.call(()),
                        "clear"
                    }
                }
            }
        }
    }
}

/// Correlation ids are too long for a chip. Keep both ends — the tail is
/// usually what distinguishes two ids at a glance.
fn shorten(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 20 {
        return value.to_string();
    }
    let head: String = chars[..10].iter().collect();
    let tail: String = chars[chars.len() - 6..].iter().collect();
    format!("{head}…{tail}")
}


#[derive(Props, Clone, PartialEq)]
struct ErrorRulesProps {
    fields: Vec<discover::RoleCandidate>,
    schemas: Vec<schema::TableSchema>,
    rules: Vec<trace::ErrorRule>,
    on_change: EventHandler<Vec<trace::ErrorRule>>,
}

/// Teaches the app which column values mean failure.
///
/// Nothing here is inferred: `ResultCode = 500` is meaningful only because
/// someone says it is, and guessing would be worse than not colouring cards
/// at all.
#[component]
fn ErrorRules(props: ErrorRulesProps) -> Element {
    let mut field = use_signal(String::new);
    let mut value = use_signal(String::new);

    let chosen = field.read().clone();

    let options: Vec<PickOption> = props
        .fields
        .iter()
        .map(|f| PickOption {
            haystack: f.id.clone(),
            id: f.id.clone(),
            label: f.label.clone(),
            note: f.note.clone(),
        })
        .collect();
    // The values actually sampled for the chosen column, so the common case is
    // clicking one rather than remembering what the codes are.
    let observed = discover::field_values(&props.schemas, &chosen);
    let label = props
        .fields
        .iter()
        .find(|f| f.id == chosen)
        .map(|f| f.label.clone())
        .unwrap_or_default();
    let ready = !chosen.is_empty() && !value.read().trim().is_empty();

    // EventHandler is Copy and the rule list is not, so each handler takes its
    // own clone rather than sharing one closure across two call sites.
    let on_change = props.on_change;
    let current = props.rules.clone();

    rsx! {
        div { class: "panel",
            div { class: "panel-head",
                h3 { "Error rules" }
                if !props.rules.is_empty() {
                    span { class: "chip warn", "{props.rules.len()} active" }
                }
            }
            p { class: "meta", "Cards matching any of these are drawn in red." }

            if !props.rules.is_empty() {
                div { class: "rule-list",
                    for rule in props.rules.iter() {
                        span { class: "rule-chip",
                            "{rule.label()}"
                            button {
                                class: "rule-drop",
                                title: "Remove this rule",
                                onclick: {
                                    let (rule, current) = (rule.clone(), current.clone());
                                    move |_| {
                                        let next: Vec<_> = current
                                            .iter()
                                            .filter(|r| **r != rule)
                                            .cloned()
                                            .collect();
                                        on_change.call(next);
                                    }
                                },
                                "✕"
                            }
                        }
                    }
                }
            }

            div { class: "rule-form",
                FieldPicker {
                    label: "Column",
                    // A rule needs a column, so there is nothing to clear to.
                    none_label: None,
                    options: options,
                    selected: chosen.clone(),
                    filter_hint: "filter… e.g. resultcode",
                    on_pick: move |id: String| {
                        // The pending value belongs to the column that was
                        // chosen before; keeping it would half-build a rule
                        // nobody asked for.
                        field.set(id);
                        value.set(String::new());
                    },
                }
                div { class: "az-field",
                    label {
                        if label.is_empty() {
                            "Means failure when it equals"
                        } else {
                            "Means failure when {label} equals"
                        }
                    }
                    input {
                        r#type: "text",
                        placeholder: "e.g. 500",
                        value: "{value}",
                        oninput: move |e| value.set(e.value()),
                        onkeydown: {
                            let (current, label) = (current.clone(), label.clone());
                            move |e: KeyboardEvent| {
                                if e.key() == Key::Enter && ready {
                                    commit(&current, &mut field, &mut value, &label, on_change);
                                }
                            }
                        },
                    }
                }
                button {
                    class: "btn",
                    disabled: !ready,
                    onclick: {
                        let (current, label) = (current.clone(), label.clone());
                        move |_| commit(&current, &mut field, &mut value, &label, on_change)
                    },
                    "Add rule"
                }
            }

            if !chosen.is_empty() && !observed.is_empty() {
                div { class: "rule-observed",
                    span { class: "meta", "sampled values:" }
                    for v in observed.iter().take(12) {
                        button {
                            class: "value-chip",
                            onclick: {
                                let v = v.clone();
                                move |_| value.set(v.clone())
                            },
                            "{v}"
                        }
                    }
                }
            }
        }
    }
}

/// Appends the pending column/value as a rule, ignoring duplicates, and
/// clears the value box ready for the next one.
fn commit(
    current: &[trace::ErrorRule],
    field: &mut Signal<String>,
    value: &mut Signal<String>,
    display: &str,
    on_change: EventHandler<Vec<trace::ErrorRule>>,
) {
    let field_id = field.peek().clone();
    let text = value.peek().trim().to_string();
    if field_id.is_empty() || text.is_empty() {
        return;
    }
    let rule = trace::ErrorRule {
        field: field_id,
        display: display.to_string(),
        value: text,
    };
    if !current.contains(&rule) {
        let mut next = current.to_vec();
        next.push(rule);
        on_change.call(next);
    }
    value.set(String::new());
}

#[derive(Props, Clone, PartialEq)]
struct TableListProps {
    schemas: Vec<schema::TableSchema>,
    trace_key: Option<discover::KeyCandidate>,
}

/// The workspace's tables. Collapsed by default — a workspace can hold a
/// hundred of them, and the header row already carries what you scan for:
/// whether the key lives there, and how much we saw.
#[component]
fn TableList(props: TableListProps) -> Element {
    let mut open = use_signal(BTreeSet::<String>::new);
    let paths: Vec<String> = props.schemas.iter().map(schema::TableSchema::path).collect();
    let all_open = !paths.is_empty() && paths.iter().all(|p| open.read().contains(p));
    let with_data = props.schemas.iter().filter(|s| s.rows_in_range > 0).count();

    if props.schemas.is_empty() {
        return rsx! {
            div { class: "panel", "No tables found in this workspace." }
        };
    }

    rsx! {
        div { class: "panel",
            div { class: "panel-head",
                h3 { "Tables" }
                span { class: "chip muted", "{with_data} of {props.schemas.len()} with data in range" }
                div { class: "spacer" }
                button {
                    class: "btn",
                    onclick: move |_| {
                        if all_open {
                            open.write().clear();
                        } else {
                            open.set(paths.iter().cloned().collect());
                        }
                    },
                    if all_open { "Collapse all" } else { "Expand all" }
                }
            }

            div { class: "container-list",
                for s in props.schemas.iter() {
                    TableRow {
                        schema: s.clone(),
                        trace_key: props.trace_key.clone(),
                        open: open.read().contains(&s.path()),
                        on_toggle: move |path: String| {
                            let mut open = open.write();
                            if !open.remove(&path) {
                                open.insert(path);
                            }
                        },
                    }
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct TableRowProps {
    schema: schema::TableSchema,
    trace_key: Option<discover::KeyCandidate>,
    open: bool,
    on_toggle: EventHandler<String>,
}

#[component]
fn TableRow(props: TableRowProps) -> Element {
    let s = &props.schema;
    let path = s.path();
    let bound = props
        .trace_key
        .as_ref()
        .and_then(|k| k.binding_for(&path))
        .map(|b| b.field.clone());

    rsx! {
        div { class: if props.open { "container-row open" } else { "container-row" },
            div {
                class: "container-head",
                onclick: {
                    let path = path.clone();
                    move |_| props.on_toggle.call(path.clone())
                },
                span { class: "caret", if props.open { "▾" } else { "▸" } }
                span { class: "container-name", "{s.table}" }
                if props.trace_key.is_some() {
                    match &bound {
                        // Metadata says whether the column exists, so this is
                        // a fact about the table rather than an inference
                        // from whatever the sample happened to show.
                        Some(field) => rsx! { span { class: "chip ok", "key: {field}" } },
                        None => rsx! { span { class: "chip muted", "no key" } },
                    }
                }
                div { class: "spacer" }
                span { class: "container-meta",
                    if s.rows_in_range == 0 {
                        "{s.fields.len()} columns · no rows in range"
                    } else {
                        "{s.fields.len()} columns · {s.rows_in_range} rows · {s.sampled_rows} sampled"
                    }
                }
            }

            if props.open {
                table { class: "schema-table",
                    thead {
                        tr {
                            th { "column" }
                            th { "type" }
                            th { "seen in" }
                            th { "distinct" }
                        }
                    }
                    tbody {
                        for f in s.fields.iter() {
                            tr {
                                class: if bound.as_deref() == Some(f.name.as_str()) { "key-row" } else { "" },
                                td { "{f.name}" }
                                // The declared type where the workspace has
                                // one; inside a dynamic column there is none,
                                // so fall back to what the values looked like.
                                td {
                                    if f.kind.is_empty() {
                                        "{f.types.join(\", \")}"
                                    } else {
                                        "{f.kind}"
                                    }
                                }
                                td { "{f.seen_in}/{s.sampled_rows}" }
                                td { "{f.distinct}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PickOption, shorten, shows};

    fn option(id: &str, haystack: &str) -> PickOption {
        PickOption {
            id: id.into(),
            label: id.into(),
            note: String::new(),
            haystack: haystack.into(),
        }
    }

    #[test]
    fn an_empty_filter_shows_everything() {
        assert!(shows(&option("operationid", "operationid"), "", ""));
    }

    #[test]
    fn the_filter_matches_anywhere_in_the_haystack() {
        let o = option("properties.correlationid", "properties.correlationid");
        assert!(shows(&o, "correlation", ""));
        assert!(shows(&o, "properties", ""));
        assert!(!shows(&o, "operation", ""));
    }

    /// A correlation key can span several spellings across several tables,
    /// and any of them is a reasonable thing to search for.
    #[test]
    fn a_key_is_findable_by_any_name_it_goes_by() {
        let o = option("AppRequests\u{1}OperationId", "operationid job_ref_g apprequests myapp_cl");
        assert!(shows(&o, "job_ref", ""));
        assert!(shows(&o, "myapp", ""));
        assert!(shows(&o, "operationid", ""));
    }

    /// The invariant that keeps the list honest: a filter must never hide the
    /// thing it says is selected.
    #[test]
    fn the_selected_option_survives_any_filter() {
        let o = option("resultcode", "resultcode");
        assert!(!shows(&o, "zzz", ""));
        assert!(shows(&o, "zzz", "resultcode"));
    }

    #[test]
    fn short_values_are_left_alone() {
        assert_eq!(shorten("order-42"), "order-42");
        assert_eq!(shorten(&"x".repeat(20)), "x".repeat(20));
    }

    #[test]
    fn long_ids_keep_both_ends() {
        let uuid = "9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";
        assert_eq!(shorten(uuid), "9f1c2d3e-4…3d4e5f");
    }

    /// Slicing by byte would panic here; the chip must survive any value the
    /// user pastes.
    #[test]
    fn multibyte_values_do_not_panic() {
        let value = "→".repeat(40);
        assert_eq!(shorten(&value).chars().count(), 17);
    }
}

async fn scan_workspace(
    workspace_id: &str,
    range: TimeRange,
) -> Result<Vec<schema::TableSchema>, String> {
    // One client for the whole scan — building it resolves credentials,
    // which can shell out to `az`.
    let client = Client::connect()?;
    schema::scan(&client, workspace_id, range).await
}
