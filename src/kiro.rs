//! Reads the subscription-credit snapshot Kiro IDE already stores locally.
//!
//! The IDE's `state.vscdb` is opened read-only. Minibar does not read Kiro
//! credentials, call private endpoints, or write to Kiro's database.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Months, Utc};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

use crate::{
    limits::{AdditionalLimit, LimitWindow, RateLimits},
    usage::UsageStatistics,
    worker::{Activator, LimitProvider, UsageProvider},
};

const STATE_KEY: &str = "kiro.kiroAgent";
const USAGE_STATE_KEY: &str = "kiro.resourceNotifications.usageState";
const CREDIT_METRIC_ID: &str = "credits";

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
        let remaining = (limit - used).max(0.0);
        let resets_at = credits
            .get("resetDate")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        let duration_minutes = resets_at.and_then(month_duration_minutes);

        Ok(RateLimits {
            sampled_at,
            credits: crate::limits::Credits {
                has_credits: true,
                unlimited: false,
                balance: Some(format!("{} included", format_credit_amount(remaining))),
            },
            additional_limits: vec![AdditionalLimit {
                id: CREDIT_METRIC_ID.into(),
                title: "Monthly credits".into(),
                window: LimitWindow {
                    used_percent: Some(used_percent),
                    resets_at,
                    duration_minutes,
                },
            }],
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

fn state_database_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|base| {
        base.config_dir()
            .join("Kiro")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    })
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

fn format_credit_amount(amount: f64) -> String {
    format!("{amount:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn month_duration_minutes(resets_at: DateTime<Utc>) -> Option<u32> {
    let start = resets_at.checked_sub_months(Months::new(1))?;
    u32::try_from((resets_at - start).num_minutes()).ok()
}
