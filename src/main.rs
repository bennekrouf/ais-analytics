mod screens;
mod services;
mod update_check;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;
use screens::{home::Home, welcome::Welcome};
use services::az::Workspace;

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        // SAFETY: single-threaded, before any other threads (e.g. tokio) start.
        unsafe {
            std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn");
        }
    }

    // Per-process subdirectory: WebView2 (Windows) and other webview engines
    // take an exclusive lock on their data directory, so two instances
    // sharing one would fail to start (or silently corrupt each other's
    // cache) when run concurrently.
    let instances_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-analytics");
    prune_stale_instance_dirs(&instances_dir);
    let webview_data_dir = instances_dir.join(format!("instance-{}", std::process::id()));

    let cfg = dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir)
        .with_window(window_builder(concat!(
            "ais-analytics ",
            env!("CARGO_PKG_VERSION")
        )));
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

fn window_builder(title: &str) -> dioxus::desktop::WindowBuilder {
    dioxus::desktop::WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1100.0, 760.0))
        .with_always_on_top(false)
}

/// Opens another window on `workspace`, in this same process — sharing the
/// webview data directory and every on-disk cache rather than racing a
/// second process for them. Each window gets its own `VirtualDom`, so its
/// own signals and its own welcome screen to go back to.
pub fn open_in_new_window(workspace: Workspace) {
    let dom = VirtualDom::new_with_props(
        AppRoot,
        AppRootProps {
            initial: Some(workspace.clone()),
        },
    );
    dioxus::desktop::window().new_window(
        dom,
        dioxus::desktop::Config::new().with_window(window_builder(&format!(
            "ais-analytics {} — {}",
            env!("CARGO_PKG_VERSION"),
            workspace.name
        ))),
    );
}

/// Removes `instance-*` webview data directories left behind by past runs.
/// PIDs are reused by the OS, so we can't check liveness directly; a run
/// older than a day is assumed to have exited (or crashed) already.
fn prune_stale_instance_dirs(instances_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(instances_dir) else {
        return;
    };
    let cutoff = std::time::Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let path = entry.path();
        let is_instance_dir = path.is_dir()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("instance-"));
        if !is_instance_dir {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|m| m.elapsed().map_err(|e| std::io::Error::other(e)))
            .is_ok_and(|age| age > cutoff);
        if stale {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// The first window's root. Every other window is an `AppRoot` too — this
/// exists only because the launcher needs a component that takes no props.
#[component]
fn App() -> Element {
    rsx! { AppRoot { initial: Option::<Workspace>::None } }
}

/// One window. Each has its own `VirtualDom`, so its own signals and its own
/// open workspace — and its own welcome screen to go back to.
#[component]
fn AppRoot(initial: Option<Workspace>) -> Element {
    let mut workspace = use_signal(|| initial);

    let system_light =
        dark_light::detect().unwrap_or(dark_light::Mode::Dark) != dark_light::Mode::Dark;
    let is_light = use_signal(|| system_light);

    // ── Auto-update check ──────────────────────────────────────────────────
    // Deliberately after a delay and entirely best-effort: a release check is
    // never worth slowing a cold start, and a failed one is not worth saying
    // anything about.
    let mut update_info = use_signal(|| Option::<update_check::UpdateInfo>::None);
    let mut update_dismissed = use_signal(|| false);
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if let Some(info) = update_check::check().await {
                update_info.set(Some(info));
            }
        },
    );

    use_effect(move || {
        let css = MAIN_CSS.replace('`', "\\`").replace("${", "\\${");
        document::eval(&format!(
            "if(!document.getElementById('ais-css')){{var s=document.createElement('style');s.id='ais-css';s.textContent=`{}`;document.head.appendChild(s);}}",
            css
        ));
    });

    use_effect(move || {
        let cls = if *is_light.read() { "light" } else { "" };
        document::eval(&format!("document.body.className = '{}';", cls));
    });

    let current = workspace.read().clone();

    rsx! {
        // Update banner — fixed top, dismissable per session.
        if let (Some(info), false) = (update_info.read().clone(), *update_dismissed.read()) {
            div { class: "update-banner",
                span { class: "update-banner-text",
                    "ais-analytics "
                    strong { "{info.latest_version}" }
                    " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                }
                a {
                    class: "update-banner-link",
                    href: "{info.release_url}",
                    target: "_blank",
                    "Download"
                }
                button {
                    class: "update-banner-dismiss",
                    onclick: move |_| update_dismissed.set(true),
                    "×"
                }
            }
        }

        match current {
            None => rsx! {
                Welcome {
                    on_connect: move |ws: Workspace| open_in_new_window(ws),
                }
            },
            Some(ws) => rsx! {
                // The theme signal is owned here so it also covers Welcome;
                // Home only needs it to render the toggle.
                Home {
                    workspace: ws,
                    is_light: is_light,
                    on_back: move |_| workspace.set(None),
                }
            },
        }
    }
}
