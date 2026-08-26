//! Thin wrapper around the local `az` CLI — same auth story as ais-tracing:
//! the user is expected to already be signed in (`az login`), and we shell
//! out for anything control-plane (ARM) rather than data-plane. Data-plane
//! queries go through `azure_identity::DeveloperToolsCredential` instead
//! (see `loganalytics.rs`), which reads the same `az` session.

use serde::{Deserialize, Serialize};
use std::process::Command;

fn az_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("az");
    cmd.args(args);
    cmd
}

#[derive(Clone, Debug, PartialEq)]
pub enum AzLoginState {
    LoggedIn {
        account: String,
        subscription_id: String,
    },
    NotLoggedIn,
    AzNotFound,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct AzAccount {
    #[serde(default)]
    name: String,
    #[serde(default)]
    id: String,
}

pub fn check_login() -> AzLoginState {
    let out = az_command(&["account", "show", "--output", "json"]).output();
    match out {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout);
            match serde_json::from_str::<AzAccount>(&body) {
                Ok(acc) => AzLoginState::LoggedIn {
                    account: acc.name,
                    subscription_id: acc.id,
                },
                Err(_) => AzLoginState::NotLoggedIn,
            }
        }
        Ok(_) => AzLoginState::NotLoggedIn,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AzLoginState::AzNotFound,
        Err(_) => AzLoginState::NotLoggedIn,
    }
}

/// Opens `az login` (non-blocking) so the desktop app doesn't have to embed
/// its own OAuth flow.
pub fn open_login() -> Result<(), String> {
    az_command(&["login"]).spawn().map(|_| ()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "Azure CLI ('az') not found on PATH.".to_string()
        } else {
            format!("Failed to start 'az login': {e}")
        }
    })
}

fn list_subscription_ids() -> Result<Vec<String>, String> {
    let output = az_command(&["account", "list", "--query", "[].id", "--output", "json"])
        .output()
        .map_err(|e| format!("az account list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))
}

/// A Log Analytics workspace discovered in the signed-in subscription(s).
///
/// `Serialize` is here so recently-opened workspaces can be remembered; the
/// field renames round-trip, so what we write back matches what `az` emits.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Workspace {
    pub name: String,
    #[serde(rename = "resourceGroup")]
    pub resource_group: String,
    /// The workspace GUID. This — not the ARM id — is what the query API
    /// addresses a workspace by, and it is what caches and history key on.
    #[serde(rename = "customerId")]
    pub customer_id: String,
    #[serde(default)]
    pub subscription_id: String,
}

impl Workspace {
    /// Full ARM resource id, needed for role assignments.
    pub fn resource_id(&self) -> String {
        format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.OperationalInsights/workspaces/{}",
            self.subscription_id, self.resource_group, self.name,
        )
    }
}

/// Lists every Log Analytics workspace visible across *all* of the signed-in
/// account's subscriptions — a single account often only has its workspaces
/// in one non-default subscription, so scanning just the current default
/// misses them. This is an ARM (control-plane) call via `az`, kept separate
/// from the data-plane queries in `loganalytics.rs`.
pub fn list_workspaces() -> Result<Vec<Workspace>, String> {
    let sub_ids = list_subscription_ids()?;
    let mut workspaces = Vec::new();
    for sub_id in sub_ids {
        let output = az_command(&[
            "monitor",
            "log-analytics",
            "workspace",
            "list",
            "--subscription",
            sub_id.as_str(),
            "--query",
            "[].{name:name,resourceGroup:resourceGroup,customerId:customerId}",
            "--output",
            "json",
        ])
        .output()
        .map_err(|e| format!("az monitor log-analytics workspace list failed: {e}"))?;
        if !output.status.success() {
            // A subscription the caller can't read (PIM not activated, etc.)
            // shouldn't block discovery in the others.
            continue;
        }
        let body = String::from_utf8_lossy(&output.stdout);
        let mut found: Vec<Workspace> =
            serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
        for ws in &mut found {
            ws.subscription_id = sub_id.clone();
        }
        workspaces.extend(found);
    }
    Ok(workspaces)
}

/// The signed-in user's Entra object id — needed as the assignee for a role
/// assignment.
fn signed_in_principal_id() -> Result<String, String> {
    let output = az_command(&[
        "ad",
        "signed-in-user",
        "show",
        "--query",
        "id",
        "--output",
        "tsv",
    ])
    .output()
    .map_err(|e| format!("az ad signed-in-user show failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Grants the signed-in user Log Analytics Reader on `workspace`, so
/// subsequent queries stop 403'ing.
///
/// Simpler than the Cosmos equivalent: Log Analytics data access is plain
/// ARM RBAC, with none of the separate SQL role plane Cosmos has. The caller
/// still needs rights to *create* the assignment (Owner or User Access
/// Administrator), which is a different thing from being able to read.
pub fn grant_self_log_analytics_reader(workspace: &Workspace) -> Result<(), String> {
    let principal_id = signed_in_principal_id()?;
    let output = az_command(&[
        "role",
        "assignment",
        "create",
        "--subscription",
        &workspace.subscription_id,
        "--role",
        "Log Analytics Reader",
        "--assignee-object-id",
        &principal_id,
        "--assignee-principal-type",
        "User",
        "--scope",
        &workspace.resource_id(),
    ])
    .output()
    .map_err(|e| format!("az role assignment create failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

// ── Key Vault references ──────────────────────────────────────────────────

/// One `@Microsoft.KeyVault(...)` app setting, and whether it resolved.
///
/// This is the root-cause half of the exceptions view. A reference that
/// fails to resolve leaves the setting *empty* rather than absent, so the
/// app starts, reads a blank connection string, and throws something that
/// says nothing about Key Vault. Catching it here names the cause before
/// anyone has to reverse-engineer it from a stack trace.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyVaultRef {
    pub app: String,
    pub setting: String,
    pub status: String,
    pub details: String,
}

impl KeyVaultRef {
    pub fn resolved(&self) -> bool {
        self.status.eq_ignore_ascii_case("Resolved")
    }
}

/// Azure resource names are a restricted alphabet. These names arrive from
/// `AppRoleName` in log data, which is written by the monitored app rather
/// than by Azure, so they are checked before being handed to the CLI.
///
/// The leading-hyphen rule is the one that matters. Arguments are passed as
/// a vector rather than through a shell, so there is no shell injection to
/// worry about — but a value that begins with `-` is read by `az` as an
/// *option* rather than as the name, which is enough to turn a log line into
/// a different command. Real Azure resource names never start with one.
fn plausible_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 90
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Resolves an app name to its ARM resource id within one subscription.
///
/// Scoped to the workspace's own subscription deliberately: searching every
/// subscription for every name seen in a log window is slow and mostly
/// wrong, and an app almost always shares a subscription with the App
/// Insights component it writes to.
fn find_site(subscription: &str, name: &str) -> Option<String> {
    if !plausible_resource_name(name) {
        return None;
    }
    let output = az_command(&[
        "resource",
        "list",
        "--name",
        name,
        "--resource-type",
        "Microsoft.Web/sites",
        "--subscription",
        subscription,
        "--query",
        "[].id",
        "--output",
        "json",
    ])
    .output()
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let ids: Vec<String> = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).ok()?;
    ids.into_iter().next()
}

/// Every Key Vault-backed app setting on one site, with its resolution state.
fn site_key_vault_refs(app: &str, resource_id: &str) -> Result<Vec<KeyVaultRef>, String> {
    let url = format!(
        "https://management.azure.com{resource_id}/config/configreferences/appsettings\
?api-version=2022-03-01"
    );
    let output = az_command(&["rest", "--method", "get", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .map_err(|e| format!("parse: {e}"))?;
    Ok(parse_config_references(app, &body))
}

/// Reads the reference list out of a `configreferences/appsettings` response.
///
/// The payload keys settings by name under `properties`. Both the flat and
/// the `configReferences`-wrapped shapes are accepted, and anything without
/// a status is skipped rather than guessed at, so an API revision degrades
/// to reporting nothing instead of reporting fiction.
fn parse_config_references(app: &str, body: &serde_json::Value) -> Vec<KeyVaultRef> {
    let props = body
        .get("properties")
        .and_then(|p| p.get("configReferences").or(Some(p)));
    let Some(serde_json::Value::Object(map)) = props else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (setting, entry) in map {
        let Some(status) = entry.get("status").and_then(serde_json::Value::as_str) else {
            continue;
        };
        out.push(KeyVaultRef {
            app: app.to_string(),
            setting: setting.clone(),
            status: status.to_string(),
            details: entry
                .get("details")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    out.sort_by(|a, b| a.setting.cmp(&b.setting));
    out
}

/// Checks the Key Vault references of every named app, returning only the
/// ones that are *not* resolved.
///
/// Names that resolve to nothing are skipped silently: an `AppRoleName` need
/// not be an App Service at all, and "we could not find a resource by that
/// name" is not a finding worth showing anyone.
pub fn unresolved_key_vault_refs(subscription: &str, apps: &[String]) -> Vec<KeyVaultRef> {
    let mut out = Vec::new();
    for app in apps {
        let Some(resource_id) = find_site(subscription, app) else {
            continue;
        };
        if let Ok(refs) = site_key_vault_refs(app, &resource_id) {
            out.extend(refs.into_iter().filter(|r| !r.resolved()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_deserialize_from_the_az_projection() {
        let body = r#"[{"name":"law-prod","resourceGroup":"rg-obs",
                        "customerId":"11111111-2222-3333-4444-555555555555"}]"#;
        let list: Vec<Workspace> = serde_json::from_str(body).unwrap();
        assert_eq!(list[0].customer_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(list[0].resource_group, "rg-obs");
    }

    #[test]
    fn resource_ids_are_assembled_for_role_assignment() {
        let ws = Workspace {
            name: "law-prod".into(),
            resource_group: "rg-obs".into(),
            customer_id: "guid".into(),
            subscription_id: "sub-1".into(),
        };
        assert_eq!(
            ws.resource_id(),
            "/subscriptions/sub-1/resourceGroups/rg-obs/providers/\
             Microsoft.OperationalInsights/workspaces/law-prod"
        );
    }

    /// Remembered workspaces round-trip, so a recent-workspace chip still
    /// carries the GUID the query API needs after a restart.
    #[test]
    fn workspaces_round_trip_through_history() {
        let ws = Workspace {
            name: "law-prod".into(),
            resource_group: "rg-obs".into(),
            customer_id: "guid".into(),
            subscription_id: "sub-1".into(),
        };
        let text = serde_json::to_string(&ws).unwrap();
        assert_eq!(serde_json::from_str::<Workspace>(&text).unwrap(), ws);
    }

    #[test]
    fn log_written_names_are_checked_before_reaching_the_cli() {
        assert!(plausible_resource_name("fn-orders-prod"));
        assert!(plausible_resource_name("my_app.v2"));
        // AppRoleName is written by the monitored app, not by Azure.
        assert!(!plausible_resource_name(""));
        // Passed as an argv entry, so this is not shell injection — but `az`
        // would read it as an option and run a different query.
        assert!(!plausible_resource_name("--query"));
        assert!(!plausible_resource_name("-n"));
        assert!(!plausible_resource_name("a b"));
        assert!(!plausible_resource_name("app;rm -rf /"));
        assert!(!plausible_resource_name(&"x".repeat(91)));
    }

    #[test]
    fn unresolved_references_are_separated_from_healthy_ones() {
        let body = serde_json::json!({
            "properties": {
                "SqlConnection": {
                    "status": "InitializationError",
                    "details": "Key Vault reference could not be resolved"
                },
                "StorageConnection": { "status": "Resolved", "details": "" }
            }
        });
        let refs = parse_config_references("fn-orders", &body);
        assert_eq!(refs.len(), 2);
        let bad: Vec<&KeyVaultRef> = refs.iter().filter(|r| !r.resolved()).collect();
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].setting, "SqlConnection");
        assert_eq!(bad[0].app, "fn-orders");
    }

    #[test]
    fn the_wrapped_payload_shape_is_also_accepted() {
        let body = serde_json::json!({
            "properties": { "configReferences": {
                "Secret": { "status": "Resolved" }
            }}
        });
        assert_eq!(parse_config_references("app", &body).len(), 1);
    }

    /// An API that changes shape should make this report nothing, never
    /// invent a status it did not receive.
    #[test]
    fn an_unrecognised_payload_reports_nothing() {
        assert!(parse_config_references("app", &serde_json::json!({})).is_empty());
        assert!(parse_config_references("app", &serde_json::json!({"properties": []})).is_empty());
        // A setting with no status at all is skipped, not defaulted.
        let no_status = serde_json::json!({"properties": {"S": {"vaultName": "v"}}});
        assert!(parse_config_references("app", &no_status).is_empty());
    }

    #[test]
    fn resolution_status_is_matched_case_insensitively() {
        let r = |s: &str| KeyVaultRef {
            app: "a".into(),
            setting: "s".into(),
            status: s.into(),
            details: String::new(),
        };
        assert!(r("Resolved").resolved());
        assert!(r("resolved").resolved());
        assert!(!r("InitializationError").resolved());
    }
}
