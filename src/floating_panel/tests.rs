use super::*;
use crate::{
    limits::{AdditionalLimit, LimitWindow, RateLimits},
    settings::{ProviderKind, Settings},
};
use chrono::{Duration, TimeZone};

#[test]
fn panel_metrics_match_the_canonical_resolver_and_preserve_reserve_reset() {
    let now = Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap();
    let primary_reset = now + Duration::hours(2);
    let quota = RateLimits {
        primary: LimitWindow {
            used_percent: Some(100),
            resets_at: Some(primary_reset),
            duration_minutes: Some(300),
        },
        secondary: LimitWindow {
            used_percent: Some(40),
            resets_at: Some(now + Duration::days(3)),
            duration_minutes: Some(10080),
        },
        additional_limits: vec![AdditionalLimit {
            id: "gpt-reserve".into(),
            title: "Reserve".into(),
            window: LimitWindow {
                used_percent: Some(12),
                resets_at: Some(now + Duration::minutes(10)),
                duration_minutes: Some(60),
            },
        }],
        ..Default::default()
    };
    let config = FloatingPanelSettings::default();
    let mut providers = ProviderSettings::default();
    providers.set_enabled(ProviderKind::Codex, true);
    let limits = ProviderLimits::from_entries([(ProviderKind::Codex, quota.clone())]);
    let result = rows(&config, &providers, &limits, now);
    for (row, selection) in result.iter().zip(&config.metrics) {
        let canonical =
            crate::widget_data::resolve_metric(selection.provider, &quota, &selection.metric_id)
                .unwrap();
        assert_eq!(row.value, canonical.window.remaining_percent());
        assert_eq!(row.label, canonical.label);
    }
    assert_eq!(result[0].value, Some(88));
    assert_eq!(result[0].detail, "Reset in 2h 0m");
}

#[test]
fn disabled_and_missing_sources_are_never_presented_as_zero_usage() {
    let now = Utc::now();
    let config = FloatingPanelSettings::default();
    let mut providers = ProviderSettings::default();
    let disabled = rows(&config, &providers, &ProviderLimits::default(), now);
    assert!(
        disabled
            .iter()
            .all(|row| row.value.is_none() && row.detail == "Provider disabled")
    );
    providers.set_enabled(ProviderKind::Codex, true);
    let missing = rows(&config, &providers, &ProviderLimits::default(), now);
    assert!(
        missing
            .iter()
            .all(|row| row.value.is_none() && row.detail == "No quota data")
    );
}

#[test]
fn source_identity_survives_every_provider_enable_combination() {
    let config = FloatingPanelSettings {
        metrics: ProviderKind::ALL
            .iter()
            .filter_map(|provider| {
                crate::provider_registry::descriptor(*provider)
                    .metrics
                    .first()
                    .map(|metric| PanelMetric {
                        provider: *provider,
                        metric_id: metric.id.into(),
                    })
            })
            .collect(),
        ..Default::default()
    };
    let now = Utc::now();
    let limits = ProviderLimits::from_entries(ProviderKind::ALL.into_iter().map(|provider| {
        (
            provider,
            RateLimits {
                primary: LimitWindow {
                    used_percent: Some(30),
                    ..Default::default()
                },
                secondary: LimitWindow {
                    used_percent: Some(70),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    }));
    for mask in 0..(1 << ProviderKind::ALL.len()) {
        let mut providers = ProviderSettings::default();
        for (index, provider) in ProviderKind::ALL.into_iter().enumerate() {
            providers.set_enabled(provider, mask & (1 << index) != 0);
        }
        let result = rows(&config, &providers, &limits, now);
        assert_eq!(result.len(), config.metrics.len());
        for (row, metric) in result.iter().zip(&config.metrics) {
            assert_eq!(row.provider, metric.provider);
            assert_eq!(
                row.id,
                format!("{}:{}", metric.provider.id(), metric.metric_id)
            );
            if !providers.is_enabled(metric.provider) {
                assert_eq!(row.value, None);
                assert_eq!(row.detail, "Provider disabled");
            }
        }
    }
}

#[test]
fn normalization_preserves_unknown_live_lanes_but_bounds_and_deduplicates_selections() {
    let mut config = FloatingPanelSettings::default();
    config.metrics.push(config.metrics[0].clone());
    config.metrics.push(PanelMetric {
        provider: ProviderKind::Claude,
        metric_id: "codex.session".into(),
    });
    for index in 0..20 {
        config.metrics.push(PanelMetric {
            provider: ProviderKind::Claude,
            metric_id: format!("claude.additional.lane-{index}"),
        });
    }
    assert!(config.normalize());
    assert_eq!(config.metrics.len(), MAX_METRICS);
    assert_eq!(config.metrics[2].metric_id, "claude.additional.lane-0");
    assert!(!config.normalize());
}

#[test]
fn overdue_reset_keeps_the_reported_quota_until_a_real_refresh() {
    let now = Utc::now();
    assert_eq!(
        reset_countdown(now - Duration::seconds(1), now),
        "Reset due · awaiting refresh"
    );
    assert_eq!(
        reset_countdown(now + Duration::seconds(1), now),
        "Reset in 1m"
    );
}

#[test]
fn settings_migration_is_opt_in_and_panel_preferences_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    let mut settings = Settings::default();
    settings.providers.set_enabled(ProviderKind::Claude, true);
    let mut document = toml::Value::try_from(&settings).unwrap();
    let root = document.as_table_mut().unwrap();
    root.remove("floating_panel");
    root.insert("version".into(), toml::Value::Integer(38));
    std::fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
    let mut migrated = Settings::load_or_create(&path).unwrap();
    assert!(!migrated.floating_panel.enabled);
    assert!(migrated.providers.is_enabled(ProviderKind::Claude));
    assert_eq!(migrated.version, crate::settings::SETTINGS_VERSION);
    migrated.floating_panel.enabled = true;
    migrated.floating_panel.position = Some(PanelPosition { x: -1700, y: 50 });
    migrated.floating_panel.hotkey = PanelHotkey::None;
    migrated.floating_panel.presentation = PanelPresentation::Rings;
    migrated.floating_panel.metrics.clear();
    migrated.save(&path).unwrap();
    assert_eq!(
        Settings::load_or_create(&path).unwrap().floating_panel,
        migrated.floating_panel
    );
}

#[test]
fn broken_panel_preferences_do_not_reset_other_settings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    let mut settings = Settings::default();
    settings.providers.set_enabled(ProviderKind::Claude, true);
    settings.show_account_name = true;
    let mut document = toml::Value::try_from(&settings).unwrap();
    document["floating_panel"]["presentation"] = toml::Value::String("broken".into());
    std::fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
    let restored = Settings::load_or_create(&path).unwrap();
    assert!(!restored.floating_panel.enabled);
    assert!(restored.providers.is_enabled(ProviderKind::Claude));
    assert!(restored.show_account_name);
}

#[cfg(windows)]
#[test]
fn position_clamping_supports_negative_monitors_and_disconnected_displays() {
    use windows_sys::Win32::Foundation::RECT;
    let work = RECT {
        left: -1920,
        top: 0,
        right: 0,
        bottom: 1040,
    };
    assert_eq!(
        native::clamp_position(PanelPosition { x: -1800, y: 50 }, 300, 200, work),
        PanelPosition { x: -1800, y: 50 }
    );
    assert_eq!(
        native::clamp_position(PanelPosition { x: 5000, y: 5000 }, 300, 200, work),
        PanelPosition { x: -300, y: 840 }
    );
    assert_eq!(
        native::clamp_position(PanelPosition { x: -5000, y: -100 }, 3000, 2000, work),
        PanelPosition { x: -1920, y: 0 }
    );
}
