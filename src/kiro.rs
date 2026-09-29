//! Reads the subscription-credit snapshot Kiro IDE already stores locally.
//!
//! The IDE's `state.vscdb` is opened read-only. Plan and account labels come
//! from local usage logs when available; Minibar never opens Kiro auth-token
//! files, calls private endpoints, or writes to Kiro's data.

use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
#[cfg(windows)]
use std::env;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Months, Utc};
use rusqlite::{Connection, OpenFlags, params};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    limits::{LimitWindow, RateLimits},
    usage::UsageStatistics,
    worker::{Activator, LimitProvider, UsageProvider},
};

const STATE_KEY: &str = "kiro.kiroAgent";
const USAGE_STATE_KEY: &str = "kiro.resourceNotifications.usageState";
const USAGE_COMMAND: &str = "GetUsageLimitsCommand";

#[derive(Clone, Default)]
pub struct KiroClient {
    state_path: Option<PathBuf>,
}

impl KiroClient {
    pub fn new() -> Self {
        Self {
            state_path: state_database_path(),
        }
    }

    fn read_usage_limits(&self) -> Result<RateLimits> {
        let path = self
            .state_path
            .as_deref()
            .ok_or_else(|| anyhow!("Could not locate the Kiro IDE user-data directory"))?;
        if !path.is_file() {
            return Err(anyhow!(
                "Kiro IDE usage data was not found. Sign in to Kiro and open it once to load your credits."
            ));
        }

        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("Could not open Kiro IDE's local usage cache read-only")?;
        connection
            .busy_timeout(Duration::from_millis(750))
            .context("Could not wait for Kiro IDE's usage cache")?;
        let raw: String = connection
            .query_row(
                "SELECT value FROM ItemTable WHERE key = ?1",
                params![STATE_KEY],
                |row| row.get(0),
            )
            .context("Kiro IDE has not saved its local usage state yet")?;
        let state: Value = serde_json::from_str(&raw)
            .context("Kiro IDE's local usage state has an unsupported format")?;
        let usage_state = state
            .get(USAGE_STATE_KEY)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                anyhow!(
                    "Kiro IDE has not cached subscription credits yet. Sign in and open Kiro to refresh its usage panel."
                )
            })?;
        let sampled_at = usage_state
            .get("timestamp")
            .and_then(Value::as_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .ok_or_else(|| anyhow!("Kiro IDE's usage cache has no valid sample time"))?;
        let breakdowns = usage_state
            .get("usageBreakdowns")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Kiro IDE's usage cache has no credit breakdowns"))?;
        let credits = breakdowns
            .iter()
            .find(|breakdown| is_credit_breakdown(breakdown))
            .ok_or_else(|| anyhow!("Kiro IDE's usage cache has no monthly credit entry"))?;

        let used = numeric_field(credits, "currentUsage")
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| anyhow!("Kiro IDE's monthly credit usage is unavailable"))?;
        let limit = numeric_field(credits, "usageLimit")
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| anyhow!("Kiro IDE's monthly credit limit is unavailable"))?;
        let used_percent = ((used / limit) * 100.0).round().clamp(0.0, 100.0) as u8;
        let resets_at = credits
            .get("resetDate")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        let duration_minutes = resets_at.and_then(month_duration_minutes);
        let metadata = local_runtime_metadata();

        Ok(RateLimits {
            sampled_at,
            account_name: metadata.account_name,
            plan_type: metadata.plan_name,
            secondary: LimitWindow {
                used_percent: Some(used_percent),
                resets_at,
                duration_minutes,
            },
            ..RateLimits::default()
        })
    }
}

impl LimitProvider for KiroClient {
    fn read_limits(&mut self) -> Result<RateLimits> {
        self.read_usage_limits()
    }
}

impl UsageProvider for KiroClient {
    fn load_cached_usage_statistics(&mut self, _history_days: u16) -> Result<UsageStatistics> {
        Ok(UsageStatistics::default())
    }

    fn refresh_usage_statistics(&mut self, _history_days: u16) -> Result<UsageStatistics> {
        Ok(UsageStatistics::default())
    }
}

pub struct KiroActivator;

impl Activator for KiroActivator {
    fn activate(&mut self) -> Result<()> {
        Err(anyhow!("Kiro does not expose a supported session activation command"))
    }
}

/// Used for Settings and onboarding readiness without accessing credentials.
pub fn has_cached_usage() -> bool {
    KiroClient::new().read_usage_limits().is_ok()
}

/// Finds the Kiro executable for the Settings source row. If Kiro is installed
/// outside the usual Windows locations, fall back to the local state database
/// that Minibar reads for its usage snapshot.
pub fn detected_source_path() -> Option<PathBuf> {
    installation_path().or_else(|| state_database_path().filter(|path| path.is_file()))
}

fn installation_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let local_app_data = env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let program_files = env::var_os("ProgramFiles").map(PathBuf::from);
        let program_files_x86 = env::var_os("ProgramFiles(x86)").map(PathBuf::from);
        local_app_data
            .into_iter()
            .map(|path| path.join("Programs/Kiro/Kiro.exe"))
            .chain(program_files.into_iter().map(|path| path.join("Kiro/Kiro.exe")))
            .chain(
                program_files_x86
                    .into_iter()
                    .map(|path| path.join("Kiro/Kiro.exe")),
            )
            .find(|path| path.is_file())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn state_database_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|base| {
        base.config_dir()
            .join("Kiro")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    })
}

#[derive(Default)]
struct KiroRuntimeMetadata {
    user_id: Option<String>,
    plan_name: Option<String>,
    account_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KiroLogEntry {
    command_name: Option<String>,
    output: Option<KiroUsageResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KiroUsageResponse {
    subscription_info: Option<KiroSubscriptionInfo>,
    user_info: Option<KiroUserInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KiroSubscriptionInfo {
    subscription_title: Option<String>,
    #[serde(rename = "type")]
    subscription_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KiroUserInfo {
    user_id: Option<String>,
    account_name: Option<String>,
    display_name: Option<String>,
    name: Option<String>,
    user_name: Option<String>,
    username: Option<String>,
    email: Option<String>,
}

/// Kiro logs the quota response locally. Read only its plan and identity labels;
/// unrelated request fields (including any future auth fields) are ignored by
/// the typed deserializer and are never copied into Minibar state.
fn local_runtime_metadata() -> KiroRuntimeMetadata {
    let mut logs = q_client_log_paths();
    logs.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    logs.reverse();

    let mut selected = KiroRuntimeMetadata::default();
    for path in logs {
        let Some(candidate) = latest_usage_metadata_in_log(&path) else {
            continue;
        };
        let same_user = match (&selected.user_id, &candidate.user_id) {
            (Some(selected), Some(candidate)) => selected == candidate,
            (Some(_), None) => false,
            _ => true,
        };
        if selected.plan_name.is_none() {
            selected.plan_name = candidate.plan_name;
        }
        if selected.user_id.is_none() {
            selected.user_id = candidate.user_id;
        }
        if same_user && selected.account_name.is_none() {
            selected.account_name = candidate.account_name;
        }
        if selected.plan_name.is_some() && selected.account_name.is_some() {
            break;
        }
    }
    selected
}

fn q_client_log_paths() -> Vec<PathBuf> {
    let Some(base) = directories::BaseDirs::new() else {
        return Vec::new();
    };
    let logs_root = base.config_dir().join("Kiro").join("logs");
    let Ok(sessions) = fs::read_dir(logs_root) else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    for session in sessions.flatten() {
        if !session.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(windows) = fs::read_dir(session.path()) else {
            continue;
        };
        for window in windows.flatten() {
            if !window.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = window
                .path()
                .join("exthost")
                .join("kiro.kiroAgent")
                .join("q-client.log");
            if path.is_file() {
                paths.push(path);
            }
        }
    }
    paths
}

fn latest_usage_metadata_in_log(path: &Path) -> Option<KiroRuntimeMetadata> {
    let file = fs::File::open(path).ok()?;
    let mut latest = None;
    for line in BufReader::new(file)
        .lines()
        .filter_map(std::result::Result::ok)
    {
        if !line.contains(USAGE_COMMAND) || !line.contains("subscriptionInfo") {
            continue;
        }
        let Some(start) = line.find('{') else {
            continue;
        };
        let mut deserializer = serde_json::Deserializer::from_str(&line[start..]);
        let Ok(entry) = KiroLogEntry::deserialize(&mut deserializer) else {
            continue;
        };
        if entry.command_name.as_deref() != Some(USAGE_COMMAND) {
            continue;
        }
        let Some(output) = entry.output else {
            continue;
        };

        let plan_name = output
            .subscription_info
            .as_ref()
            .and_then(display_plan_name);
        let user = output.user_info;
        let candidate = KiroRuntimeMetadata {
            user_id: user.as_ref().and_then(|user| nonempty(user.user_id.as_deref())),
            plan_name,
            account_name: user.as_ref().and_then(account_display_name),
        };
        if candidate.plan_name.is_some()
            || candidate.account_name.is_some()
            || candidate.user_id.is_some()
        {
            if let Some(current) = latest.as_mut() {
                merge_log_metadata(current, candidate);
            } else {
                latest = Some(candidate);
            }
        }
    }
    latest
}

fn merge_log_metadata(current: &mut KiroRuntimeMetadata, candidate: KiroRuntimeMetadata) {
    let user_changed = matches!(
        (&current.user_id, &candidate.user_id),
        (Some(current), Some(candidate)) if current != candidate
    );
    if user_changed {
        *current = candidate;
        return;
    }
    if current.user_id.is_none() && candidate.user_id.is_some() {
        // An earlier label without an id cannot be safely tied to this user.
        current.account_name = None;
        current.user_id = candidate.user_id;
    }
    if candidate.plan_name.is_some() {
        current.plan_name = candidate.plan_name;
    }
    if candidate.account_name.is_some() {
        current.account_name = candidate.account_name;
    }
}

fn display_plan_name(subscription: &KiroSubscriptionInfo) -> Option<String> {
    subscription
        .subscription_title
        .as_deref()
        .and_then(normalize_plan_name)
        .or_else(|| {
            subscription
                .subscription_type
                .as_deref()
                .and_then(known_plan_from_type)
        })
}

fn normalize_plan_name(value: &str) -> Option<String> {
    let value = value.trim();
    let value = if value
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("kiro "))
    {
        &value[5..]
    } else {
        value
    }
    .trim();
    nonempty(Some(value))
}

fn known_plan_from_type(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
        "free" => Some("Free".into()),
        "pro" => Some("Pro".into()),
        "proplus" => Some("Pro+".into()),
        "promax" => Some("Pro Max".into()),
        "power" => Some("Power".into()),
        _ => None,
    }
}

fn account_display_name(user: &KiroUserInfo) -> Option<String> {
    first_nonempty([
        user.account_name.as_deref(),
        user.display_name.as_deref(),
        user.name.as_deref(),
        user.user_name.as_deref(),
        user.username.as_deref(),
        user.email.as_deref(),
    ])
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    values
        .into_iter()
        .find_map(nonempty)
}

fn nonempty(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || is_redaction_marker(value) {
        None
    } else {
        Some(value.to_owned())
    }
}

fn is_redaction_marker(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "sensitiveinformation" | "redacted" | "redactedvalue"
    )
}

fn is_credit_breakdown(value: &Value) -> bool {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
    let display_name = value
        .get("displayName")
        .or_else(|| value.get("displayNamePlural"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    kind.eq_ignore_ascii_case("CREDIT")
        || display_name.to_ascii_lowercase().contains("credit")
}

fn numeric_field(value: &Value, name: &str) -> Option<f64> {
    value.get(name).and_then(Value::as_f64)
}

fn month_duration_minutes(resets_at: DateTime<Utc>) -> Option<u32> {
    let start = resets_at.checked_sub_months(Months::new(1))?;
    u32::try_from((resets_at - start).num_minutes()).ok()
}
