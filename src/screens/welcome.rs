use crate::services::az::{self, AzLoginState, Workspace};
use crate::services::history;
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct WelcomeProps {
    pub on_connect: EventHandler<Workspace>,
}

/// Sign-in + workspace picker. Same shape as ais-tracing's `Welcome`
/// screen: check `az login`, offer to connect if not signed in, then let
/// the user pick the resource to work with — here a Log Analytics
/// workspace instead of a Cosmos DB account.
#[component]
pub fn Welcome(props: WelcomeProps) -> Element {
    let mut az_state = use_signal(|| AzLoginState::AzNotFound);
    let mut checking = use_signal(|| true);
    let mut workspaces = use_signal(Vec::<Workspace>::new);
    let mut workspaces_error = use_signal(|| Option::<String>::None);
    let mut loading_workspaces = use_signal(|| false);
    let mut selected_name = use_signal(String::new);
    let mut login_error = use_signal(|| Option::<String>::None);
    let mut recent = use_signal(history::load_workspaces);

    let mut load_workspaces = move || {
        loading_workspaces.set(true);
        spawn(async move {
            match tokio::task::spawn_blocking(az::list_workspaces).await {
                Ok(Ok(list)) => { workspaces.set(list); workspaces_error.set(None); }
                Ok(Err(e)) => { workspaces.set(vec![]); workspaces_error.set(Some(e)); }
                Err(e) => { workspaces.set(vec![]); workspaces_error.set(Some(e.to_string())); }
            }
            loading_workspaces.set(false);
        });
    };

    use_effect(move || {
        spawn(async move {
            let state = tokio::task::spawn_blocking(az::check_login).await
                .unwrap_or(AzLoginState::NotLoggedIn);
            let is_logged_in = matches!(state, AzLoginState::LoggedIn { .. });
            az_state.set(state);
            checking.set(false);
            if is_logged_in {
                load_workspaces();
            }
        });
    });

    let is_logged_in = matches!(*az_state.read(), AzLoginState::LoggedIn { .. });
    let list = workspaces.read().clone();
    let can_connect = !selected_name.read().is_empty();

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "ais-analytics" }
                p { class: "subtitle", "Azure Log Analytics — correlation-key flow explorer" }

                div { class: "welcome-box",
                    div { class: "welcome-pick",
                        if *checking.read() {
                            div { class: "az-status",
                                span { class: "dot pulse" }
                                span { "Checking Azure login..." }
                            }
                        } else {
                            match &*az_state.read() {
                                AzLoginState::LoggedIn { account, .. } => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot ok" }
                                        span { "Connected: {account}" }
                                    }
                                },
                                AzLoginState::AzNotFound => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot error" }
                                        span { "Azure CLI ('az') not found on PATH." }
                                    }
                                },
                                AzLoginState::NotLoggedIn => {
                                    let err = login_error.read().clone();
                                    rsx! {
                                        div { class: "az-status",
                                            span { class: "dot error" }
                                            span { "Not signed in" }
                                            button {
                                                class: "btn-primary",
                                                onclick: move |_| {
                                                    login_error.set(None);
                                                    match az::open_login() {
                                                        Ok(()) => {
                                                            checking.set(true);
                                                            spawn(async move {
                                                                for _ in 0..24 {
                                                                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                                                    let state = tokio::task::spawn_blocking(az::check_login).await
                                                                        .unwrap_or(AzLoginState::NotLoggedIn);
                                                                    let done = matches!(state, AzLoginState::LoggedIn { .. });
                                                                    az_state.set(state);
                                                                    checking.set(false);
                                                                    if done {
                                                                        load_workspaces();
                                                                        break;
                                                                    }
                                                                }
                                                            });
                                                        }
                                                        Err(e) => login_error.set(Some(e)),
                                                    }
                                                },
                                                "Connect to Azure"
                                            }
                                        }
                                        if let Some(e) = err {
                                            p { style: "margin-top:8px; font-size:12px; color:var(--red);", "{e}" }
                                        }
                                    }
                                },
                            }
                        }
                    }

                    if is_logged_in {
                        div { class: "az-form",
                            h3 { style: "font-size:11px; color:var(--text2); text-transform:uppercase; letter-spacing:0.06em; text-align:left;",
                                "Log Analytics workspace"
                            }
                            if *loading_workspaces.read() {
                                div { class: "az-loading", "Discovering Log Analytics workspaces across your subscriptions..." }
                            } else if let Some(e) = workspaces_error.read().clone() {
                                div { class: "az-error", "{e}" }
                            } else if list.is_empty() {
                                div { class: "az-hint", "No Log Analytics workspaces found in any signed-in subscription." }
                            } else {
                                div { class: "az-field",
                                    select {
                                        onchange: move |evt| selected_name.set(evt.value()),
                                        option { value: "", selected: selected_name.read().is_empty(), "-- choose a workspace --" }
                                        for ws in list.iter() {
                                            option { value: "{ws.customer_id}", "{ws.name}  ({ws.resource_group})" }
                                        }
                                    }
                                }
                            }
                            div { class: "az-form-actions",
                                button {
                                    class: "btn-primary",
                                    disabled: !can_connect,
                                    onclick: {
                                        let list = list.clone();
                                        let on_connect = props.on_connect.clone();
                                        move |_| {
                                            // Selected by GUID, not name: two
                                            // subscriptions can hold workspaces
                                            // that read identically.
                                            let id = selected_name.read().clone();
                                            if let Some(ws) = list.iter().find(|w| w.customer_id == id).cloned() {
                                                recent.set(history::record_workspace(&ws));
                                                on_connect.call(ws);
                                            }
                                        }
                                    },
                                    "Connect →"
                                }
                            }
                        }
                    }

                    // Recently opened workspaces. Shown even when signed out —
                    // knowing what you last worked on is useful before you can
                    // act on it — but only openable once `az` has a session.
                    if !recent.read().is_empty() {
                        div { class: "profile-section",
                            h3 { "Recent" }
                            for ws in recent.read().iter() {
                                div { class: "profile-item",
                                    div { class: "profile-main",
                                        div { class: "profile-label", "{ws.name}" }
                                        div { class: "profile-sub", "{ws.resource_group}" }
                                    }
                                    div { class: "profile-actions",
                                        button {
                                            class: "btn btn-open btn-small",
                                            title: if is_logged_in { "Open this workspace" } else { "Sign in first" },
                                            disabled: !is_logged_in,
                                            onclick: {
                                                let ws = ws.clone();
                                                let on_connect = props.on_connect.clone();
                                                move |_| {
                                                    recent.set(history::record_workspace(&ws));
                                                    on_connect.call(ws.clone());
                                                }
                                            },
                                            "Open →"
                                        }
                                        button {
                                            class: "btn btn-small",
                                            title: "Forget this workspace",
                                            onclick: {
                                                let id = ws.customer_id.clone();
                                                move |_| recent.set(history::forget_workspace(&id))
                                            },
                                            "✕"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
