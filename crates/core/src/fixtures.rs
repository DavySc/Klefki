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
    casing_collision: bool,
}

pub(crate) fn load() -> Snapshot {
    let raw: RawSnapshot = serde_json::from_str(RAW).expect("fixtures/ui-snapshot.json is valid");
    Snapshot {
        cli_version: raw.cli_version.map(|v| sanitize(&v)),
        project_count: raw.project_count,
        projects: Vec::new(),
        servers: raw
            .servers
            .into_iter()
            .map(|s| ServerRow {
                name: sanitize(&s.name),
                scope: match s.scope.as_str() {
                    "user" => Scope::User,
                    "local" => Scope::Local,
                    _ => Scope::Project,
                },
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
                casing_collision: s.casing_collision,
            })
            .collect(),
        warnings: Vec::new(),
    }
}
