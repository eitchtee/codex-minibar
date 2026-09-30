//! In-process rail reordering shared by nav rows and popup tabs.
//!
//! The pressed item follows the pointer along the list axis (clamped to the
//! first/last item), siblings slide out of its way via `UIElement.Translation`,
//! and on release the item settles into its slot before `on_drop` commits the
//! new order. No system drag visual or "Move" caption is involved.
use std::cell::Cell;
use std::time::Duration;

use super::*;
use crate::reorder_bindings as source;

const SHIFT_DURATION: Duration = Duration::from_millis(150);
const SETTLE_DURATION: Duration = Duration::from_millis(150);
/// Owners remount reordered lists; if nothing replaced the items by then the
/// commit was rejected and the translated items slide back.
const REJECT_GRACE: Duration = Duration::from_millis(700);
const DRAG_THRESHOLD: f64 = 6.0;

/// Pointer handlers for one reorderable item. Dropping unregisters the item
/// from its rail scope.
pub(super) struct NavigationHandlers {
    token: u64,
    _pointer: Vec<RoutedPointerHandler>,
}

impl Drop for NavigationHandlers {
    fn drop(&mut self) {
        REGISTRY.with(|registry| {
            if let Ok(mut registry) = registry.try_borrow_mut() {
                registry.retain(|item| item.token != self.token);
            }
        });
    }
}

struct RoutedPointerHandler {
    element: source::IUIElement,
    event: source::RoutedEvent,
    // AddHandler accepts a boxed delegate, not the delegate's IUnknown.
    // Keep this exact box for RemoveHandler as well.
    handler: windows_reference::IReference<BoxablePointerEventHandler>,
}

/// Generated non-generic delegates report `{iid}` as their WinRT signature,
/// but `IReference<Delegate>` hashes `delegate({iid})`. Boxing the generated
/// type yields an IID WinUI does not recognize and `AddHandler` rejects it.
#[repr(transparent)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct BoxablePointerEventHandler(bindings::PointerEventHandler);

unsafe impl windows_core::Interface for BoxablePointerEventHandler {
    type Vtable = <bindings::PointerEventHandler as windows_core::Interface>::Vtable;
    const IID: windows_core::GUID =
        <bindings::PointerEventHandler as windows_core::Interface>::IID;
}

impl windows_core::RuntimeType for BoxablePointerEventHandler {
    const SIGNATURE: windows_core::imp::ConstBuffer = windows_core::imp::ConstBuffer::new()
        .push_slice(b"delegate(")
        .push_other(windows_core::imp::ConstBuffer::for_interface::<
            bindings::PointerEventHandler,
        >())
        .push_slice(b")");
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
) -> windows_reference::IReference<BoxablePointerEventHandler> {
    windows_reference::IReference::from(BoxablePointerEventHandler(handler))
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
        if (x - start_x).hypot(y - start_y) < DRAG_THRESHOLD {
            return false;
        }
        self.pressed_at = None;
        true
    }
}

struct Registered {
    token: u64,
    scope: String,
    id: String,
    element: bindings::UIElement,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    start: f64,
    extent: f64,
}

struct Slot {
    id: String,
    element: bindings::UIElement,
    span: Span,
}

struct Session {
    scope: String,
    source: usize,
    target: usize,
    horizontal: bool,
    pointer_start: f64,
    gap: f64,
    animations: bool,
    slots: Vec<Slot>,
    on_drop: Callback<(String, String)>,
}

impl Session {
    fn spans(&self) -> Vec<Span> {
        self.slots.iter().map(|slot| slot.span).collect()
    }

    fn is_source(&self, config: &ReorderItem) -> bool {
        self.scope == config.scope && self.slots[self.source].id == config.id
    }
}

thread_local! {
    static REGISTRY: RefCell<Vec<Registered>> = const { RefCell::new(Vec::new()) };
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static NEXT_TOKEN: Cell<u64> = const { Cell::new(1) };
    /// Bumped on every drag start so stale settle/cleanup timers leave the
    /// new session's items alone.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static TIMERS: RefCell<Vec<(Rc<Cell<bool>>, crate::hooks::DispatcherTimer)>> =
        const { RefCell::new(Vec::new()) };
}

pub(super) fn attach_navigation(
    ui: &bindings::UIElement,
    config: &ReorderItem,
) -> Result<NavigationHandlers> {
    let element = ui.cast::<source::IUIElement>()?;
    let token = NEXT_TOKEN.with(|next| {
        let token = next.get();
        next.set(token + 1);
        token
    });
    REGISTRY.with(|registry| {
        registry.borrow_mut().push(Registered {
            token,
            scope: config.scope.clone(),
            id: config.id.clone(),
            element: ui.clone(),
        })
    });
    let gesture = Rc::new(RefCell::new(DragGesture::default()));

    let press_gesture = gesture.clone();
    let pressed = bindings::PointerEventHandler::new(move |sender, args| {
        if let (Some(sender), Some(args)) = (sender.as_ref(), args.as_ref())
            && let Ok(ui) = sender.cast::<bindings::UIElement>()
        {
            let info = pointer_event_info(&ui, args.into());
            press_gesture
                .borrow_mut()
                .press(info.x, info.y, info.is_left_button_pressed);
        }
    });

    let move_gesture = gesture.clone();
    let move_config = config.clone();
    let moved = bindings::PointerEventHandler::new(move |sender, args| {
        let (Some(sender), Some(args)) = (sender.as_ref(), args.as_ref()) else {
            return;
        };
        let Ok(ui) = sender.cast::<bindings::UIElement>() else {
            return;
        };
        let info = pointer_event_info(&ui, args.into());
        if is_active_source(&move_config) {
            let _ = args.SetHandled(true);
            if info.is_left_button_pressed {
                update(args);
            } else {
                finish();
            }
            return;
        }
        let started = move_gesture
            .borrow_mut()
            .moved(info.x, info.y, info.is_left_button_pressed);
        if started && !has_session() && begin(args, &ui, &move_config) {
            let _ = args.SetHandled(true);
            update(args);
        }
    });

    let release_gesture = gesture;
    let release_config = config.clone();
    let released = bindings::PointerEventHandler::new(move |_, args| {
        release_gesture.borrow_mut().pressed_at = None;
        if is_active_source(&release_config) {
            if let Some(args) = args.as_ref() {
                let _ = args.SetHandled(true);
            }
            finish();
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
    Ok(NavigationHandlers {
        token,
        _pointer: pointer,
    })
}

fn has_session() -> bool {
    SESSION.with(|session| session.borrow().is_some())
}

fn is_active_source(config: &ReorderItem) -> bool {
    SESSION.with(|session| {
        session
            .borrow()
            .as_ref()
            .is_some_and(|session| session.is_source(config))
    })
}

fn root_point(args: &bindings::PointerRoutedEventArgs) -> Option<(f64, f64)> {
    let position = args
        .GetCurrentPoint(None::<&bindings::UIElement>)
        .ok()?
        .Position()
        .ok()?;
    Some((position.x as f64, position.y as f64))
}

fn local_point(
    args: &bindings::PointerRoutedEventArgs,
    element: &bindings::UIElement,
) -> Option<(f64, f64)> {
    let position = args.GetCurrentPoint(element).ok()?.Position().ok()?;
    Some((position.x as f64, position.y as f64))
}

fn set_axis_translation(element: &bindings::UIElement, horizontal: bool, offset: f64) {
    let offset = offset as f32;
    let (x, y) = if horizontal {
        (offset, 0.0)
    } else {
        (0.0, offset)
    };
    diag::dropped(element.SetTranslation(windows_numerics::Vector3 { x, y, z: 0.0 }));
}

fn clear_translation_transition(element: &bindings::UIElement) {
    diag::dropped(element.SetTranslationTransition(None));
}

fn set_translation_transition(element: &bindings::UIElement, duration: Duration) {
    if let Err(error) = apply_xaml_translation_transition(element, duration) {
        diag::warn(format_args!("reorder TranslationTransition: {error:?}"));
    }
}

fn set_z_index(element: &bindings::UIElement, value: i32) {
    diag::dropped(bindings::Canvas::SetZIndex(element, value));
}

/// Measures every registered item in the pressed item's scope, lays out the
/// rail, and captures the pointer. Returns `false` when there is nothing to
/// reorder against.
fn begin(
    args: &bindings::PointerRoutedEventArgs,
    source_ui: &bindings::UIElement,
    config: &ReorderItem,
) -> bool {
    let Some(root) = root_point(args) else {
        return false;
    };
    GENERATION.with(|generation| generation.set(generation.get() + 1));

    // Later registrations replace earlier ones with the same id.
    let mut candidates: Vec<(String, bindings::UIElement)> = Vec::new();
    REGISTRY.with(|registry| {
        for item in registry.borrow().iter() {
            if item.scope != config.scope {
                continue;
            }
            let element = if item.id == config.id {
                source_ui.clone()
            } else {
                item.element.clone()
            };
            match candidates.iter_mut().find(|(id, _)| *id == item.id) {
                Some(existing) => existing.1 = element,
                None => candidates.push((item.id.clone(), element)),
            }
        }
    });

    let mut measured: Vec<(String, bindings::UIElement, f64, f64, f64, f64)> = Vec::new();
    for (id, element) in candidates {
        clear_translation_transition(&element);
        set_axis_translation(&element, true, 0.0);
        let Some((local_x, local_y)) = local_point(args, &element) else {
            continue;
        };
        let Ok(frame) = element.cast::<bindings::IFrameworkElement>() else {
            continue;
        };
        let width = frame.ActualWidth().unwrap_or(0.0);
        let height = frame.ActualHeight().unwrap_or(0.0);
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        measured.push((id, element, root.0 - local_x, root.1 - local_y, width, height));
    }
    if measured.len() < 2 || !measured.iter().any(|item| item.0 == config.id) {
        return false;
    }

    let spread = |pick: fn(&(String, bindings::UIElement, f64, f64, f64, f64)) -> f64| {
        let (min, max) = measured
            .iter()
            .map(pick)
            .fold((f64::MAX, f64::MIN), |(min, max), value| {
                (min.min(value), max.max(value))
            });
        max - min
    };
    let horizontal = spread(|item| item.2) > spread(|item| item.3);

    let mut slots: Vec<Slot> = measured
        .into_iter()
        .map(|(id, element, x, y, width, height)| Slot {
            id,
            element,
            span: if horizontal {
                Span {
                    start: x,
                    extent: width,
                }
            } else {
                Span {
                    start: y,
                    extent: height,
                }
            },
        })
        .collect();
    slots.sort_by(|a, b| a.span.start.total_cmp(&b.span.start));
    let Some(source_index) = slots.iter().position(|slot| slot.id == config.id) else {
        return false;
    };
    let spans: Vec<Span> = slots.iter().map(|slot| slot.span).collect();
    let gap = average_gap(&spans);

    for (index, slot) in slots.iter().enumerate() {
        if index == source_index || !config.animations_enabled {
            clear_translation_transition(&slot.element);
        } else {
            set_translation_transition(&slot.element, SHIFT_DURATION);
        }
    }
    set_z_index(&slots[source_index].element, 1);
    if let (Ok(element), Ok(routed)) = (
        source_ui.cast::<source::IUIElement>(),
        args.cast::<source::IPointerRoutedEventArgs>(),
    ) && let Ok(pointer) = routed.Pointer()
    {
        diag::dropped(element.CapturePointer(&pointer));
    }

    SESSION.with(|session| {
        *session.borrow_mut() = Some(Session {
            scope: config.scope.clone(),
            source: source_index,
            target: source_index,
            horizontal,
            pointer_start: if horizontal { root.0 } else { root.1 },
            gap,
            animations: config.animations_enabled,
            slots,
            on_drop: config.on_drop.clone(),
        })
    });
    true
}

fn update(args: &bindings::PointerRoutedEventArgs) {
    let Some(root) = root_point(args) else {
        return;
    };
    SESSION.with(|session| {
        let mut session = session.borrow_mut();
        let Some(session) = session.as_mut() else {
            return;
        };
        let pointer = if session.horizontal { root.0 } else { root.1 };
        let spans = session.spans();
        let delta = clamp_delta(&spans, session.source, pointer - session.pointer_start);
        set_axis_translation(
            &session.slots[session.source].element,
            session.horizontal,
            delta,
        );
        let target = target_index(&spans, session.source, delta);
        if target == session.target {
            return;
        }
        session.target = target;
        let offsets = layout_offsets(&spans, session.gap, session.source, target);
        for (index, slot) in session.slots.iter().enumerate() {
            if index != session.source {
                set_axis_translation(&slot.element, session.horizontal, offsets[index]);
            }
        }
    });
}

fn finish() {
    let Some(session) = SESSION.with(|session| session.borrow_mut().take()) else {
        return;
    };
    let source_element = session.slots[session.source].element.clone();
    if let Ok(element) = source_element.cast::<source::IUIElement>() {
        diag::dropped(element.ReleasePointerCaptures());
    }
    let generation = GENERATION.with(Cell::get);
    let spans = session.spans();
    let offsets = layout_offsets(&spans, session.gap, session.source, session.target);
    let settle = if session.animations {
        SETTLE_DURATION
    } else {
        Duration::ZERO
    };
    if session.animations {
        set_translation_transition(&source_element, SETTLE_DURATION);
    }
    set_axis_translation(&source_element, session.horizontal, offsets[session.source]);

    let elements: Vec<bindings::UIElement> = session
        .slots
        .iter()
        .map(|slot| slot.element.clone())
        .collect();
    if session.target == session.source {
        schedule(settle + Duration::from_millis(20), move || {
            if is_current(generation) {
                cleanup(&elements);
            }
        });
        return;
    }

    let from = session.slots[session.source].id.clone();
    let to = session.slots[session.target].id.clone();
    let on_drop = session.on_drop.clone();
    let horizontal = session.horizontal;
    let animations = session.animations;
    schedule(settle, move || {
        on_drop.invoke((from.clone(), to.clone()));
        let elements = elements.clone();
        schedule(REJECT_GRACE, move || {
            if !is_current(generation) {
                return;
            }
            for element in &elements {
                if animations {
                    set_translation_transition(element, SHIFT_DURATION);
                }
                set_axis_translation(element, horizontal, 0.0);
            }
            let elements = elements.clone();
            schedule(SHIFT_DURATION + Duration::from_millis(20), move || {
                if is_current(generation) {
                    cleanup(&elements);
                }
            });
        });
    });
}

fn is_current(generation: u64) -> bool {
    GENERATION.with(Cell::get) == generation && !has_session()
}

fn cleanup(elements: &[bindings::UIElement]) {
    for element in elements {
        clear_translation_transition(element);
        set_z_index(element, 0);
    }
}

/// Runs `task` once on the UI thread after `after`. Fired timers are pruned
/// lazily so a timer is never dropped from inside its own tick.
fn schedule(after: Duration, task: impl FnOnce() + 'static) {
    let fired = Rc::new(Cell::new(false));
    let task = Cell::new(Some(task));
    let done = fired.clone();
    let timer = crate::hooks::DispatcherTimer::new_one_shot(
        after.max(Duration::from_millis(1)),
        move || {
            if let Some(task) = task.take() {
                task();
            }
            done.set(true);
        },
    );
    match timer {
        Ok(timer) => TIMERS.with(|timers| {
            let mut timers = timers.borrow_mut();
            timers.retain(|(fired, _)| !fired.get());
            timers.push((fired, timer));
        }),
        Err(error) => diag::warn(format_args!("reorder timer failed: {error:?}")),
    }
}

fn average_gap(spans: &[Span]) -> f64 {
    if spans.len() < 2 {
        return 0.0;
    }
    let total: f64 = spans
        .windows(2)
        .map(|pair| (pair[1].start - (pair[0].start + pair[0].extent)).max(0.0))
        .sum();
    total / (spans.len() - 1) as f64
}

/// Keeps the dragged item on the rail between the first and last slot.
fn clamp_delta(spans: &[Span], source: usize, delta: f64) -> f64 {
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
        return 0.0;
    };
    let item = spans[source];
    let min = first.start - item.start;
    let max = (last.start + last.extent) - (item.start + item.extent);
    delta.clamp(min.min(0.0), max.max(0.0))
}

/// Index the dragged item would occupy: the number of other items whose
/// centers lie before the dragged item's center.
fn target_index(spans: &[Span], source: usize, delta: f64) -> usize {
    let item = spans[source];
    let center = item.start + delta + item.extent / 2.0;
    spans
        .iter()
        .enumerate()
        .filter(|(index, span)| *index != source && span.start + span.extent / 2.0 < center)
        .count()
}

/// Translation for every original slot once `source` moves to `target`.
fn layout_offsets(spans: &[Span], gap: f64, source: usize, target: usize) -> Vec<f64> {
    let mut offsets = vec![0.0; spans.len()];
    if source == target || spans.is_empty() {
        return offsets;
    }
    let mut order: Vec<usize> = (0..spans.len()).filter(|index| *index != source).collect();
    order.insert(target.min(order.len()), source);
    let mut cursor = spans[0].start;
    for index in order {
        offsets[index] = cursor - spans[index].start;
        cursor += spans[index].extent + gap;
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::{DragGesture, Span, clamp_delta, layout_offsets, target_index};

    fn rail() -> Vec<Span> {
        (0..4)
            .map(|index| Span {
                start: index as f64 * 44.0,
                extent: 40.0,
            })
            .collect()
    }

    #[test]
    fn routed_pointer_handler_is_boxed_and_retains_delegate_identity() {
        use windows_core::Interface;
        let handler = super::bindings::PointerEventHandler::new(|_, _| {});
        // Delegates themselves cannot be passed as WinRT Object values.
        assert!(handler.cast::<windows_core::IInspectable>().is_err());
        let boxed = super::box_pointer_handler(handler.clone());
        let inspectable = boxed.cast::<windows_core::IInspectable>().unwrap();
        let recovered = inspectable
            .cast::<windows_reference::IReference<super::BoxablePointerEventHandler>>()
            .unwrap();
        assert_eq!(recovered.Value().unwrap().0, handler);
        assert_eq!(recovered.as_raw(), boxed.as_raw());
    }

    #[test]
    fn boxed_pointer_handler_uses_winrt_delegate_signature() {
        use windows_core::RuntimeType;
        let signature = super::BoxablePointerEventHandler::SIGNATURE;
        assert_eq!(
            signature.as_slice(),
            b"delegate({a48a71e1-8bb4-5597-9e31-903a3f6a04fb})"
        );
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

    #[test]
    fn rail_clamps_to_first_and_last_slot() {
        let spans = rail();
        assert_eq!(clamp_delta(&spans, 1, -500.0), -44.0);
        assert_eq!(clamp_delta(&spans, 1, 500.0), 88.0);
        assert_eq!(clamp_delta(&spans, 0, -10.0), 0.0);
    }

    #[test]
    fn target_moves_after_crossing_neighbor_center() {
        let spans = rail();
        assert_eq!(target_index(&spans, 0, 20.0), 0);
        assert_eq!(target_index(&spans, 0, 30.0), 1);
        assert_eq!(target_index(&spans, 0, 132.0), 3);
        assert_eq!(target_index(&spans, 3, -132.0), 0);
    }

    #[test]
    fn siblings_shift_by_one_slot_and_source_settles_in_target() {
        let spans = rail();
        assert_eq!(layout_offsets(&spans, 4.0, 0, 2), vec![88.0, -44.0, -44.0, 0.0]);
        assert_eq!(layout_offsets(&spans, 4.0, 3, 1), vec![0.0, 44.0, 44.0, -88.0]);
        assert_eq!(layout_offsets(&spans, 4.0, 1, 1), vec![0.0; 4]);
    }
}
