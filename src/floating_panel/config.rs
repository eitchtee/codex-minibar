use serde::{Deserialize, Serialize};

use crate::settings::ProviderKind;

pub const MAX_METRICS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelPresentation {
    Numbers,
    #[default]
    Bars,
    Rings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl PanelSize {
    pub fn scale(self) -> f64 {
        match self {
            Self::Small => 0.85,
            Self::Medium => 1.0,
            Self::Large => 1.2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelHotkey {
    None,
    #[default]
    CtrlAltM,
    CtrlShiftM,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelPosition {
    /// Virtual-screen physical coordinates, including negative monitor origins.
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelMetric {
    pub provider: ProviderKind,
    pub metric_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FloatingPanelSettings {
    pub enabled: bool,
    pub always_on_top: bool,
    pub lock_position: bool,
    pub show_used: bool,
    pub show_reset: bool,
    pub presentation: PanelPresentation,
    pub size: PanelSize,
    pub hotkey: PanelHotkey,
    pub position: Option<PanelPosition>,
    pub metrics: Vec<PanelMetric>,
}

impl Default for FloatingPanelSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            always_on_top: true,
            lock_position: false,
            show_used: false,
            show_reset: true,
            presentation: PanelPresentation::Bars,
            size: PanelSize::Medium,
            hotkey: PanelHotkey::CtrlAltM,
            position: None,
            metrics: vec![
                PanelMetric {
                    provider: ProviderKind::Codex,
                    metric_id: "codex.session".into(),
                },
                PanelMetric {
                    provider: ProviderKind::Codex,
                    metric_id: "codex.weekly".into(),
                },
            ],
        }
    }
}

impl FloatingPanelSettings {
    pub fn normalize(&mut self) -> bool {
        let before = self.clone();
        let mut seen = std::collections::HashSet::new();
        self.metrics.retain(|metric| {
            metric
                .metric_id
                .starts_with(&format!("{}.", metric.provider.id()))
                && metric.metric_id.len() <= 160
                && seen.insert((metric.provider, metric.metric_id.clone()))
        });
        self.metrics.truncate(MAX_METRICS);
        *self != before
    }
}
