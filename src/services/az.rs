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
    LoggedIn { account: String, subscription_id: String },
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
                Ok(acc) => AzLoginState::LoggedIn { account: acc.name, subscription_id: acc.id },
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
    az_command(&["login"])
        .spawn()
        .map(|_| ())
        .map_err(|e| {
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
                "monitor", "log-analytics", "workspace", "list",
                "--subscription", sub_id.as_str(),
                "--query", "[].{name:name,resourceGroup:resourceGroup,customerId:customerId}",
                "--output", "json",
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
    let output = az_command(&["ad", "signed-in-user", "show", "--query", "id", "--output", "tsv"])
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
            "role", "assignment", "create",
            "--subscription", &workspace.subscription_id,
            "--role", "Log Analytics Reader",
            "--assignee-object-id", &principal_id,
            "--assignee-principal-type", "User",
            "--scope", &workspace.resource_id(),
        ])
        .output()
        .map_err(|e| format!("az role assignment create failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
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
}
