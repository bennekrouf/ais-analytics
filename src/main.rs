mod kit;
mod mcp;
mod screens;
mod services;
mod telemetry;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;
use screens::{home::Home, welcome::Welcome};
use services::az::Workspace;

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn main() {
    // Before anything can shell out: an app launched from Finder or a .dmg
    // does not inherit the terminal's PATH, so `az` reads as "not found".
    services::env::adopt_login_path();

    if std::env::var("RUST_LOG").is_err() {
        // SAFETY: single-threaded, before any other threads (e.g. tokio) start.
        unsafe {
            std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn");
        }
    }

    kit::init();
    // `ais-analytics mcp`: the tools over stdio for an MCP client, and no
    // window, so no webview data folder either.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        return mcp::serve();
    }

    // Per-process subdirectory: WebView2 (Windows) and other webview engines
    // take an exclusive lock on their data directory, so two instances
    // sharing one would fail to start (or silently corrupt each other's
    // cache) when run concurrently.
    let instances_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-analytics");
    prune_dead_instance_dirs(&instances_dir);
    let webview_data_dir = instances_dir.join(format!("instance-{}", std::process::id()));
    claim_instance_dir(&webview_data_dir);

    let cfg = dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir)
        .with_window(window_builder(concat!(
            "AIS Analytics ",
            env!("CARGO_PKG_VERSION")
        )));
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

fn window_builder(title: &str) -> dioxus::desktop::WindowBuilder {
    dioxus::desktop::WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1100.0, 760.0))
        .with_always_on_top(false)
        .with_window_icon(window_icon())
}

/// The window icon, decoded from the embedded logo.
///
/// build.rs embeds `assets/icon.ico` into the .exe resource, which covers the
/// Start menu and shortcuts — but the *window* (title bar, alt-tab, taskbar
/// button) shows only what the app sets at runtime, and Windows falls back to
/// a blank default when it sets nothing.
///
/// Downscaled to 64px on the way in: tao hands Windows this single bitmap for
/// every size it needs, and letting it stretch a 1024px source down to a 16px
/// title bar is what makes the icon look muddy.
fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
    const SIZE: u32 = 64;

    let img = image::load_from_memory(ICON_PNG).ok()?.resize_exact(
        SIZE,
        SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    dioxus::desktop::tao::window::Icon::from_rgba(img.into_rgba8().into_raw(), SIZE, SIZE).ok()
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
            "AIS Analytics {} — {}",
            env!("CARGO_PKG_VERSION"),
            workspace.name
        ))),
    );
}

/// Held open for the life of the process. While this file is locked, the
/// directory holding it is in use; `prune_dead_instance_dirs` reads exactly
/// that to tell a live instance from a crashed one.
static INSTANCE_LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

/// Marks this process's webview data directory as in use.
///
/// Best-effort: a lock we cannot take costs us the protection below, never
/// the ability to start.
fn claim_instance_dir(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    let Ok(file) = std::fs::File::create(dir.join(LOCK_FILE)) else {
        return;
    };
    if file.try_lock().is_ok() {
        let _ = INSTANCE_LOCK.set(file);
    }
}

const LOCK_FILE: &str = ".instance-lock";

/// Removes `instance-*` webview data directories left behind by past runs.
///
/// Liveness is read from the lock file each instance holds open, not from a
/// timestamp: a directory's mtime only moves when entries are created or
/// removed *directly* in it, and the webview engines write into
/// subdirectories — so a session left open overnight looks untouched and the
/// next launch would delete the profile out from under it.
///
/// A directory with no lock file predates this scheme; those fall back to the
/// old age test, which is safe for them because nothing is holding them.
fn prune_dead_instance_dirs(instances_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(instances_dir) else {
        return;
    };
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
        if instance_is_dead(&path) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Whether nothing is using `dir` any more.
fn instance_is_dead(dir: &std::path::Path) -> bool {
    // Opened for writing, not just reading: an exclusive lock on Windows
    // needs a writable handle. `create(false)` so probing never leaves a
    // lock file behind in a directory that had none.
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(LOCK_FILE));
    match probe {
        // Locked by a live instance, or unreadable for a reason we cannot
        // interpret. Either way, leave it alone.
        Ok(file) => matches!(file.try_lock(), Ok(())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => aged_out(dir),
        Err(_) => false,
    }
}

/// The pre-lock-file fallback: a directory nothing has written to in a day.
fn aged_out(dir: &std::path::Path) -> bool {
    const CUTOFF: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .and_then(|m| m.elapsed().map_err(std::io::Error::other))
        .is_ok_and(|age| age > CUTOFF)
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

    // The update and notice banners are desktop-kit's (see `Banners` below):
    // "Install & restart" where this copy can replace itself.
    let mut connect_open = use_signal(|| false);
    use_context_provider(|| ConnectOpen(connect_open));

    use_effect(move || {
        // Encoded as a JSON string rather than pasted into a template
        // literal: hand-escaping covered the backtick and `${` but not the
        // backslash, so a single `content: "\201C"` in the stylesheet would
        // have been eaten by JS before CSS ever saw it.
        let css = serde_json::Value::String(MAIN_CSS.to_string()).to_string();
        document::eval(&format!(
            "if(!document.getElementById('ais-css')){{var s=document.createElement('style');s.id='ais-css';s.textContent={};document.head.appendChild(s);}}",
            css
        ));
    });

    use_effect(move || {
        let cls = if *is_light.read() { "light" } else { "" };
        document::eval(&format!("document.body.className = '{}';", cls));
    });

    let current = workspace.read().clone();

    // ── Anonymous usage statistics ─────────────────────────────────────────
    // On by default, but only once the person has been told: `start` reads the
    // opt-outs (including a shell profile's), records the launch, and says
    // whether the notice is still owed. See telemetry.rs.
    let mut ask_consent = use_signal(|| false);
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            if telemetry::start().await {
                ask_consent.set(true);
            }
            telemetry::flush_forever().await;
        },
    );
    // Recorded while the notice is on screen, so collection starts from the
    // next event — never from one the person had no chance to read about.
    use_effect(move || {
        if *ask_consent.read() {
            telemetry::mark_informed();
        }
    });

    rsx! {
        // A newer release and the notice from mayorana.ch, at the top.
        // AIS Analytics counts the update events in its own statistics.
        desktop_kit::ui::Banners {
            on_update_offered: move |to| telemetry::record(telemetry::Event::UpdateOffered { to }),
            on_update_clicked: move |to| telemetry::record(telemetry::Event::UpdateClicked { to }),
        }

        if connect_open() {
            desktop_kit::ui::ConnectWindow {
                lead: "Claude can list your Log Analytics workspaces, find the columns that tie \
                       a flow's steps together, follow a correlation id through them and read \
                       the signals, logs and recurring exceptions, the way this window does. \
                       It only reads.",
                try_it: "\u{201c}Which exceptions keep coming back in my workspace today?\u{201d}",
                caution: "Claude reads Azure as your signed-in az account, and the rows it reads \
                          go to the AI model you use with it.".to_string(),
                on_close: move |_| connect_open.set(false),
            }
        }

        // Usage-statistics notice: once, at the bottom so it never sits under the
        // update or notice banners. Either button is remembered.
        if *ask_consent.read() {
            div { class: "consent-banner",
                span { class: "update-banner-text",
                    strong { "AIS Analytics shares anonymous usage statistics. " }
                    "Whether it is installed and opened, and its version and operating system \
                     — never your files, data, accounts or anything you type."
                }
                button {
                    class: "update-banner-link",
                    onclick: move |_| {
                        telemetry::set_consent(true);
                        ask_consent.set(false);
                    },
                    "OK"
                }
                button {
                    class: "update-banner-link",
                    onclick: move |_| {
                        telemetry::set_consent(false);
                        ask_consent.set(false);
                    },
                    "Turn off"
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

/// Whether the "Use AIS Analytics from Claude" window is open, shared with
/// the screens so each can put the MCP button in its own bar.
#[derive(Clone, Copy)]
pub struct ConnectOpen(pub Signal<bool>);

/// The MCP button: opens the window that adds AIS Analytics to Claude
/// Desktop, Claude Code, Codex or VS Code.
#[component]
pub fn McpButton(class: String) -> Element {
    let ConnectOpen(mut open) = use_context::<ConnectOpen>();
    rsx! {
        button {
            class: "{class}",
            title: "Use AIS Analytics from Claude Desktop, Claude Code, Codex or VS Code",
            onclick: move |_| open.set(true),
            "MCP"
        }
    }
}
