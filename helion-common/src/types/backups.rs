use serde::{ Deserialize, Serialize };
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct StepStatus {
    pub ok: bool,
    /// RFC3339.
    pub at: Option<String>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct RetentionPolicy {
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct BackupSnapshotInfo {
    pub id: String,
    pub short_id: String,
    /// RFC3339.
    pub time: String,
    pub hostname: String,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    #[ts(type = "number | null")]
    pub size_bytes: Option<u64>,
}

/// What the host is asked to do. Written to the request file with hyphens
/// (`backup-state`), as the host's dispatcher expects.
#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum BackupRequestKind {
    /// The nightly backup, now.
    Backup,
    /// The dumps, volumes and `.env` only, into the state repository: what an
    /// update took first, and still does when no database matches.
    BackupState,
    /// Only the databases an update's services use, into the state
    /// repository: what an update takes first now.
    BackupDump,
    RestoreTest,
}

impl BackupRequestKind {
    /// The name in request and result files.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::BackupState => "backup-state",
            Self::BackupDump => "backup-dump",
            Self::RestoreTest => "restore-test",
        }
    }

    pub fn from_file_name(s: &str) -> Option<Self> {
        match s {
            "backup" => Some(Self::Backup),
            "backup-state" => Some(Self::BackupState),
            "backup-dump" => Some(Self::BackupDump),
            "restore-test" => Some(Self::RestoreTest),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum BackupRequestState {
    /// Waiting for the host to pick it up.
    Queued,
    Running,
    Succeeded,
    Failed,
}

/// A backup or restore test someone asked for, and how it went.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct BackupRequest {
    pub id: String,
    pub kind: BackupRequestKind,
    pub requested_by: Option<String>,
    pub state: BackupRequestState,
    /// RFC3339.
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    Pass,
    Warn,
    Fail,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct RestoreCheck {
    pub result: CheckResult,
    /// Never a path: the script writes path-free messages for this file.
    pub message: String,
}

/// The latest restore test: dumps loaded into throwaway databases, sampled
/// files compared with the live ones.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct RestoreTestStatus {
    /// RFC3339.
    pub finished_at: String,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub duration_secs: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub passed: u32,
    pub warned: u32,
    pub failed: u32,
    pub checks: Vec<RestoreCheck>,
}

/// A database the backup dumps, for the restore guide.
#[derive(Serialize, Deserialize, TS, Debug, Clone, PartialEq, Eq)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct BackupDatabase {
    /// The dump's name, e.g. `immich` for `immich.sql.gz`.
    pub name: String,
    /// The compose service it's loaded into.
    pub service: String,
    /// Services to stop while it's restored.
    pub used_by: Vec<String>,
}

/// `POST /v1/backups/run`.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct BackupRunInput {
    pub kind: BackupRequestKind,
}

/// Read-only view of a restic repository, assembled by the backup script on
/// the host and read from a status file by the agent.
///
/// Deliberately absent: the repository path, `RESTIC_PASSWORD`, and the
/// heartbeat push URL (it embeds a token). The agent never holds any of them,
/// so they cannot leak from here.
#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export, export_to = "../../app/src/generated/")]
pub struct BackupsStatus {
    /// RFC3339, written by the script.
    pub generated_at: String,
    /// Agent-computed: `generated_at` older than 1.5x the expected interval.
    ///
    /// The most important field here. A backup system that silently stopped
    /// running looks identical to a healthy one if all you render is a list of
    /// snapshots — they just quietly stop getting newer.
    pub stale: bool,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    /// Independent signal: mtime of the systemd timer stamp file. If this is
    /// newer than `generated_at`, the timer fired but the job died before it
    /// could write its status.
    pub timer_last_fired: Option<String>,
    pub last_exit_code: Option<i32>,
    #[ts(type = "number | null")]
    pub duration_secs: Option<u64>,
    /// A display name from config, never the real path.
    pub repo_label: String,
    #[ts(type = "number | null")]
    pub repo_size_bytes: Option<u64>,
    pub snapshot_count: u32,
    pub snapshots: Vec<BackupSnapshotInfo>,
    pub retention: Option<RetentionPolicy>,
    pub postgres_dump: Option<StepStatus>,
    /// The second copy of the state (dumps, volumes, config) on another disk.
    /// Absent from agents before 0.5, and null when the script doesn't make one.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub state_copy: Option<StepStatus>,
    /// The copy that leaves the machine (`restic copy` to another node or
    /// offsite). Null until the host's script makes one: the app says no
    /// copy leaves the machine. Absent from agents before 0.17.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub offsite_copy: Option<StepStatus>,
    /// Where the offsite copy goes, for display. Never a path or a URL with
    /// credentials. Absent from agents before 0.17.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub offsite_label: Option<String>,
    /// Whether the repository's filesystem had room for tonight's backup; the
    /// message says how much was free. Absent from agents before 0.5.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub space_check: Option<StepStatus>,
    pub heartbeat: Option<StepStatus>,
    /// Backups and restore tests asked for from Helion, newest first. Absent
    /// from agents before 0.7.
    #[serde(default)]
    #[ts(as = "Option<Vec<BackupRequest>>", optional)]
    pub requests: Vec<BackupRequest>,
    /// Absent from agents before 0.7, and null before the first test.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub restore_test: Option<RestoreTestStatus>,
    /// Absent from agents before 0.7.
    #[serde(default)]
    #[ts(as = "Option<Vec<BackupDatabase>>", optional)]
    pub databases: Vec<BackupDatabase>,
}
