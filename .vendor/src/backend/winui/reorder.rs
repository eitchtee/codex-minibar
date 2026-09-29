//! Native drag lifetime and target feedback shared by nav rows and popup tabs.
use super::*;
use crate::reorder_bindings as source;

/// NavigationViewItem handles pointer events in its control template. Observe
/// those handled events and explicitly start WinUI's drag once the mouse moves.
pub(super) struct NavigationHandlers {
    _native: Vec<windows_core::EventRevoker>,
    _pointer: Vec<RoutedPointerHandler>,
}

struct RoutedPointerHandler {
    element: source::IUIElement,
    event: source::RoutedEvent,
    handler: bindings::PointerEventHandler,
}

impl RoutedPointerHandler {
    fn new(
        element: &source::IUIElement,
        event: source::RoutedEvent,
        handler: bindings::PointerEventHandler,
    ) -> Result<Self> {
        // AddHandler takes an untyped Object but expects the pointer delegate's
        // IUnknown ABI. Pass that ABI directly: delegates do not implement
        // IInspectable, so QueryInterface/casting the delegate would fail.
        unsafe {
            (windows_core::Interface::vtable(element).AddHandler)(
                element.as_raw(),
                event.as_raw(),
                handler.as_raw(),
                true,
            )
            .ok()?;
        }
        Ok(Self {
            element: element.clone(),
            event,
            handler,
        })
    }
}

impl Drop for RoutedPointerHandler {
    fn drop(&mut self) {
        unsafe {
            diag::dropped(
                (windows_core::Interface::vtable(&self.element).RemoveHandler)(
                    self.element.as_raw(),
                    self.event.as_raw(),
                    self.handler.as_raw(),
                )
                .ok(),
            );
        }
    }
}

#[derive(Default)]
struct DragGesture {
    pressed_at: Option<(f64, f64)>,
}

impl DragGesture {
    fn press(&mut self, x: f64, y: f64, left: bool) {
        self.pressed_at = left.then_some((x, y));
    }

    fn moved(&mut self, x: f64, y: f64, left: bool) -> bool {
        if !left {
            self.pressed_at = None;
            return false;
        }
        let Some((start_x, start_y)) = self.pressed_at else {
            return false;
        };
        if (x - start_x).hypot(y - start_y) < 6.0 {
            return false;
        }
        self.pressed_at = None;
        true
    }
}

pub(super) fn attach_navigation(
    ui: &bindings::UIElement,
    config: &ReorderItem,
) -> Result<NavigationHandlers> {
    let native = attach(ui, config)?;
    let element = ui.cast::<source::IUIElement>()?;
    let gesture = Rc::new(RefCell::new(DragGesture::default()));
    let on_press = gesture.clone();
    let pressed = bindings::PointerEventHandler::new(move |sender, args| {
        if let (Some(sender), Some(args)) = (sender.as_ref(), args.as_ref())
            && let Ok(ui) = sender.cast::<bindings::UIElement>()
        {
            let info = pointer_event_info(&ui, args.into());
            on_press
                .borrow_mut()
                .press(info.x, info.y, info.is_left_button_pressed);
        }
    });
    let moved = bindings::PointerEventHandler::new(move |sender, args| {
        let (Some(sender), Some(args)) = (sender.as_ref(), args.as_ref()) else {
            return;
        };
        let Ok(ui) = sender.cast::<bindings::UIElement>() else {
            return;
        };
        let info = pointer_event_info(&ui, args.into());
        if !gesture
            .borrow_mut()
            .moved(info.x, info.y, info.is_left_button_pressed)
        {
            return;
        }
        // Release the RefCell borrow before StartDragAsync raises DragStarting.
        let _ = args.SetHandled(true);
        let result = args.GetCurrentPoint(&ui).and_then(|point| {
            ui.cast::<source::IUIElement>()?
                .StartDragAsync(&point.cast::<source::PointerPoint>()?)
        });
        if let Err(error) = result {
            diag::warn(format_args!("start navigation drag failed: {error:?}"));
        }
    });
    let pointer = vec![
        RoutedPointerHandler::new(&element, source::UIElement::PointerPressedEvent()?, pressed)?,
        RoutedPointerHandler::new(&element, source::UIElement::PointerMovedEvent()?, moved)?,
    ];
    Ok(NavigationHandlers {
        _native: native,
        _pointer: pointer,
    })
}

#[cfg(test)]
mod tests {
    use super::DragGesture;

    #[test]
    fn navigation_drag_waits_for_movement_and_starts_only_once() {
        let mut drag = DragGesture::default();
        drag.press(10.0, 20.0, true);
        assert!(!drag.moved(13.0, 23.0, true));
        assert!(drag.moved(10.0, 27.0, true));
        assert!(!drag.moved(10.0, 35.0, true));
    }

    #[test]
    fn navigation_drag_ignores_right_click_and_released_buttons() {
        let mut drag = DragGesture::default();
        drag.press(0.0, 0.0, false);
        assert!(!drag.moved(30.0, 0.0, true));
        drag.press(0.0, 0.0, true);
        assert!(!drag.moved(30.0, 0.0, false));
        assert!(!drag.moved(60.0, 0.0, true));
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

pub(super) fn attach(
    ui: &bindings::UIElement,
    config: &ReorderItem,
) -> Result<Vec<windows_core::EventRevoker>> {
    let source_ui = ui.cast::<source::IUIElement>()?;
    source_ui.SetCanDrag(true)?;
    ui.SetAllowDrop(true)?;
    let transitions = if config.animations_enabled {
        Some(bindings::XamlReader::Load("<TransitionCollection xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation'><EntranceThemeTransition IsStaggeringEnabled='False'/><RepositionThemeTransition/></TransitionCollection>")?.cast::<source::TransitionCollection>()?)
    } else {
        None
    };
    source_ui.SetTransitions(transitions.as_ref())?;
    let scope = config.scope.clone();
    let id = config.id.clone();
    let mut revokers = vec![
        source_ui.DragStarting(move |_, args| {
            let Some(args) = args.as_ref() else {
                return;
            };
            // A real WinUI drag (with its movement threshold), not PointerPressed.
            if args
                .Data()
                .and_then(|data| data.SetText("minibar-provider-reorder"))
                .is_err()
            {
                return;
            }
            let _ = args.SetAllowedOperations(source::DataPackageOperation::Move);
            ACTIVE.with(|active| *active.borrow_mut() = Some((scope.clone(), id.clone())));
        })?,
        source_ui.DropCompleted(move |_, _| {
            ACTIVE.with(|active| *active.borrow_mut() = None);
        })?,
    ];
    let scope = config.scope.clone();
    let id = config.id.clone();
    let accept = move |args: &bindings::DragEventArgs| {
        let allowed = ACTIVE.with(|active| {
            active
                .borrow()
                .as_ref()
                .is_some_and(|(group, from)| group == &scope && from != &id)
        });
        let _ = args.SetAcceptedOperation(if allowed {
            bindings::DataPackageOperation::Move
        } else {
            bindings::DataPackageOperation::None
        });
        allowed
    };
    let accept = Rc::new(accept);
    let baseline = source_ui.Opacity().unwrap_or(1.0);
    let entered = accept.clone();
    revokers.push(ui.DragEnter(move |sender, args| {
        if let Some(args) = args.as_ref()
            && entered(args)
            && let Some(sender) = sender.as_ref()
            && let Ok(ui) = sender.cast::<bindings::IUIElement>()
        {
            let _ = ui.SetOpacity(baseline * 0.6);
        }
    })?);
    revokers.push(ui.DragOver(move |_, args| {
        if let Some(args) = args.as_ref() {
            accept(args);
        }
    })?);
    revokers.push(ui.DragLeave(move |sender, _| {
        if let Some(sender) = sender.as_ref()
            && let Ok(ui) = sender.cast::<bindings::IUIElement>()
        {
            let _ = ui.SetOpacity(baseline);
        }
    })?);
    let scope = config.scope.clone();
    let target = config.id.clone();
    let on_drop = config.on_drop.clone();
    revokers.push(ui.Drop(move |sender, args| {
        if let Some(sender) = sender.as_ref()
            && let Ok(ui) = sender.cast::<bindings::IUIElement>()
        {
            let _ = ui.SetOpacity(baseline);
        }
        let from = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active
                .as_ref()
                .is_some_and(|(group, from)| group == &scope && from != &target)
            {
                active.take().map(|(_, from)| from)
            } else {
                None
            }
        });
        if let Some(from) = from {
            if let Some(args) = args.as_ref() {
                let _ = args.SetAcceptedOperation(bindings::DataPackageOperation::Move);
            }
            on_drop.invoke((from, target.clone()));
        }
    })?);
    Ok(revokers)
}
