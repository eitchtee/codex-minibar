//! Opt-in, independent quota panel. Provider polling stays in the existing workers.

mod config;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod runtime;
#[cfg(windows)]
mod view;

pub use config::*;

use crate::{limits::ProviderLimits, settings::ProviderSettings};
use chrono::{DateTime, Utc};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PanelRow {
    pub id: String,
    pub provider: crate::settings::ProviderKind,
    pub label: String,
    pub value: Option<u8>,
    pub detail: String,
}

pub(crate) fn rows(
    config: &FloatingPanelSettings,
    providers: &ProviderSettings,
    limits: &ProviderLimits,
    now: DateTime<Utc>,
) -> Vec<PanelRow> {
    config
        .metrics
        .iter()
        .map(|selection| {
            let provider_limits = limits.get(selection.provider);
            let enabled = providers.is_enabled(selection.provider);
            let metric = enabled
                .then(|| {
                    crate::widget_data::resolve_metric(
                        selection.provider,
                        provider_limits,
                        &selection.metric_id,
                    )
                })
                .flatten();
            let label = metric
                .as_ref()
                .map(|metric| metric.label.clone())
                .unwrap_or_else(|| {
                    crate::provider_registry::metric_label(
                        selection.provider,
                        provider_limits,
                        &selection.metric_id,
                    )
                });
            let value = metric.as_ref().and_then(|metric| {
                if config.show_used {
                    metric.window.used_percent.map(|value| value.min(100))
                } else {
                    metric.window.remaining_percent()
                }
            });
            let detail = if !enabled {
                "Provider disabled".into()
            } else if value.is_none() {
                "No quota data".into()
            } else if config.show_reset {
                metric
                    .as_ref()
                    .and_then(|metric| metric.window.resets_at)
                    .map(|reset| reset_countdown(reset, now))
                    .unwrap_or_else(|| "Reset time unavailable".into())
            } else if config.show_used {
                "used".into()
            } else {
                "remaining".into()
            };
            PanelRow {
                id: format!("{}:{}", selection.provider.id(), selection.metric_id),
                provider: selection.provider,
                label,
                value,
                detail,
            }
        })
        .collect()
}

fn reset_countdown(reset: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (reset - now).num_seconds();
    if seconds <= 0 {
        return "Reset due · awaiting refresh".into();
    }
    let minutes = (seconds + 59) / 60;
    if minutes >= 1440 {
        format!("Reset in {}d {}h", minutes / 1440, minutes % 1440 / 60)
    } else if minutes >= 60 {
        format!("Reset in {}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("Reset in {minutes}m")
    }
}

#[cfg(windows)]
pub(crate) use native::status;
#[cfg(windows)]
pub(crate) use runtime::refresh;
#[cfg(windows)]
pub(crate) use runtime::{begin_drag, hide, reset_position, show, sync, toggle};
#[cfg(not(windows))]
pub(crate) fn refresh(_dispatcher: windows_reactor::UiMarshaller) {}

#[cfg(not(windows))]
pub(crate) fn sync(
    _settings: crate::settings::Settings,
    _state: std::sync::Arc<crate::app::AppState>,
    _dispatcher: windows_reactor::UiMarshaller,
) {
}
#[cfg(not(windows))]
pub(crate) fn show() {}
#[cfg(not(windows))]
pub(crate) fn reset_position() {}
#[cfg(not(windows))]
pub(crate) fn status() -> Option<String> {
    None
}

#[cfg(test)]
mod tests;
