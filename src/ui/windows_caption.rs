//! Keep the native Windows caption in step with the application theme.
//!
//! The caption stays native: Windows draws the title, the buttons, the border
//! and the system menu, and owns dragging and resizing. Only its colours are
//! set, through the documented DWM attributes, and only when the theme
//! changes - never once per frame and never by moving or resizing the window.
//!
//! * Windows 11 takes the exact caption, text and border colours.
//! * Windows 10 cannot colour the caption, but it does take the immersive dark
//!   mode attribute, which gives a dark caption with light text and buttons for
//!   the dark themes and the standard light caption for the light theme.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use egui::Color32;
use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
};
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
};

use super::theme::Tokens;

/// The main window, once eframe has created it. Zero until then, and in tests.
static HWND_ROOT: AtomicIsize = AtomicIsize::new(0);

/// What the caption should look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Want {
    dark: bool,
    caption: Color32,
    text: Color32,
}

/// What was last handed to DWM, and whether the window had focus then.
struct State {
    applied: Option<Want>,
    focused: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    applied: None,
    focused: false,
});

pub(super) fn initialize(cc: &eframe::CreationContext<'_>) {
    let Some(window) = cc.winit_window() else {
        return;
    };
    // Pin winit to its light path on every Windows version. Its dark path
    // (DarkMode_Explorer plus WCA_USEDARKMODECOLORS) leaves Win10 caption text
    // and buttons unreadable after activation, and with an explicit preference
    // winit no longer re-applies its own theme on system theme changes, which
    // would otherwise undo the attributes set below.
    window.set_theme(Some(winit::window::Theme::Light));

    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = window.window_handle() {
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            HWND_ROOT.store(win32.hwnd.get(), Ordering::Relaxed);
        }
    }
}

/// Paint the caption with the theme's surface and text colours.
///
/// Cheap to call every frame: DWM is only touched when the colours change, or
/// - on Windows 10, where activation repaints the caption - when the window
///   regains focus.
pub(super) fn follow(ctx: &egui::Context, tokens: &Tokens) {
    let hwnd = HWND_ROOT.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    let want = Want {
        dark: tokens.dark,
        caption: tokens.surface,
        text: tokens.text,
    };
    let focused = ctx.input(|input| input.viewport().focused.unwrap_or(false));
    let mut state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let changed = state.applied != Some(want);
    let regained = focused && !state.focused;
    state.focused = focused;
    if !changed && !(regained && !supports_caption_colours()) {
        return;
    }
    state.applied = Some(want);
    drop(state);
    apply(hwnd as HWND, want, changed);
}

fn apply(hwnd: HWND, want: Want, changed: bool) {
    let dark = i32::from(want.dark);
    // Windows 10 before build 18985 knows the dark mode attribute as 19.
    if !set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark) {
        set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1, &dark);
    }
    if supports_caption_colours() {
        // Windows 11 repaints the caption as soon as these are set.
        set(hwnd, DWMWA_CAPTION_COLOR, &colorref(want.caption));
        set(hwnd, DWMWA_TEXT_COLOR, &colorref(want.text));
        set(hwnd, DWMWA_BORDER_COLOR, &colorref(want.caption));
    } else if changed {
        // Windows 10 keeps the old caption until the frame is recalculated.
        // One refresh per theme change, without moving, sizing or focusing.
        unsafe {
            SetWindowPos(
                hwnd,
                core::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

const DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1: DWMWINDOWATTRIBUTE = 19;

fn set<T: Copy>(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: &T) -> bool {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute as u32,
            value as *const T as *const core::ffi::c_void,
            core::mem::size_of::<T>() as u32,
        ) >= 0
    }
}

/// A colour in the order Windows spells a `COLORREF`: `0x00BBGGRR`.
fn colorref(color: Color32) -> u32 {
    u32::from(color.r()) | (u32::from(color.g()) << 8) | (u32::from(color.b()) << 16)
}

/// Explicit caption colours arrived with Windows 11 (build 22000).
fn supports_caption_colours() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // RtlGetVersion reports the real version without a compatibility manifest.
        let status = unsafe { RtlGetVersion(&mut version) };
        status >= 0 && !is_windows_10(version.dwMajorVersion, version.dwBuildNumber)
    })
}

fn is_windows_10(major: u32, build: u32) -> bool {
    major < 10 || (major == 10 && build < 22000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_colours_are_limited_to_windows_11() {
        for (major, build) in [(6, 9600), (10, 10240), (10, 17763), (10, 19045)] {
            assert!(is_windows_10(major, build));
        }
        for (major, build) in [(10, 22000), (10, 22631), (10, 26100)] {
            assert!(!is_windows_10(major, build));
        }
    }

    #[test]
    fn colorref_is_bgr() {
        assert_eq!(colorref(Color32::from_rgb(0x12, 0x34, 0x56)), 0x0056_3412);
    }
}
