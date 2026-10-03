use super::*;
use std::{cell::RefCell, rc::Rc};
use windows_reactor::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PanelView {
    pub config: FloatingPanelSettings,
    pub rows: Vec<PanelRow>,
}

pub(super) fn dimensions(config: &FloatingPanelSettings) -> WindowSize {
    let scale = config.size.scale();
    let row_height = match config.presentation {
        PanelPresentation::Numbers => 54.0,
        PanelPresentation::Bars => 72.0,
        PanelPresentation::Rings => 64.0,
    };
    WindowSize {
        width: 300.0 * scale,
        height: (36.0 * scale).max(36.0)
            + (16.0 + row_height * config.metrics.len().max(1) as f64) * scale,
    }
}

pub(super) fn render(
    cx: &mut RenderCx,
    source: &Rc<RefCell<PanelView>>,
    publisher: &Rc<RefCell<Option<AsyncSetState<PanelView>>>>,
    state: std::sync::Arc<crate::app::AppState>,
) -> Element {
    let (view, set_view) = cx.use_async_state(source.borrow().clone());
    *publisher.borrow_mut() = Some(set_view);
    let (displayed, set_displayed) = cx.use_state(view.clone());
    let (content_opacity, set_content_opacity) = cx.use_state(1.0);
    let transition = cx.use_ref(None::<DispatcherTimer>);
    let previous_layout = layout_signature(&displayed);
    let next_view = view.clone();
    cx.use_effect(next_view.clone(), move || {
        let view = next_view;
        transition.set(None);
        if previous_layout != layout_signature(&view) && crate::theme::animations_enabled() {
            set_content_opacity.call(0.0);
            let target = view.clone();
            let apply = set_displayed.clone();
            let opacity = set_content_opacity.clone();
            let timer =
                DispatcherTimer::new_one_shot(crate::theme::CONTROL_FASTER_ANIMATION, move || {
                    apply.call(target.clone());
                    opacity.call(1.0);
                });
            match timer {
                Ok(timer) => transition.set(Some(timer)),
                Err(_) => {
                    set_displayed.call(view);
                    set_content_opacity.call(1.0);
                }
            }
        } else {
            set_displayed.call(view);
            set_content_opacity.call(1.0);
        }
    });
    let scheme = cx.use_color_scheme();
    let inner = cx.use_inner_size();
    let scale = view.config.size.scale();
    let label = if view.config.lock_position {
        "Minibar · locked"
    } else {
        "Minibar"
    };
    let settings_state = state.clone();
    let header = grid((
        border(text_block(label).font_size(12.0 * scale).semibold())
            .background(Color::transparent())
            .padding(Thickness::uniform(8.0 * scale))
            .grid_column(0)
            .on_pointer_pressed(|event: PointerEventInfo| {
                if event.is_left_button_pressed {
                    super::begin_drag();
                }
            })
            .automation_name("Drag floating panel"),
        Button::new("Settings")
            .font_size(11.0 * scale)
            .grid_column(1)
            .on_click(move || {
                if let Err(error) = crate::settings_window::open_floating_panel(
                    settings_state.settings_tx.clone(),
                    settings_state.usage_actions_tx.clone(),
                    settings_state.updates.clone(),
                ) {
                    crate::logger::info(format!("Could not open panel settings: {error}"));
                }
            }),
        Button::new("Hide")
            .font_size(11.0 * scale)
            .grid_column(2)
            .on_click(super::hide)
            .automation_name("Hide floating panel; restore it from Settings or the hotkey"),
    ))
    .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
    .column_spacing(4.0 * scale)
    .height((36.0 * scale).max(36.0))
    .grid_row(0);

    let mut content = Vec::new();
    for row in &displayed.rows {
        content.push(metric_row(row, &displayed.config, scheme));
    }
    if content.is_empty() {
        content.push(
            text_block("Choose indicators in Settings → Floating panel.")
                .font_size(12.0 * scale)
                .wrap()
                .foreground(ThemeRef::SecondaryText)
                .padding(Thickness::uniform(12.0 * scale))
                .into(),
        );
    }
    let rows = vstack(content).spacing(0.0).with_key(format!(
        "panel-rows-{}-{:?}",
        layout_signature(&displayed),
        scheme
    ));
    let body = scroll_viewer(
        // The keyed strip must be in a multi-child container: keys on a
        // Border's sole child do not force remounts in windows-reactor.
        grid(vec![rows.into()])
            .columns([GridLength::Star(1.0)])
            .opacity(content_opacity)
            .with_opacity_transition(crate::theme::duration(
                crate::theme::CONTROL_FASTER_ANIMATION,
            )),
    )
    .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
    .grid_row(1);
    let surface = border(
        grid((header, body))
            .columns([GridLength::Star(1.0)])
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .padding(Thickness::uniform(6.0 * scale)),
    )
    .background(ThemeRef::LayerFill)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(8.0)
    .into();
    // Win10 does not supply DWM-rounded corners. Fill the complete client
    // rectangle so the rounded XAML border never exposes black window pixels.
    grid(vec![
        border(Element::Empty)
            .background(ThemeRef::LayerFill)
            .into(),
        surface,
    ])
    .width(inner.width)
    .height(inner.height)
    .into()
}

fn layout_signature(view: &PanelView) -> String {
    format!(
        "{:?}-{:?}-{}",
        view.config.presentation,
        view.config.size,
        view.rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>()
            .join("|")
    )
}

fn metric_row(row: &PanelRow, config: &FloatingPanelSettings, scheme: ColorScheme) -> Element {
    let scale = config.size.scale();
    let name = crate::provider_registry::descriptor(row.provider).display_name;
    let value = row
        .value
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| "—".into());
    let color = indicator_color(row.value, config.show_used, scheme);
    let label = vstack((
        text_block(format!("{name} · {}", row.label))
            .font_size(12.0 * scale)
            .semibold()
            .text_trimming(TextTrimming::CharacterEllipsis),
        text_block(&row.detail)
            .font_size(11.0 * scale)
            .foreground(ThemeRef::SecondaryText)
            .text_trimming(TextTrimming::CharacterEllipsis),
    ))
    .spacing(3.0 * scale)
    .vertical_alignment(VerticalAlignment::Center);
    let number = text_block(&value)
        .font_size(22.0 * scale)
        .semibold()
        .width(62.0 * scale)
        .horizontal_alignment(HorizontalAlignment::Right)
        .foreground(color)
        .vertical_alignment(VerticalAlignment::Center);
    let content: Element = match config.presentation {
        PanelPresentation::Numbers => grid((label.grid_column(0), number.grid_column(1)))
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(8.0)
            .into(),
        PanelPresentation::Bars => {
            let bar_width = 264.0 * scale;
            let bar = grid(vec![
                border(Element::Empty)
                    .background(ThemeRef::SubtleFill)
                    .corner_radius(3.0)
                    .height(5.0 * scale)
                    .into(),
                border(Element::Empty)
                    .background(color)
                    .corner_radius(3.0)
                    .width(bar_width * f64::from(row.value.unwrap_or(0)) / 100.0)
                    .height(5.0 * scale)
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .into(),
            ])
            .width(bar_width);
            vstack((
                grid((label.grid_column(0), number.grid_column(1)))
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .column_spacing(8.0),
                bar,
            ))
            .spacing(7.0 * scale)
            .into()
        }
        PanelPresentation::Rings => grid((
            ring(row, color, scale).grid_column(0),
            label.grid_column(1),
            number.font_size(18.0 * scale).grid_column(2),
        ))
        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
        .column_spacing(8.0 * scale)
        .into(),
    };
    border(content)
        .padding(Thickness::uniform(10.0 * scale))
        .height(
            match config.presentation {
                PanelPresentation::Numbers => 54.0,
                PanelPresentation::Bars => 72.0,
                PanelPresentation::Rings => 64.0,
            } * scale,
        )
        .automation_name(format!(
            "{name}, {}, {value} {}, {}",
            row.label,
            if config.show_used {
                "used"
            } else {
                "remaining"
            },
            row.detail
        ))
        .with_key(row.id.clone())
        .into()
}

fn indicator_color(value: Option<u8>, used: bool, scheme: ColorScheme) -> Color {
    let remaining = value.map(|value| if used { 100 - value } else { value });
    match remaining {
        Some(0..=10) => {
            if scheme == ColorScheme::Dark {
                Color::rgb(255, 153, 164)
            } else {
                Color::rgb(196, 43, 28)
            }
        }
        Some(11..=25) => {
            if scheme == ColorScheme::Dark {
                Color::rgb(252, 225, 0)
            } else {
                Color::rgb(157, 93, 0)
            }
        }
        _ => {
            let [r, g, b] = crate::theme::current_accent_rgb();
            Color::rgb(r, g, b)
        }
    }
}

fn ring(row: &PanelRow, color: Color, scale: f64) -> Element {
    let value = row.value.unwrap_or(0);
    let stroke = format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b);
    let arc = if value == 100 {
        format!("<Ellipse Width=\"32\" Height=\"32\" Stroke=\"{stroke}\" StrokeThickness=\"4\"/>")
    } else if value == 0 {
        String::new()
    } else {
        let angle = f64::from(value) / 100.0 * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
        let (x, y) = (16.0 + 14.0 * angle.cos(), 16.0 + 14.0 * angle.sin());
        format!(
            "<Path Stroke=\"{stroke}\" StrokeThickness=\"4\" Data=\"M 16,2 A 14,14 0 {} 1 {x:.3},{y:.3}\"/>",
            u8::from(value > 50)
        )
    };
    let xaml = format!(
        r##"<Viewbox xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"><Grid Width="32" Height="32"><Ellipse Width="32" Height="32" Stroke="#40787878" StrokeThickness="4"/>{arc}</Grid></Viewbox>"##
    );
    let mut host = swap_chain_panel()
        .width(34.0 * scale)
        .height(34.0 * scale)
        .with_key(format!("ring-{}-{value}-{stroke}-{scale}", row.id));
    host.mounted = Some(Callback::new(
        move |native: Option<windows_core::IInspectable>| {
            if let Some(native) = native
                && let Err(error) = crate::acrylic::install_spend_donut_into(native, &xaml)
            {
                crate::logger::info(format!("Could not paint panel ring: {error}"));
            }
        },
    ));
    host.unmounted = Some(Callback::new(
        |native: Option<windows_core::IInspectable>| {
            if let Some(native) = native {
                let _ = crate::acrylic::clear_children(native);
            }
        },
    ));
    host.into()
}
