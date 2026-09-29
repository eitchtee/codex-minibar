//! Native drag lifetime and target feedback shared by nav rows and popup tabs.
use super::*;
use crate::reorder_bindings as source;

/// NavigationViewItem handles pointer events in its control template. Observe
/// those handled events and commit a local reorder after the mouse moves.
pub(super) struct NavigationHandlers {
    _pointer: Vec<RoutedPointerHandler>,
}

struct RoutedPointerHandler {
    element: source::IUIElement,
    event: source::RoutedEvent,
    // AddHandler accepts a boxed delegate, not the delegate's IUnknown.
    // Keep this exact box for RemoveHandler as well.
    handler: windows_reference::IReference<bindings::PointerEventHandler>,
}

impl RoutedPointerHandler {
    fn new(
        element: &source::IUIElement,
        event: source::RoutedEvent,
        handler: bindings::PointerEventHandler,
    ) -> Result<Self> {
        // Matches C++/WinRT's box_value(PointerEventHandler(...)). The box
        // implements IInspectable and lets WinUI unwrap the typed delegate.
        let handler = box_pointer_handler(handler);
        element.AddHandler(&event, &handler, true)?;
        Ok(Self {
            element: element.clone(),
            event,
            handler,
        })
    }
}

impl Drop for RoutedPointerHandler {
    fn drop(&mut self) {
        diag::dropped(self.element.RemoveHandler(&self.event, &self.handler));
    }
}

fn box_pointer_handler(
    handler: bindings::PointerEventHandler,
) -> windows_reference::IReference<bindings::PointerEventHandler> {
    windows_reference::IReference::from(handler)
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
    let element = ui.cast::<source::IUIElement>()?;
    let scope = config.scope.clone();
    let id = config.id.clone();
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
    let moved_gesture = gesture.clone();
    let moved_scope = scope.clone();
    let moved_id = id.clone();
    let moved = bindings::PointerEventHandler::new(move |sender, args| {
        let (Some(sender), Some(args)) = (sender.as_ref(), args.as_ref()) else {
            return;
        };
        let Ok(ui) = sender.cast::<bindings::UIElement>() else {
            return;
        };
        let info = pointer_event_info(&ui, args.into());
        let started_here =
            moved_gesture
                .borrow_mut()
                .moved(info.x, info.y, info.is_left_button_pressed);
        let dragging_same_scope = ACTIVE.with(|active| {
            active
                .borrow()
                .as_ref()
                .is_some_and(|(group, _)| group == &moved_scope)
        });
        if !started_here && !dragging_same_scope {
            return;
        }
        if started_here {
            ACTIVE.with(|active| {
                *active.borrow_mut() = Some((moved_scope.clone(), moved_id.clone()))
            });
        }
    });
    let on_drop = config.on_drop.clone();
    let release_gesture = gesture.clone();
    let released = bindings::PointerEventHandler::new(move |_, args| {
        release_gesture.borrow_mut().pressed_at = None;
        let from = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            active
                .take()
                .and_then(|(group, from)| (group == scope && from != id).then_some(from))
        });
        if let Some(from) = from {
            if let Some(args) = args.as_ref() {
                let _ = args.SetHandled(true);
            }
            on_drop.invoke((from, id.clone()));
        }
    });
    let pointer = vec![
        RoutedPointerHandler::new(&element, source::UIElement::PointerPressedEvent()?, pressed)?,
        RoutedPointerHandler::new(&element, source::UIElement::PointerMovedEvent()?, moved)?,
        RoutedPointerHandler::new(
            &element,
            source::UIElement::PointerReleasedEvent()?,
            released,
        )?,
    ];
    Ok(NavigationHandlers { _pointer: pointer })
}

#[cfg(test)]
mod tests {
    use super::DragGesture;
    #[test]
    fn routed_pointer_handler_is_boxed_and_retains_delegate_identity() {
        use windows_core::Interface;
        let handler = super::bindings::PointerEventHandler::new(|_, _| {});
        // Delegates themselves cannot be passed as WinRT Object values.
        assert!(handler.cast::<windows_core::IInspectable>().is_err());
        let boxed = super::box_pointer_handler(handler.clone());
        let inspectable = boxed.cast::<windows_core::IInspectable>().unwrap();
        let recovered = inspectable
            .cast::<windows_reference::IReference<super::bindings::PointerEventHandler>>()
            .unwrap();
        assert_eq!(recovered.Value().unwrap(), handler);
        assert_eq!(recovered.as_raw(), boxed.as_raw());
    }

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

#[allow(dead_code)]
pub(super) fn attach(
    ui: &bindings::UIElement,
    config: &ReorderItem,
) -> Result<Vec<windows_core::EventRevoker>> {
    // StartDragAsync is initiated from the routed pointer handler below. The
    // automatic CanDrag path is unreliable for templated NavigationViewItems.
    ui.SetAllowDrop(true)
        .map_err(|error| windows_core::Error::new(error.code(), format!("AllowDrop: {error}")))?;
    let mut revokers = Vec::new();
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
    let baseline = 1.0;
    let entered = accept.clone();
    revokers.push(
        ui.DragEnter(move |sender, args| {
            if let Some(args) = args.as_ref()
                && entered(args)
                && let Some(sender) = sender.as_ref()
                && let Ok(ui) = sender.cast::<bindings::IUIElement>()
            {
                let _ = ui.SetOpacity(baseline * 0.6);
            }
        })
        .map_err(|error| windows_core::Error::new(error.code(), format!("DragEnter: {error}")))?,
    );
    revokers.push(
        ui.DragOver(move |_, args| {
            if let Some(args) = args.as_ref() {
                accept(args);
            }
        })
        .map_err(|error| windows_core::Error::new(error.code(), format!("DragOver: {error}")))?,
    );
    revokers.push(
        ui.DragLeave(move |sender, _| {
            if let Some(sender) = sender.as_ref()
                && let Ok(ui) = sender.cast::<bindings::IUIElement>()
            {
                let _ = ui.SetOpacity(baseline);
            }
        })
        .map_err(|error| windows_core::Error::new(error.code(), format!("DragLeave: {error}")))?,
    );
    let scope = config.scope.clone();
    let target = config.id.clone();
    let on_drop = config.on_drop.clone();
    revokers.push(
        ui.Drop(move |sender, args| {
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
        })
        .map_err(|error| windows_core::Error::new(error.code(), format!("Drop: {error}")))?,
    );
    Ok(revokers)
}
