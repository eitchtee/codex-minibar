use super::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

struct NaturalSizeObserver {
    active: Cell<bool>,
    last_size: Cell<Option<(f64, f64)>>,
    callback: Rc<dyn Fn(f64, f64)>,
}

impl NaturalSizeObserver {
    fn new(callback: Rc<dyn Fn(f64, f64)>) -> Self {
        Self {
            active: Cell::new(true),
            last_size: Cell::new(None),
            callback,
        }
    }

    fn report(&self, width: f64, height: f64) {
        if !self.active.get() || !width.is_finite() || !height.is_finite() || height < 1.0 {
            return;
        }
        if self.last_size.get() == Some((width, height)) {
            return;
        }
        self.last_size.set(Some((width, height)));
        (self.callback)(width, height);
    }

    fn stop(&self) {
        self.active.set(false);
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    #[test]
    fn queued_measurement_from_unmounted_page_cannot_restore_startup_gap() {
        let height = Rc::new(Cell::new(0.0));
        let sink = height.clone();
        let callback: Rc<dyn Fn(f64, f64)> = Rc::new(move |_, h| sink.set(h));
        let startup = NaturalSizeObserver::new(callback.clone());
        startup.report(380.0, 850.0);
        startup.stop();
        let settled = NaturalSizeObserver::new(callback);
        settled.report(380.0, 770.0);
        startup.report(380.0, 850.0); // old low-priority dispatcher work
        assert_eq!(height.get(), 770.0);
    }

    #[test]
    fn completed_layout_changes_natural_size_without_republishing_identical_samples() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let sink = values.clone();
        let observer = NaturalSizeObserver::new(Rc::new(move |_, h| sink.borrow_mut().push(h)));
        observer.report(380.0, 850.0);
        observer.report(380.0, 770.0);
        observer.report(380.0, 770.0);
        assert_eq!(*values.borrow(), vec![850.0, 770.0]);
    }
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct StackPanel {
    pub key: Option<String>,
    pub modifiers: Modifiers,
    pub mounted: Option<Callback<Option<windows_core::IInspectable>>>,
    pub unmounted: Option<Callback<Option<windows_core::IInspectable>>>,
    pub orientation: Orientation,
    pub spacing: f64,
    pub children: Vec<Element>,
}

impl StackPanel {
    /// Report natural size after completed layout, even when the arranged
    /// viewport is unchanged. Identical samples and unmounted panels are ignored.
    pub fn on_resize(mut self, f: impl Fn(f64, f64) + 'static) -> Self {
        let f: Rc<dyn Fn(f64, f64)> = Rc::new(f);
        let previous_mounted = self.mounted.take();
        let previous_unmounted = self.unmounted.take();
        let revoker_slot: Rc<RefCell<Option<windows_core::EventRevoker>>> =
            Rc::new(RefCell::new(None));
        let observer_slot: Rc<RefCell<Option<Rc<NaturalSizeObserver>>>> =
            Rc::new(RefCell::new(None));
        let revoker_for_mount = revoker_slot.clone();
        let observer_for_mount = observer_slot.clone();
        self.mounted = Some(Callback::new(
            move |native: Option<windows_core::IInspectable>| {
                if let Some(ref callback) = previous_mounted {
                    callback.invoke(native.clone());
                }
                let Some(native) = native else {
                    return;
                };
                let Ok(element) = native.cast::<bindings::IFrameworkElement>() else {
                    return;
                };
                let observer = Rc::new(NaturalSizeObserver::new(f.clone()));
                if let Some(previous) = observer_for_mount.borrow_mut().replace(observer.clone()) {
                    previous.stop();
                }
                let measure_target = native.clone();
                let measure = Rc::new(move || {
                    if !observer.active.get() {
                        return;
                    }
                    if let Ok(size) = measure_target
                        .cast::<bindings::IUIElement>()
                        .and_then(|element| element.DesiredSize())
                    {
                        observer.report(size.width as f64, size.height as f64);
                    }
                });
                // LayoutUpdated runs after SizeChanged. Read the captured target:
                // WinUI deliberately supplies a null sender for this global event.
                let after_layout = measure.clone();
                if let Ok(revoker) = element.LayoutUpdated(move |_, _| after_layout()) {
                    *revoker_for_mount.borrow_mut() = Some(revoker);
                }
                if let Ok(queue) = bindings::DispatcherQueue::GetForCurrentThread() {
                    let first_measure = measure.clone();
                    let handler = bindings::DispatcherQueueHandler::new(move || first_measure());
                    let _ = queue
                        .TryEnqueueWithPriority(bindings::DispatcherQueuePriority::Low, &handler);
                }
            },
        ));
        self.unmounted = Some(Callback::new(
            move |native: Option<windows_core::IInspectable>| {
                // Dispatcher work cannot be revoked; invalidate its observer before
                // dropping the native event so an old page cannot restore its height.
                if let Some(observer) = observer_slot.borrow_mut().take() {
                    observer.stop();
                }
                *revoker_slot.borrow_mut() = None;
                if let Some(ref callback) = previous_unmounted {
                    callback.invoke(native);
                }
            },
        ));
        self
    }
}

impl StackPanel {
    pub fn vertical() -> Self {
        Self {
            orientation: Orientation::Vertical,
            ..Self::default()
        }
    }
    pub fn horizontal() -> Self {
        Self {
            orientation: Orientation::Horizontal,
            ..Self::default()
        }
    }
}

impl Widget for StackPanel {
    widget_header!(ControlKind::StackPanel);
    fn bindings(&self) -> PropBindings {
        generated::stack_panel_bindings(self)
    }
    fn children(&self) -> Children<'_> {
        Children::Keyed(&self.children)
    }
    fn on_mounted_callback(&self) -> Option<&Callback<Option<windows_core::IInspectable>>> {
        self.mounted.as_ref()
    }
    fn on_unmounted_callback(&self) -> Option<&Callback<Option<windows_core::IInspectable>>> {
        self.unmounted.as_ref()
    }
}

impl StackPanel {
    pub fn spacing(mut self, v: f64) -> Self {
        self.spacing = v;
        self
    }
}

pub fn vstack(children: impl IntoElements) -> StackPanel {
    let mut s = StackPanel::vertical();
    s.children = children.into_elements();
    s
}

pub fn hstack(children: impl IntoElements) -> StackPanel {
    let mut s = StackPanel::horizontal();
    s.children = children.into_elements();
    s
}
