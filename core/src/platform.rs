//! Windows-specific input, clipboard and folder access (calls Win32 directly).

use std::path::PathBuf;
use std::sync::OnceLock;

use windows::core::PWSTR;
use windows::Win32::Foundation::{
    HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, VK_CONTROL, VK_MENU,
};
use windows::Win32::UI::Shell::{
    DragQueryFileW, FOLDERID_Desktop, SHGetKnownFolderPath, DROPFILES, HDROP, KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, SetCursorPos,
    SetWindowsHookExW, TranslateMessage, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SM_CXSCREEN, SM_CYSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::protocol::{Button, Message};
use crate::{AppError, Result};

fn os_err(what: &str, e: windows::core::Error) -> AppError {
    AppError::Io(format!("{what}: {e}"))
}

/// Call once at start-up so coordinates are real pixels at any display scaling.
pub fn init_dpi() {
    // Ignore failure: the app manifest normally already sets this.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Size of the primary screen in pixels.
pub fn screen_size() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

pub fn set_cursor_pos(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// True while Ctrl and Alt are both held (used for the switch hotkeys).
pub fn ctrl_alt_down() -> bool {
    unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 && GetAsyncKeyState(VK_MENU.0 as i32) < 0 }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse(
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    dx: i32,
    dy: i32,
    data: i32,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Apply a message from the master (mouse, wheel, keyboard) on this PC.
pub fn inject(msg: &Message) {
    match msg {
        Message::Enter { x, y } | Message::MouseAbs { x, y } => set_cursor_pos(*x, *y),
        Message::MouseMove { dx, dy } => send(&[mouse(MOUSEEVENTF_MOVE, *dx, *dy, 0)]),
        Message::MouseButton { button, down } => {
            let (flag, data) = match (button, down) {
                (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                (Button::X1, true) => (MOUSEEVENTF_XDOWN, 1),
                (Button::X1, false) => (MOUSEEVENTF_XUP, 1),
                (Button::X2, true) => (MOUSEEVENTF_XDOWN, 2),
                (Button::X2, false) => (MOUSEEVENTF_XUP, 2),
            };
            send(&[mouse(flag, 0, 0, data)]);
        }
        Message::Wheel { dx, dy } => {
            if *dy != 0 {
                send(&[mouse(MOUSEEVENTF_WHEEL, 0, 0, *dy)]);
            }
            if *dx != 0 {
                send(&[mouse(MOUSEEVENTF_HWHEEL, 0, 0, *dx)]);
            }
        }
        Message::Key {
            scancode,
            extended,
            down,
        } => {
            let mut flags = KEYEVENTF_SCANCODE;
            if *extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !*down {
                flags |= KEYEVENTF_KEYUP;
            }
            send(&[INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: Default::default(),
                        wScan: *scancode,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }]);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- clipboard

/// Changes whenever anything is copied: poll this to notice new clipboard content.
pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

struct Clip;

impl Clip {
    fn open() -> Option<Clip> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Clip);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

pub fn get_clipboard_text() -> Option<String> {
    let _c = Clip::open()?;
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let g = HGLOBAL(h.0);
        let p = GlobalLock(g) as *const u16;
        if p.is_null() {
            return None;
        }
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        let _ = GlobalUnlock(g);
        Some(s)
    }
}

unsafe fn put_global(bytes: &[u8]) -> Result<HGLOBAL> {
    let g = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| os_err("clipboard memory", e))?;
    let p = GlobalLock(g) as *mut u8;
    if p.is_null() {
        return Err(AppError::Io("clipboard memory".into()));
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    let _ = GlobalUnlock(g);
    Ok(g)
}

pub fn set_clipboard_text(text: &str) -> Result<()> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes: Vec<u8> = wide.iter().flat_map(|w| w.to_le_bytes()).collect();
    let _c = Clip::open().ok_or_else(|| AppError::Io("clipboard is busy".into()))?;
    unsafe {
        let g = put_global(&bytes)?;
        let _ = EmptyClipboard();
        SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(g.0)))
            .map_err(|e| os_err("set clipboard", e))?;
    }
    Ok(())
}

/// Files currently copied in Explorer (empty if the clipboard holds something else).
pub fn get_clipboard_files() -> Vec<PathBuf> {
    let Some(_c) = Clip::open() else {
        return vec![];
    };
    let mut out = Vec::new();
    unsafe {
        let Ok(h) = GetClipboardData(CF_HDROP.0 as u32) else {
            return out;
        };
        let drop = HDROP(h.0);
        let n = DragQueryFileW(drop, u32::MAX, None);
        for i in 0..n {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got])));
        }
    }
    out.retain(|p| p.is_file());
    out
}

/// Put files on the clipboard so Ctrl+V in Explorer pastes them.
pub fn set_clipboard_files(files: &[PathBuf]) -> Result<()> {
    let header = std::mem::size_of::<DROPFILES>();
    let mut bytes = vec![0u8; header];
    // DROPFILES: pFiles = header size, fWide = TRUE.
    bytes[0..4].copy_from_slice(&(header as u32).to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for f in files {
        for w in f.to_string_lossy().encode_utf16() {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
    }
    bytes.extend_from_slice(&[0, 0]);
    let _c = Clip::open().ok_or_else(|| AppError::Io("clipboard is busy".into()))?;
    unsafe {
        let g = put_global(&bytes)?;
        let _ = EmptyClipboard();
        SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(g.0)))
            .map_err(|e| os_err("set clipboard", e))?;
    }
    Ok(())
}

/// The user's real Desktop folder (follows OneDrive redirection).
pub fn desktop_dir() -> Result<PathBuf> {
    unsafe {
        let p: PWSTR = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None)
            .map_err(|e| os_err("find Desktop folder", e))?;
        let s = p
            .to_string()
            .map_err(|_| AppError::Io("Desktop folder name".into()));
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(PathBuf::from(s?))
    }
}

/// "<Desktop>\Shared Files"
pub fn shared_files_dir() -> Result<PathBuf> {
    Ok(desktop_dir()?.join("Shared Files"))
}

// ------------------------------------------------------------ input capture

/// What the global hooks report on the master PC.
#[derive(Debug, Clone, Copy)]
pub enum Captured {
    /// Cursor position (screen pixels).
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseButton {
        button: Button,
        down: bool,
    },
    Wheel {
        dx: i32,
        dy: i32,
    },
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
}

type Handler = Box<dyn Fn(Captured) -> bool + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();

fn dispatch(ev: Captured) -> bool {
    HANDLER.get().map(|h| h(ev)).unwrap_or(false)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        // Ignore movement we made ourselves (injected events).
        if info.flags & LLMHF_INJECTED == 0 {
            let hi = (info.mouseData >> 16) as i16 as i32;
            let ev = match wparam.0 as u32 {
                WM_MOUSEMOVE => Some(Captured::MouseMove {
                    x: info.pt.x,
                    y: info.pt.y,
                }),
                WM_LBUTTONDOWN => Some(Captured::MouseButton {
                    button: Button::Left,
                    down: true,
                }),
                WM_LBUTTONUP => Some(Captured::MouseButton {
                    button: Button::Left,
                    down: false,
                }),
                WM_RBUTTONDOWN => Some(Captured::MouseButton {
                    button: Button::Right,
                    down: true,
                }),
                WM_RBUTTONUP => Some(Captured::MouseButton {
                    button: Button::Right,
                    down: false,
                }),
                WM_MBUTTONDOWN => Some(Captured::MouseButton {
                    button: Button::Middle,
                    down: true,
                }),
                WM_MBUTTONUP => Some(Captured::MouseButton {
                    button: Button::Middle,
                    down: false,
                }),
                WM_XBUTTONDOWN | WM_XBUTTONUP => {
                    let button = if hi == 1 { Button::X1 } else { Button::X2 };
                    Some(Captured::MouseButton {
                        button,
                        down: wparam.0 as u32 == WM_XBUTTONDOWN,
                    })
                }
                WM_MOUSEWHEEL => Some(Captured::Wheel { dx: 0, dy: hi }),
                WM_MOUSEHWHEEL => Some(Captured::Wheel { dx: hi, dy: 0 }),
                _ => None,
            };
            if let Some(ev) = ev {
                if dispatch(ev) {
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if info.flags.0 & LLKHF_INJECTED.0 == 0 {
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            if down || up {
                let ev = Captured::Key {
                    scancode: info.scanCode as u16,
                    extended: info.flags.0 & LLKHF_EXTENDED.0 != 0,
                    down,
                };
                if dispatch(ev) {
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// Start the global mouse and keyboard hooks on their own thread.
/// `handler` returns true to swallow the event (it will not reach local apps).
/// It runs inside the hook, so it must return quickly.
pub fn start_capture(handler: impl Fn(Captured) -> bool + Send + Sync + 'static) -> Result<()> {
    HANDLER
        .set(Box::new(handler))
        .map_err(|_| AppError::Io("input capture already started".into()))?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
    std::thread::spawn(move || unsafe {
        let hmod = HINSTANCE::default();
        let m: std::result::Result<HHOOK, _> =
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hmod), 0);
        let k: std::result::Result<HHOOK, _> =
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), Some(hmod), 0);
        match (m, k) {
            (Ok(_), Ok(_)) => {
                let _ = tx.send(Ok(()));
            }
            (Err(e), _) | (_, Err(e)) => {
                let _ = tx.send(Err(os_err("install input hooks", e)));
                return;
            }
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, Some(HWND::default()), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
    rx.recv()
        .map_err(|_| AppError::Io("input hook thread stopped".into()))?
}
