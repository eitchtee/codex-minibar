use super::*;
use super::{persistence::persist_update, shared::settings_section_heading};
use crate::floating_panel::{
    FloatingPanelSettings, MAX_METRICS, PanelHotkey, PanelMetric, PanelPresentation, PanelSize,
};

fn change<T: Clone + 'static>(
    ctx: &SettingsPageContext<'_>,
    update: impl Fn(&mut FloatingPanelSettings, T) + 'static,
) -> impl Fn(T) + 'static {
    let current = ctx.floating_panel.clone();
    let setter = ctx.set_floating_panel.clone();
    let tx = ctx.settings_tx.clone();
    move |value| {
        let mut next = current.clone();
        update(&mut next, value.clone());
        next.normalize();
        if next == current {
            return;
        }
        setter.call(next);
        // Mutate the latest on-disk value: dragging the panel must never be
        // undone by a settings callback holding an earlier position snapshot.
        persist_update(tx.clone(), |settings| {
            update(&mut settings.floating_panel, value);
            settings.floating_panel.normalize();
        });
    }
}

fn toggle(
    ctx: &SettingsPageContext<'_>,
    label: &str,
    description: &str,
    value: bool,
    update: impl Fn(&mut FloatingPanelSettings, bool) + 'static,
    id: &'static str,
) -> Element {
    settings_toggle_card_with_description(
        label,
        Some(description),
        value,
        change(ctx, update),
        id,
        ctx.hovered_card_id,
        ctx.set_hovered_card_id.clone(),
    )
    .with_key(id)
}

pub(super) fn render(ctx: &SettingsPageContext<'_>) -> (&'static str, Vec<Element>) {
    let panel = ctx.floating_panel;
    let mut rows = vec![
        toggle(
            ctx,
            "Enable floating panel",
            "Keep selected quota indicators visible in an independent window.",
            panel.enabled,
            |panel, value| panel.enabled = value,
            "panel-enabled",
        ),
        settings_control_card(
            "Visibility",
            Some("Hiding keeps the panel enabled. Restore it here or with its shortcut."),
            Button::new("Show panel")
                .enabled(panel.enabled)
                .on_click(crate::floating_panel::show),
            "panel-show",
            ctx.hovered_card_id,
            ctx.set_hovered_card_id.clone(),
        )
        .with_key("panel-show"),
        settings_section_heading("Appearance").with_key("panel-appearance"),
        settings_control_card(
            "Presentation",
            None,
            ComboBox::new(["Numbers", "Bars", "Rings"])
                .selected_index(match panel.presentation {
                    PanelPresentation::Numbers => 0,
                    PanelPresentation::Bars => 1,
                    PanelPresentation::Rings => 2,
                })
                .on_selection_changed(change(ctx, |panel, index| {
                    panel.presentation = match index {
                        0 => PanelPresentation::Numbers,
                        2 => PanelPresentation::Rings,
                        _ => PanelPresentation::Bars,
                    }
                })),
            "panel-presentation",
            ctx.hovered_card_id,
            ctx.set_hovered_card_id.clone(),
        )
        .with_key("panel-presentation"),
        settings_control_card(
            "Size",
            None,
            ComboBox::new(["Small", "Medium", "Large"])
                .selected_index(match panel.size {
                    PanelSize::Small => 0,
                    PanelSize::Medium => 1,
                    PanelSize::Large => 2,
                })
                .on_selection_changed(change(ctx, |panel, index| {
                    panel.size = match index {
                        0 => PanelSize::Small,
                        2 => PanelSize::Large,
                        _ => PanelSize::Medium,
                    }
                })),
            "panel-size",
            ctx.hovered_card_id,
            ctx.set_hovered_card_id.clone(),
        )
        .with_key("panel-size"),
        toggle(
            ctx,
            "Show used percentage",
            "Otherwise, each indicator shows the remaining quota.",
            panel.show_used,
            |panel, value| panel.show_used = value,
            "panel-used",
        ),
        toggle(
            ctx,
            "Show reset countdowns",
            "Display the time until each limit resets.",
            panel.show_reset,
            |panel, value| panel.show_reset = value,
            "panel-reset",
        ),
        settings_section_heading("Window").with_key("panel-window"),
        toggle(
            ctx,
            "Always on top",
            "Keep the panel above other windows.",
            panel.always_on_top,
            |panel, value| panel.always_on_top = value,
            "panel-topmost",
        ),
        toggle(
            ctx,
            "Lock position",
            "Prevent dragging. The panel remembers its position across launches.",
            panel.lock_position,
            |panel, value| panel.lock_position = value,
            "panel-locked",
        ),
        settings_control_card(
            "Show / hide shortcut",
            Some("Works even while another application is focused."),
            ComboBox::new(["None", "Ctrl+Alt+M", "Ctrl+Shift+M"])
                .selected_index(match panel.hotkey {
                    PanelHotkey::None => 0,
                    PanelHotkey::CtrlAltM => 1,
                    PanelHotkey::CtrlShiftM => 2,
                })
                .on_selection_changed(change(ctx, |panel, index| {
                    panel.hotkey = match index {
                        1 => PanelHotkey::CtrlAltM,
                        2 => PanelHotkey::CtrlShiftM,
                        _ => PanelHotkey::None,
                    }
                })),
            "panel-hotkey",
            ctx.hovered_card_id,
            ctx.set_hovered_card_id.clone(),
        )
        .with_key("panel-hotkey"),
        settings_control_card(
            "Reset position",
            Some("Move to the top-right corner of the current monitor."),
            {
                let reset = change(ctx, |panel, ()| panel.position = None);
                Button::new("Reset position").on_click(move || {
                    reset(());
                    crate::floating_panel::reset_position();
                })
            },
            "panel-position",
            ctx.hovered_card_id,
            ctx.set_hovered_card_id.clone(),
        )
        .with_key("panel-position"),
    ];
    if let Some(message) = crate::floating_panel::status() {
        rows.push(settings_info_card("Panel status", message).with_key("panel-status"));
    }
    rows.push(
        settings_section_heading(format!(
            "Indicators · {} / {MAX_METRICS}",
            panel.metrics.len()
        ))
        .with_key("panel-indicators"),
    );
    rows.push(text_block("Choose quota sources independently of tray icons. Disabled providers stay labeled as disabled; enable them on the Providers page.")
        .font_size(12.0).foreground(ThemeRef::SecondaryText).wrap().with_key("panel-sources-help").into());
    for (index, metric) in panel.metrics.iter().enumerate() {
        rows.push(indicator_card(ctx, index, metric));
    }
    // Pick an unused static source; all API-discovered quota lanes are available
    // in each indicator's selector after its provider is selected.
    let available = ProviderKind::ALL
        .iter()
        .flat_map(|provider| {
            crate::provider_registry::descriptor(*provider)
                .metrics
                .iter()
                .map(|metric| PanelMetric {
                    provider: *provider,
                    metric_id: metric.id.into(),
                })
        })
        .find(|metric| !panel.metrics.contains(metric));
    let add = change(ctx, |panel, metric: Option<PanelMetric>| {
        if let Some(metric) = metric
            && panel.metrics.len() < MAX_METRICS
            && !panel.metrics.contains(&metric)
        {
            panel.metrics.push(metric);
        }
    });
    rows.push(
        Button::new("Add indicator")
            .enabled(panel.metrics.len() < MAX_METRICS && available.is_some())
            .on_click(move || add(available.clone()))
            .with_key("panel-add")
            .into(),
    );
    ("Floating panel", rows)
}

fn indicator_card(ctx: &SettingsPageContext<'_>, index: usize, metric: &PanelMetric) -> Element {
    let available_metrics = |provider| {
        crate::provider_registry::tray_metric_options(provider, ctx.discovered_popup_bricks)
            .into_iter()
            .filter(|(id, _)| {
                !ctx.floating_panel.metrics.iter().any(|other| {
                    other != metric && other.provider == provider && other.metric_id == *id
                })
            })
            .collect::<Vec<_>>()
    };
    let providers = ProviderKind::ALL
        .into_iter()
        .filter_map(|provider| {
            let options = available_metrics(provider);
            (provider == metric.provider || !options.is_empty()).then_some((provider, options))
        })
        .collect::<Vec<_>>();
    let provider_index = providers
        .iter()
        .position(|(provider, _)| *provider == metric.provider)
        .unwrap_or(0);
    let provider_labels = providers
        .iter()
        .map(|(provider, _)| {
            crate::provider_registry::descriptor(*provider)
                .display_name
                .to_string()
        })
        .collect::<Vec<_>>();
    let mut metrics = available_metrics(metric.provider);
    if !metrics.iter().any(|(id, _)| id == &metric.metric_id) {
        metrics.push((
            metric.metric_id.clone(),
            format!("{} (unavailable)", metric.metric_id),
        ));
    }
    let metric_index = metrics
        .iter()
        .position(|(id, _)| id == &metric.metric_id)
        .unwrap_or(0);
    let current = metric.clone();
    let choose_provider = change(ctx, move |panel, selection: i32| {
        if let Ok(selection) = usize::try_from(selection)
            && let Some((provider, options)) = providers.get(selection)
            && let Some(slot) = panel.metrics.iter_mut().find(|slot| **slot == current)
            && slot.provider != *provider
            && let Some((id, _)) = options.first()
        {
            *slot = PanelMetric {
                provider: *provider,
                metric_id: id.clone(),
            };
        }
    });
    let current = metric.clone();
    let metric_ids = metrics.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
    let choose_metric = change(ctx, move |panel, selection: i32| {
        if let Ok(selection) = usize::try_from(selection)
            && let Some(id) = metric_ids.get(selection)
            && let Some(slot) = panel.metrics.iter_mut().find(|slot| **slot == current)
        {
            slot.metric_id = id.clone();
        }
    });
    let current = metric.clone();
    let remove = change(ctx, move |panel, ()| {
        panel.metrics.retain(|slot| *slot != current)
    });
    let current = metric.clone();
    let move_up = change(ctx, move |panel, ()| {
        if let Some(index) = panel.metrics.iter().position(|slot| *slot == current)
            && index > 0
        {
            panel.metrics.swap(index, index - 1);
        }
    });
    let current = metric.clone();
    let move_down = change(ctx, move |panel, ()| {
        if let Some(index) = panel.metrics.iter().position(|slot| *slot == current)
            && index + 1 < panel.metrics.len()
        {
            panel.metrics.swap(index, index + 1);
        }
    });
    border(
        vstack((
            grid((
                text_block(format!("Indicator {}", index + 1))
                    .semibold()
                    .grid_column(0)
                    .vertical_alignment(VerticalAlignment::Center),
                hstack((
                    Button::new("Up")
                        .enabled(index > 0)
                        .on_click(move || move_up(())),
                    Button::new("Down")
                        .enabled(index + 1 < ctx.floating_panel.metrics.len())
                        .on_click(move || move_down(())),
                    Button::new("Remove").on_click(move || remove(())),
                ))
                .spacing(4.0)
                .grid_column(1),
            ))
            .columns([GridLength::Star(1.0), GridLength::Auto]),
            grid((
                vstack((
                    text_block("Provider").font_size(12.0),
                    ComboBox::new(provider_labels)
                        .selected_index(provider_index as i32)
                        .on_selection_changed(choose_provider),
                ))
                .spacing(4.0)
                .grid_column(0),
                vstack((
                    text_block("Quota").font_size(12.0),
                    ComboBox::new(metrics.into_iter().map(|(_, label)| label))
                        .selected_index(metric_index as i32)
                        .on_selection_changed(choose_metric),
                ))
                .spacing(4.0)
                .grid_column(1),
            ))
            .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
            .column_spacing(12.0),
        ))
        .spacing(10.0),
    )
    .padding(settings_card_padding())
    .background(ThemeRef::CardBackground)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(8.0)
    .with_key(format!(
        "panel-indicator-{}-{}",
        metric.provider.id(),
        metric.metric_id
    ))
    .into()
}
