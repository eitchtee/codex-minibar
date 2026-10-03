use super::{
    native,
    view::{self, PanelView},
};
use crate::{app::AppState, settings::Settings};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use windows_reactor::*;

thread_local! {
    static PANEL: RefCell<Option<PanelRuntime>> = const { RefCell::new(None) };
}

struct PanelRuntime {
    host: Rc<ReactorHost>,
    source: Rc<RefCell<PanelView>>,
    publisher: Rc<RefCell<Option<AsyncSetState<PanelView>>>>,
    state: Arc<AppState>,
    settings: Settings,
    hidden: bool,
    mounted: bool,
    timer: Option<DispatcherTimer>,
    resize_timer: Option<DispatcherTimer>,
    resize_finished: Rc<Cell<bool>>,
    target_size: WindowSize,
}

pub(crate) fn sync(settings: Settings, state: Arc<AppState>, dispatcher: UiMarshaller) {
    dispatcher.dispatch(move || {
        if let Err(error) = sync_on_ui(settings, state) {
            let message = format!("Could not open floating panel: {error}");
            native::set_error(message.clone());
            crate::logger::info(message);
            windows_reactor::request_ui_rerender_on_ui_thread();
        }
    });
}

fn snapshot(settings: &Settings, state: &AppState) -> PanelView {
    let rows = state
        .limits
        .lock()
        .map(|limits| {
            super::rows(
                &settings.floating_panel,
                &settings.providers,
                &limits,
                chrono::Utc::now(),
            )
        })
        .unwrap_or_default();
    PanelView {
        config: settings.floating_panel.clone(),
        rows,
    }
}

fn sync_on_ui(mut settings: Settings, state: Arc<AppState>) -> windows_core::Result<()> {
    settings.floating_panel.normalize();
    PANEL.with(|slot| -> windows_core::Result<()> {
        let mut slot = slot.borrow_mut();
        if slot.is_none() && settings.floating_panel.enabled {
            crate::theme::set_animations_enabled(settings.animations_enabled);
            crate::theme::apply_appearance(settings.theme, settings.accent_color);
            let source = Rc::new(RefCell::new(snapshot(&settings, &state)));
            let publisher = Rc::new(RefCell::new(None));
            let render_source = source.clone();
            let render_publisher = publisher.clone();
            let render_state = state.clone();
            let size = view::dimensions(&settings.floating_panel);
            let host = Rc::new(ReactorHost::new_with_window_options(
                native::title(),
                Some(size),
                InnerConstraints {
                    min_width: Some(80.0),
                    min_height: Some(40.0),
                    max_width: None,
                    max_height: None,
                },
                Box::new(move |_: &(), cx: &mut RenderCx| {
                    view::render(cx, &render_source, &render_publisher, render_state.clone())
                }),
                |_| {},
            )?);
            // Configure the exact independent HWND before the first content attach/show.
            native::configure(native::find())?;
            host.set_shown_in_switchers(false)?;
            let first = Rc::new(Cell::new(true));
            host.set_render_complete(move |_| {
                if first.replace(false) {
                    mounted();
                }
            });
            *slot = Some(PanelRuntime {
                host,
                source,
                publisher,
                state: state.clone(),
                settings: settings.clone(),
                hidden: false,
                mounted: false,
                timer: None,
                resize_timer: None,
                resize_finished: Rc::new(Cell::new(true)),
                target_size: size,
            });
        }
        let Some(panel) = slot.as_mut() else {
            return Ok(());
        };
        let was_enabled = panel.settings.floating_panel.enabled;
        let previous_position = panel.settings.floating_panel.position;
        panel.settings = settings;
        panel.state = state;
        let hwnd = native::find();
        native::apply(hwnd, &panel.settings.floating_panel);
        if !panel.settings.floating_panel.enabled {
            panel.timer.take();
            panel.resize_timer.take();
            panel.hidden = false;
            native::hide(hwnd);
            return Ok(());
        }
        if !was_enabled {
            panel.hidden = false;
        }
        if panel.timer.is_none() {
            panel.timer = Some(DispatcherTimer::new(Duration::from_secs(1), tick)?);
        }
        publish(panel);
        resize(panel, panel.mounted && !panel.hidden)?;
        if previous_position != panel.settings.floating_panel.position {
            native::place(hwnd, panel.settings.floating_panel.position);
        }
        // Do not reopen a temporarily hidden panel when an unrelated setting changes.
        if panel.mounted && !panel.hidden && !native::visible(hwnd) {
            show_host(&panel.host);
        }
        Ok(())
    })?;
    windows_reactor::request_ui_rerender_on_ui_thread();
    Ok(())
}

fn publish(panel: &PanelRuntime) {
    let view = snapshot(&panel.settings, &panel.state);
    if *panel.source.borrow() != view {
        *panel.source.borrow_mut() = view.clone();
        if let Some(setter) = panel.publisher.borrow().as_ref() {
            setter.call(view);
        }
    }
}

fn mounted() {
    let host = PANEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let panel = slot.as_mut()?;
        panel.mounted = true;
        let hwnd = native::find();
        let _ = resize(panel, false);
        native::place(hwnd, panel.settings.floating_panel.position);
        (panel.settings.floating_panel.enabled && !panel.hidden).then(|| panel.host.clone())
    });
    if let Some(host) = host {
        show_host(&host);
    }
}

fn show_host(host: &ReactorHost) {
    // Activate XAML composition before native non-activating re-show, as with
    // the tray popup. Preserve the user's currently focused application.
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    let foreground = unsafe { GetForegroundWindow() };
    if let Err(error) = host.activate_now() {
        crate::logger::info(format!("Could not show panel: {error}"));
    }
    native::show(native::find());
    if !foreground.is_null() && foreground != native::find() {
        unsafe {
            SetForegroundWindow(foreground);
        }
    }
}

pub(crate) fn refresh(dispatcher: UiMarshaller) {
    dispatcher.dispatch(tick);
}

fn tick() {
    PANEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(panel) = slot.as_mut() else {
            return;
        };
        if panel.resize_finished.get() {
            panel.resize_timer.take();
        }
        if !panel.settings.floating_panel.enabled || panel.hidden || !panel.mounted {
            return;
        }
        publish(panel);
        let _ = resize(panel, true);
        native::keep_visible(native::find());
    });
}

fn resize(panel: &mut PanelRuntime, animate: bool) -> windows_core::Result<()> {
    let hwnd = native::find();
    let mut target = view::dimensions(&panel.settings.floating_panel);
    let (max_width, max_height) = native::maximum_size(hwnd);
    target.width = target.width.min(max_width);
    target.height = target.height.min(max_height);
    let (width, height) = native::client_size(hwnd);
    if panel.target_size == target
        && ((width - target.width).abs() < 1.0 && (height - target.height).abs() < 1.0
            || animate && panel.resize_timer.is_some())
    {
        return Ok(());
    }
    panel.target_size = target;
    panel.resize_timer.take();
    panel.resize_finished.set(true);
    let duration = crate::theme::duration(Duration::from_millis(180));
    if !animate || duration.is_zero() {
        panel.host.resize_client(target.width, target.height)?;
        native::keep_visible(hwnd);
    } else {
        let host = Rc::downgrade(&panel.host);
        let start = Instant::now();
        let finished = panel.resize_finished.clone();
        finished.set(false);
        panel.resize_timer = Some(DispatcherTimer::new(
            Duration::from_millis(16),
            move || {
                if finished.get() {
                    return;
                }
                let Some(host) = host.upgrade() else {
                    return;
                };
                let progress = if crate::theme::animations_enabled() {
                    (start.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0)
                } else {
                    1.0
                };
                let eased = 1.0 - (1.0 - progress).powi(3);
                let _ = host.resize_client(
                    width + (target.width - width) * eased,
                    height + (target.height - height) * eased,
                );
                if progress >= 1.0 {
                    finished.set(true);
                    native::keep_visible(native::find());
                }
            },
        )?);
    }
    Ok(())
}

pub(crate) fn show() {
    let host = PANEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let panel = slot.as_mut()?;
        if !panel.settings.floating_panel.enabled {
            return None;
        }
        panel.hidden = false;
        publish(panel);
        let _ = resize(panel, false);
        native::keep_visible(native::find());
        panel.mounted.then(|| panel.host.clone())
    });
    if let Some(host) = host {
        show_host(&host);
    }
}

pub(crate) fn hide() {
    PANEL.with(|slot| {
        if let Some(panel) = slot.borrow_mut().as_mut() {
            panel.hidden = true;
            panel.resize_timer.take();
            native::hide(native::find());
        }
    });
}

pub(crate) fn toggle() {
    if native::visible(native::find()) {
        hide();
    } else {
        show();
    }
}

pub(crate) fn begin_drag() {
    let can_drag = PANEL.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|panel| !panel.settings.floating_panel.lock_position)
    });
    if can_drag {
        native::drag(native::find());
    }
}

pub(crate) fn reset_position() {
    native::place(native::find(), None);
}

pub(super) fn save_position() {
    let update = PANEL.with(|slot| {
        slot.borrow().as_ref().and_then(|panel| {
            native::position(native::find())
                .map(|position| (panel.state.settings_tx.clone(), position))
        })
    });
    if let Some((tx, position)) = update {
        crate::settings_window::persist_update(tx, |settings| {
            settings.floating_panel.position = Some(position)
        });
    }
}
