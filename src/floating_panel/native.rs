use super::*;
use std::{cell::Cell, sync::Mutex};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
    },
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{
            MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, ReleaseCapture,
            UnregisterHotKey,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

const TITLE: &str = "Codex Minibar Floating Panel";
const SUBCLASS: usize = 0x434D4650;
const HOTKEY_ID: i32 = 0x4D50;
static STATUS: Mutex<Option<String>> = Mutex::new(None);
thread_local! {
    static REGISTERED_HOTKEY: Cell<PanelHotkey> = const { Cell::new(PanelHotkey::None) };
}

pub(super) fn title() -> &'static str {
    TITLE
}
pub(crate) fn status() -> Option<String> {
    STATUS.lock().ok().and_then(|status| status.clone())
}
pub(super) fn set_error(message: String) {
    if let Ok(mut status) = STATUS.lock() {
        *status = Some(message);
    }
}

pub(super) fn find() -> HWND {
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) }
}

pub(super) fn configure(hwnd: HWND) -> windows_core::Result<()> {
    if hwnd.is_null() {
        return Err(windows_core::Error::from_thread());
    }
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        SetWindowLongW(
            hwnd,
            GWL_STYLE,
            (style & !(WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU))
                as i32,
        );
        let extended = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongW(
            hwnd,
            GWL_EXSTYLE,
            ((extended | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW) as i32,
        );
        if SetWindowSubclass(hwnd, Some(subclass), SUBCLASS, 0) == 0 {
            return Err(windows_core::Error::from_thread());
        }
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    Ok(())
}

pub(super) fn apply(hwnd: HWND, config: &FloatingPanelSettings) {
    unsafe {
        SetWindowPos(
            hwnd,
            if config.always_on_top {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    let desired = if config.enabled {
        config.hotkey
    } else {
        PanelHotkey::None
    };
    REGISTERED_HOTKEY.with(|registered| {
        if registered.get() == desired {
            if desired == PanelHotkey::None && let Ok(mut status) = STATUS.lock() { *status = None; }
            return;
        }
        unsafe { UnregisterHotKey(hwnd, HOTKEY_ID); }
        registered.set(PanelHotkey::None);
        if let Ok(mut status) = STATUS.lock() { *status = None; }
        let modifiers = match desired {
            PanelHotkey::None => return,
            PanelHotkey::CtrlAltM => MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            PanelHotkey::CtrlShiftM => MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
        };
        if unsafe { RegisterHotKey(hwnd, HOTKEY_ID, modifiers, u32::from(b'M')) } == 0 {
            set_error("The panel shortcut is already in use. Choose another shortcut; Show panel still works.".into());
        } else { registered.set(desired); }
    });
}

pub(super) fn visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd) != 0 }
}
pub(super) fn hide(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
}
pub(super) fn show(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}
pub(super) fn drag(hwnd: HWND) {
    unsafe {
        ReleaseCapture();
        SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
    }
}

pub(super) fn position(hwnd: HWND) -> Option<PanelPosition> {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0).then_some(PanelPosition {
        x: rect.left,
        y: rect.top,
    })
}

fn work_area(hwnd: HWND, point: Option<PanelPosition>) -> RECT {
    unsafe {
        let monitor = match point {
            Some(point) => MonitorFromPoint(
                POINT {
                    x: point.x,
                    y: point.y,
                },
                MONITOR_DEFAULTTONEAREST,
            ),
            None => MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
        };
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        }
    }
}

pub(super) fn maximum_size(hwnd: HWND) -> (f64, f64) {
    let work = work_area(hwnd, None);
    let scale = f64::from(unsafe { GetDpiForWindow(hwnd) }.max(96)) / 96.0;
    (
        f64::from((work.right - work.left - 32).max(1)) / scale,
        f64::from((work.bottom - work.top - 32).max(1)) / scale,
    )
}

pub(super) fn place(hwnd: HWND, saved: Option<PanelPosition>) {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return;
    }
    let work = work_area(hwnd, saved);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let desired = saved.unwrap_or(PanelPosition {
        x: work.right - width - 16,
        y: work.top + 16,
    });
    let point = clamp_position(desired, width, height, work);
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            point.x,
            point.y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub(super) fn keep_visible(hwnd: HWND) {
    let Some(position) = position(hwnd) else {
        return;
    };
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return;
    }
    let clamped = clamp_position(
        position,
        rect.right - rect.left,
        rect.bottom - rect.top,
        work_area(hwnd, None),
    );
    if clamped != position {
        place(hwnd, Some(clamped));
    }
}

pub(super) fn client_size(hwnd: HWND) -> (f64, f64) {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetClientRect(hwnd, &mut rect);
    }
    let scale = f64::from(unsafe { GetDpiForWindow(hwnd) }.max(96)) / 96.0;
    (
        f64::from(rect.right - rect.left) / scale,
        f64::from(rect.bottom - rect.top) / scale,
    )
}

pub(super) fn clamp_position(
    point: PanelPosition,
    width: i32,
    height: i32,
    work: RECT,
) -> PanelPosition {
    PanelPosition {
        x: point
            .x
            .clamp(work.left, (work.right - width).max(work.left)),
        y: point
            .y
            .clamp(work.top, (work.bottom - height).max(work.top)),
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    // Never unwind across the native callback boundary.
    let handled = std::panic::catch_unwind(|| match message {
        WM_HOTKEY if wp == HOTKEY_ID as usize => {
            super::toggle();
            Some(0)
        }
        WM_CLOSE => {
            super::hide();
            Some(0)
        }
        WM_EXITSIZEMOVE => {
            super::runtime::save_position();
            None
        }
        WM_NCDESTROY => {
            unsafe {
                UnregisterHotKey(hwnd, HOTKEY_ID);
                RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS);
            }
            REGISTERED_HOTKEY.with(|key| key.set(PanelHotkey::None));
            None
        }
        _ => None,
    })
    .ok()
    .flatten();
    handled.unwrap_or_else(|| unsafe { DefSubclassProc(hwnd, message, wp, lp) })
}
