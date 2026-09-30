use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WidgetDragState {
    pub(super) active: PopupWidgetKind,
    pub(super) over: PopupWidgetKind,
    pub(super) column: Option<usize>,
    pub(super) gesture: u64,
}

pub(super) fn persist_popup_order(
    settings_tx: Sender<Settings>,
    set_ui: AsyncSetState<UiState>,
    mut ui: UiState,
    next_order: Vec<PopupWidgetKind>,
    right_column: Vec<PopupWidgetKind>,
) {
    ui.popup_order = next_order.clone();
    ui.popup_right_column = Some(right_column.clone());
    set_ui.call(ui);
    crate::settings_window::persist_update(settings_tx, move |settings| {
        settings.popup_order = next_order;
        settings.popup_right_column = Some(right_column);
        settings.normalize_popup_order();
    });
}

pub(super) fn persist_total_spend_period(
    settings_tx: Sender<Settings>,
    set_ui: AsyncSetState<UiState>,
    mut ui: UiState,
    period: TotalSpendPeriod,
) {
    if ui.total_spend_period == period {
        return;
    }
    ui.total_spend_period = period;
    set_ui.call(ui);
    crate::settings_window::persist_update(settings_tx, move |settings| {
        settings.total_spend_period = period;
    });
}

pub(super) fn commit_widget_drag(
    settings_tx: Sender<Settings>,
    set_ui: AsyncSetState<UiState>,
    ui: UiState,
    drag: WidgetDragState,
    set_drag: SetState<Option<WidgetDragState>>,
) {
    // A routed release may reach the block, column and page in the same frame.
    // Claim the gesture once, including no-op drops, before publishing state.
    thread_local! { static LAST_COMMIT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
    if LAST_COMMIT.with(|last| {
        if last.get() == drag.gesture {
            true
        } else {
            last.set(drag.gesture);
            false
        }
    }) {
        return;
    }
    set_drag.call(None);
    let show_total_spend = ui.usage_stats_enabled
        && ui.show_total_spend_on_all_tab
        && total_spend_provider_count(
            ui.codex_enabled,
            ui.claude_enabled,
            ui.cursor_enabled,
            ui.opencode_zen_enabled,
            ui.opencode_go_enabled,
            ui.openrouter_enabled,
            &ui.usage_stats_excluded_providers,
        ) > 1;
    let mut scratch = Settings {
        popup_order: ui.popup_order.clone(),
        popup_right_column: Some(home_right_column(&ui, show_total_spend)),
        providers: crate::settings::ProviderSettings::from_enabled(
            crate::provider_registry::PROVIDERS
                .iter()
                .filter(|descriptor| match descriptor.kind {
                    ProviderKind::Codex => ui.codex_enabled,
                    ProviderKind::Claude => ui.claude_enabled,
                    ProviderKind::Cursor => ui.cursor_enabled,
                    ProviderKind::OpenCodeZen => ui.opencode_zen_enabled,
                    ProviderKind::OpenCodeGo => ui.opencode_go_enabled,
                    ProviderKind::OpenRouter => ui.openrouter_enabled,
                    ProviderKind::Antigravity => ui.antigravity_enabled,
                    ProviderKind::Grok => ui.grok_enabled,
                    ProviderKind::Kiro => ui.kiro_enabled,
                })
                .map(|descriptor| descriptor.kind),
        ),
        show_total_spend_on_all_tab: ui.show_total_spend_on_all_tab,
        ..Settings::default()
    };
    let reordered = scratch.move_popup_widget(drag.active, drag.over, show_total_spend);
    let moved_column = drag
        .column
        .is_some_and(|column| scratch.assign_popup_widget_column(drag.active, column));
    if reordered || moved_column {
        remember_widget_positions();
        persist_popup_order(
            settings_tx,
            set_ui,
            ui,
            scratch.popup_order,
            scratch.popup_right_column.unwrap_or_default(),
        );
    }
}

pub(super) fn drag_handle(
    widget: PopupWidgetKind,
    color_scheme: ColorScheme,
    drag: &Option<WidgetDragState>,
    set_drag: SetState<Option<WidgetDragState>>,
) -> Element {
    let idle = popup_chrome_icon_color(color_scheme, false);
    let active = drag.as_ref().is_some_and(|state| state.active == widget);
    let set_on_press = set_drag.clone();
    relative_panel::<Vec<Element>>(vec![
        border(Element::Empty)
            .background(ThemeRef::SubtleFill)
            .opacity(if active { 1.0 } else { 0.0 })
            .corner_radius(4.0)
            .relative_align_left()
            .relative_align_right()
            .relative_align_top()
            .relative_align_bottom()
            .into(),
        crate::icons::element("fluent-drag", 14.0, idle)
            .relative_align_h_center()
            .relative_align_v_center(),
    ])
    .tooltip("Drag to reorder")
    .width(REORDER_BUTTON_SIZE)
    .height(REORDER_BUTTON_SIZE)
    .min_width(REORDER_BUTTON_SIZE)
    .min_height(REORDER_BUTTON_SIZE)
    .max_width(REORDER_BUTTON_SIZE)
    .max_height(REORDER_BUTTON_SIZE)
    .background(Color::transparent())
    .on_pointer_pressed(move |_: PointerEventInfo| {
        set_on_press.call(Some(WidgetDragState {
            active: widget,
            over: widget,
            column: None,
            gesture: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, Ordering::Relaxed)
            },
        }));
    })
    .with_key(format!("drag-handle-{}", widget.id()))
    .into()
}

pub(super) fn with_widget_drop_target(
    widget: PopupWidgetKind,
    content: Element,
    column: Option<usize>,
    drag: &Option<WidgetDragState>,
    set_drag: SetState<Option<WidgetDragState>>,
    settings_tx: Sender<Settings>,
    set_ui: AsyncSetState<UiState>,
    ui: UiState,
) -> Element {
    let is_active = drag.as_ref().is_some_and(|state| state.active == widget);
    let is_over = drag.as_ref().is_some_and(|state| state.over == widget);
    let show_outline = is_over && !is_active;
    let dragging = drag.is_some();
    let set_on_enter = set_drag.clone();
    let set_on_release = set_drag.clone();
    let drag_for_enter = drag.clone();
    let drag_for_release = drag.clone();

    // Visual ring only — null fill so it does not steal hits on its own.
    let outline: Element = border(Element::Empty)
        .border_thickness(Thickness::uniform(1.0))
        .border_brush(ThemeRef::Accent)
        .corner_radius(6.0)
        .opacity(if show_outline { 1.0 } else { 0.0 })
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .vertical_alignment(VerticalAlignment::Stretch)
        .grid_column(0)
        .grid_row(0)
        .into();

    let mut layers: Vec<Element> = vec![
        content
            .opacity(if is_active { 0.55 } else { 1.0 })
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .grid_column(0)
            .grid_row(0),
        outline,
    ];

    // While dragging, a transparent full-size catcher matches the highlight
    // zone (header + cards) so release on the title row commits the drop.
    // WinUI hit-tests Transparent backgrounds; null backgrounds do not.
    if dragging {
        layers.push(
            border(Element::Empty)
                .background(Color::transparent())
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .vertical_alignment(VerticalAlignment::Stretch)
                .grid_column(0)
                .grid_row(0)
                .on_pointer_entered(move |_: PointerEventInfo| {
                    let Some(current) = drag_for_enter.clone() else {
                        return;
                    };
                    if current.over == widget {
                        return;
                    }
                    set_on_enter.call(Some(WidgetDragState {
                        active: current.active,
                        over: widget,
                        column,
                        gesture: current.gesture,
                    }));
                })
                .on_pointer_released(move |_: PointerEventInfo| {
                    let Some(current) = drag_for_release.clone() else {
                        return;
                    };
                    // This catcher covers the whole section (header + body), so
                    // the drop target is always `widget` — do not trust a possibly
                    // stale `over` captured before the last pointer-enter update.
                    commit_widget_drag(
                        settings_tx.clone(),
                        set_ui.clone(),
                        ui.clone(),
                        WidgetDragState {
                            active: current.active,
                            over: widget,
                            column,
                            gesture: current.gesture,
                        },
                        set_on_release.clone(),
                    );
                })
                .with_key(format!("drop-catcher-{}", widget.id()))
                .into(),
        );
    }

    let layer: Element = grid(layers)
        .columns([GridLength::Star(1.0)])
        .rows([GridLength::Auto])
        .into();
    let mut host = vstack([layer]);
    host.mounted = Some(Callback::new(
        move |native: Option<windows_core::IInspectable>| {
            let Some(native) = native else {
                return;
            };
            WIDGET_MOUNTS.with(|mounts| {
                mounts.borrow_mut().insert(widget, native.clone());
            });
            let _ = after_layout(move || {
                let current =
                    WIDGET_MOUNTS.with(|mounts| mounts.borrow().get(&widget) == Some(&native));
                if !current {
                    return;
                }
                let old = WIDGET_POSITIONS.with(|positions| positions.borrow_mut().remove(&widget));
                if let Some((x, y)) = old
                    && popup::animations_enabled()
                    && let Ok((next_x, next_y)) = layout_position(native.clone())
                {
                    let _ = animate_layout_displacement(
                        native.clone(),
                        x - next_x,
                        y - next_y,
                        crate::theme::CONTROL_NORMAL_ANIMATION,
                    );
                }
            });
        },
    ));
    host.unmounted = Some(Callback::new(
        move |native: Option<windows_core::IInspectable>| {
            WIDGET_MOUNTS.with(|mounts| {
                let mut mounts = mounts.borrow_mut();
                if mounts.get(&widget) == native.as_ref() {
                    mounts.remove(&widget);
                }
            });
        },
    ));
    host.horizontal_alignment(HorizontalAlignment::Stretch)
        // Keep the host identity stable across highlight toggles so a remount
        // cannot swallow the in-flight pointer release.
        .with_key(format!("drop-target-{}", widget.id()))
        .into()
}

#[derive(Clone)]
pub(super) struct OpenRouterPopupActions {
    pub(super) settings_tx: Sender<Settings>,
    pub(super) hovered_action: Option<String>,
    pub(super) set_hovered_action: SetState<Option<String>>,
    pub(super) now: DateTime<Utc>,
}

pub(super) fn remove_openrouter_api_key(
    account_id: String,
    key_id: String,
    settings_tx: Sender<Settings>,
) {
    let change =
        crate::openrouter::AccountSecretChange::api_key(account_id.clone(), key_id.clone(), None);
    let rollback = match crate::openrouter::apply_account_secret_changes(&[change]) {
        Ok(rollback) => rollback,
        Err(error) => {
            notifications::show("OpenRouter key not removed", &format!("{error:#}"));
            return;
        }
    };
    if let Err(error) =
        crate::settings_window::try_persist_update_fallible(settings_tx, move |settings| {
            let mut accounts = crate::openrouter::accounts_for_settings(settings);
            let account = accounts
                .iter_mut()
                .find(|account| account.id == account_id)
                .ok_or_else(|| anyhow::anyhow!("OpenRouter account no longer exists"))?;
            let before = account.api_key_ids.len();
            account.api_key_ids.retain(|id| id != &key_id);
            anyhow::ensure!(
                account.api_key_ids.len() != before,
                "OpenRouter API key no longer exists"
            );
            settings.openrouter_accounts = accounts;
            settings.openrouter_credentials_revision =
                settings.openrouter_credentials_revision.wrapping_add(1);
            Ok(())
        })
    {
        let message = match rollback.restore() {
            Ok(()) => format!("{error:#}"),
            Err(rollback_error) => {
                format!("{error:#}; restoring the protected key also failed: {rollback_error:#}")
            }
        };
        notifications::show("OpenRouter key not removed", &message);
    }
}

pub(super) fn openrouter_delete_button(
    id: String,
    color_scheme: ColorScheme,
    hovered_action: &Option<String>,
    set_hovered_action: SetState<Option<String>>,
    on_click: impl IntoUnitCallback,
) -> Element {
    let hovered = hovered_action.as_deref() == Some(id.as_str());
    let set_on_enter = set_hovered_action.clone();
    let set_on_exit = set_hovered_action;
    let button_id = id.clone();
    let idle_color = popup_chrome_icon_color(color_scheme, false);
    let hover_background: Element = border(Element::Empty)
        .background(ThemeRef::SubtleFill)
        .opacity(if hovered { 1.0 } else { 0.0 })
        .corner_radius(4.0)
        .relative_align_left()
        .relative_align_right()
        .relative_align_top()
        .relative_align_bottom()
        .into();
    // Same hit target and glyph size as popup footer chrome. The old 24/14
    // slot crushed the Fluent delete path into unreadable slivers.
    let idle_icon: Element = crate::icons::element("fluent-delete", 18.0, idle_color)
        .opacity(if hovered { 0.0 } else { 1.0 })
        .relative_align_h_center()
        .relative_align_v_center();
    let accent_icon: Element = crate::icons::accent_element("fluent-delete", 18.0)
        .opacity(if hovered { 1.0 } else { 0.0 })
        .relative_align_h_center()
        .relative_align_v_center();
    relative_panel(vec![hover_background, idle_icon, accent_icon])
        .tooltip("Remove key")
        .width(POPUP_ACTION_SIZE)
        .height(POPUP_ACTION_SIZE)
        .min_width(POPUP_ACTION_SIZE)
        .min_height(POPUP_ACTION_SIZE)
        .max_width(POPUP_ACTION_SIZE)
        .max_height(POPUP_ACTION_SIZE)
        .background(Color::transparent())
        .on_pointer_entered(move |_: PointerEventInfo| {
            set_on_enter.call(Some(button_id.clone()));
        })
        .on_pointer_exited(move || set_on_exit.call(None))
        .on_tapped(on_click)
        .with_key(format!(
            "{id}-delete-{}-18-{:02X}{:02X}{:02X}",
            POPUP_ACTION_SIZE, idle_color.r, idle_color.g, idle_color.b
        ))
        .into()
}

/// A column remains a drop target when empty and below its final block.
pub(super) fn widget_column(
    column: usize,
    blocks: Vec<Element>,
    drag: &Option<WidgetDragState>,
    set_drag: SetState<Option<WidgetDragState>>,
    settings_tx: Sender<Settings>,
    set_ui: AsyncSetState<UiState>,
    ui: UiState,
) -> Element {
    let mut blocks = blocks;
    let show_drop_zone = drag.is_some() || blocks.is_empty();
    let mut target = border(
        caption(if drag.is_some() { "Drop here" } else { "" })
            .foreground(ThemeRef::TertiaryText)
            .horizontal_alignment(HorizontalAlignment::Center),
    )
    .height(if blocks.is_empty() { 80.0 } else { 32.0 })
    .background(Color::transparent())
    .corner_radius(6.0)
    .border_thickness(Thickness::uniform(1.0))
    .border_brush(ThemeRef::Accent)
    .opacity(if drag.is_some() { 1.0 } else { 0.0 });
    if let Some(current) = drag.clone() {
        let on_enter = current.clone();
        let set_enter = set_drag.clone();
        target = target
            .on_pointer_entered(move |_| {
                set_enter.call(Some(WidgetDragState {
                    over: on_enter.active,
                    column: Some(column),
                    ..on_enter.clone()
                }));
            })
            .on_pointer_released(move |_| {
                commit_widget_drag(
                    settings_tx.clone(),
                    set_ui.clone(),
                    ui.clone(),
                    WidgetDragState {
                        over: current.active,
                        column: Some(column),
                        ..current.clone()
                    },
                    set_drag.clone(),
                );
            });
    }
    if show_drop_zone {
        blocks.push(target.with_key(format!("column-drop-{column}")).into());
    }
    vstack(blocks)
        .spacing(6.0)
        .vertical_alignment(VerticalAlignment::Top)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .grid_column(column as i32)
        .with_key(format!("home-column-{column}"))
        .into()
}

thread_local! {
    static WIDGET_MOUNTS: std::cell::RefCell<HashMap<PopupWidgetKind, windows_core::IInspectable>> = std::cell::RefCell::new(HashMap::new());
    static WIDGET_POSITIONS: std::cell::RefCell<HashMap<PopupWidgetKind, (f32, f32)>> = std::cell::RefCell::new(HashMap::new());
}

pub(super) fn remember_widget_positions() {
    WIDGET_POSITIONS.with(|positions| {
        let mut positions = positions.borrow_mut();
        positions.clear();
        if !popup::animations_enabled() {
            return;
        }
        WIDGET_MOUNTS.with(|mounts| {
            for (widget, native) in mounts.borrow().iter() {
                if let Ok(position) = layout_position(native.clone()) {
                    let (origin_x, origin_y) = popup::window_origin_dip();
                    positions.insert(*widget, (position.0 + origin_x, position.1 + origin_y));
                }
            }
        });
    });
}

/// Auto-distribute only the currently visible blocks until the user first moves one.
/// Once assigned, hidden providers keep their saved column when they return.
pub(super) fn home_right_column(ui: &UiState, show_total_spend: bool) -> Vec<PopupWidgetKind> {
    ui.popup_right_column.clone().unwrap_or_else(|| {
        visible_popup_widgets(
            &ui.popup_order,
            show_total_spend,
            &ui.popup_visibility,
            ui.codex_enabled,
            ui.claude_enabled,
            ui.cursor_enabled,
            ui.opencode_zen_enabled,
            ui.opencode_go_enabled,
            ui.openrouter_enabled,
            ui.antigravity_enabled,
            ui.grok_enabled,
            ui.kiro_enabled,
        )
        .into_iter()
        .skip(1)
        .step_by(2)
        .collect()
    })
}
