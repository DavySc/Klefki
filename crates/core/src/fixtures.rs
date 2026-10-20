//! Loads `fixtures/ui-snapshot.json` (embedded) through the sanitising path real data will use.
use crate::api::{Approval, Scope, ServerRow, Snapshot, Status, TransportKind};
use crate::sanitize::sanitize;
use serde::Deserialize;

const RAW: &str = include_str!("../../../fixtures/ui-snapshot.json");

#[derive(Deserialize)]
struct RawSnapshot {
    cli_version: Option<String>,
    #[serde(default)]
    project_count: usize,
    servers: Vec<RawServer>,
}

#[derive(Deserialize)]
struct RawServer {
    name: String,
    scope: String,
    project: Option<String>,
    transport: String,
    target: String,
    status: String,
    enabled: bool,
    #[serde(default)]
    disabled_in: usize,
    approval: Option<String>,
    #[serde(default)]
    shadowed: bool,
    #[serde(default)]
    shadowed_in: Vec<String>,
    #[serde(default)]
    casing_collision: bool,
    #[serde(default)]
    relative_paths: Vec<String>,
    /// Plugin servers: `"<plugin>@<marketplace>"`.
    plugin: Option<String>,
    same_as: Option<String>,
    #[serde(default)]
    written_target: Option<String>,
    /// Plugin servers: the projects where the plugin is off.
    #[serde(default)]
    plugin_off_in: Vec<String>,
    /// Organisation servers: the file that sets it.
    managed: Option<String>,
    not_loaded: Option<String>,
}

pub(crate) fn load() -> Snapshot {
    let raw: RawSnapshot = serde_json::from_str(RAW).expect("fixtures/ui-snapshot.json is valid");
    Snapshot {
        cli_version: raw.cli_version.map(|v| sanitize(&v)),
        project_count: raw.project_count,
        projects: Vec::new(),
        legacy_disabled: Vec::new(),
        kept: Vec::new(),
        organisation_only: None,
        organisation_notes: Vec::new(),
        servers: raw
            .servers
            .into_iter()
            .map(|s| ServerRow {
                connection: crate::model::ConnectionId::new(
                    scope_of(&s.scope),
                    s.project.as_deref(),
                    &s.name,
                ),
                name: sanitize(&s.name),
                scope: scope_of(&s.scope),
                project: s.project.map(|p| sanitize(&p)),
                transport: match s.transport.as_str() {
                    "stdio" => TransportKind::Stdio,
                    "http" => TransportKind::Http,
                    "sse" => TransportKind::Sse,
                    "ws" => TransportKind::Ws,
                    _ => TransportKind::Invalid,
                },
                target: sanitize(&s.target),
                status: match s.status.as_str() {
                    "connected" => Status::Connected,
                    "needs_auth" => Status::NeedsAuth,
                    "failed" => Status::Failed,
                    "degraded" => Status::Degraded,
                    "pending_approval" => Status::PendingApproval,
                    "not_configured" => Status::NotConfigured,
                    "checking" => Status::Checking,
                    _ => Status::Unknown,
                },
                enabled: s.enabled,
                disabled_in: s.disabled_in,
                approval: s.approval.as_deref().map(|a| match a {
                    "approved" => Approval::Approved,
                    "rejected" => Approval::Rejected,
                    _ => Approval::Pending,
                }),
                shadowed: s.shadowed,
                shadowed_in: s.shadowed_in.iter().map(|p| sanitize(p)).collect(),
                casing_collision: s.casing_collision,
                relative_paths: s.relative_paths.iter().map(|p| sanitize(p)).collect(),
                status_cached: false,
                oauth: None,
                plugin: s.plugin.as_deref().map(|p| {
                    let (plugin, marketplace) = p.split_once('@').unwrap_or((p, ""));
                    crate::api::PluginInfo {
                        plugin: sanitize(plugin),
                        marketplace: sanitize(marketplace),
                        same_as: s.same_as.as_deref().map(sanitize),
                        written_target: s.written_target.as_deref().map(sanitize),
                        off_in: s.plugin_off_in.iter().map(|p| sanitize(p)).collect(),
                    }
                }),
                managed: s.managed.as_deref().map(sanitize),
                not_loaded: s.not_loaded.as_deref().map(sanitize),
                blocked: None,
            })
            .collect(),
        warnings: Vec::new(),
    }
}

fn scope_of(word: &str) -> Scope {
    match word {
        "user" => Scope::User,
        "local" => Scope::Local,
        "plugin" => Scope::Plugin,
        "managed" => Scope::Managed,
        _ => Scope::Project,
    }
}
