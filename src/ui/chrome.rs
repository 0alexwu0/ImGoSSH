//! The window frame the operating system paints: the bar the title lives in.
//!
//! egui draws everything below the caption and nothing above it, so the strip
//! handed to us by the window manager is the one part of the interface these
//! themes cannot reach by drawing. Left alone it keeps a frame of its own about
//! dark and light - and turns white in the middle of a dark window - so it is
//! told what the panels were painted with whenever that moves.

use egui::Color32;

/// What the native frame should be painted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    /// The bar itself: the surface every panel under it sits on.
    pub caption: Color32,
    /// The title, which has to read against `caption`.
    pub text: Color32,
    /// True when the bar is dark, so the system draws its own glyphs light.
    pub dark: bool,
}

/// Native theme changes can finish after the first egui frame. Reapply for a
/// bounded startup interval and whenever focus returns, without polling at idle.
#[derive(Default)]
pub struct Chrome {
    desired: Option<Want>,
    applied: Option<Want>,
    focused: bool,
    attempts_left: u8,
    next_attempt: Option<std::time::Instant>,
}

const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
const ATTEMPTS: u8 = 5;

impl Chrome {
    pub fn follow(&mut self, ctx: &egui::Context, want: Want) {
        if self.desired != Some(want) {
            // Set winit's preferred theme too, so later system theme messages
            // cannot undo the application theme on Windows 10.
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if want.dark {
                egui::SystemTheme::Dark
            } else {
                egui::SystemTheme::Light
            }));
        }
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(false));
        if let Some(delay) = self.update(want, focused, std::time::Instant::now(), apply) {
            ctx.request_repaint_after(delay);
        }
    }

    fn update(
        &mut self,
        want: Want,
        focused: bool,
        now: std::time::Instant,
        mut apply: impl FnMut(Want) -> bool,
    ) -> Option<std::time::Duration> {
        if self.desired != Some(want) || (focused && !self.focused) {
            self.desired = Some(want);
            self.attempts_left = ATTEMPTS;
            self.next_attempt = Some(now);
        }
        self.focused = focused;
        if self.attempts_left == 0 {
            return None;
        }
        let due = self.next_attempt.unwrap_or(now);
        if now < due {
            return Some(due - now);
        }
        if apply(want) {
            self.applied = Some(want);
        } else {
            self.applied = None;
        }
        self.attempts_left -= 1;
        if self.attempts_left == 0 && self.applied != Some(want) {
            log::warn!(
                "Native title bar theme could not be applied; will retry when focus returns"
            );
        }
        self.next_attempt = (self.attempts_left > 0).then_some(now + RETRY_DELAY);
        self.next_attempt.map(|due| due - now)
    }
}

/// Paint every window of ours that has a title bar.
///
/// `true` when visible captioned windows were found and accepted dark mode
/// and the frame refresh. Win11-only colour attributes are optional on Win10.
#[cfg(windows)]
fn apply(want: Want) -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible,
        SetWindowPos, GWL_STYLE, GW_OWNER, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, WS_CAPTION,
    };

    /// The windows of this process, and what the walk did to them.
    struct Walk {
        want: Want,
        /// A window belongs to us when this process owns it or this thread does:
        /// eframe paints from the thread that owns the window, and the worker
        /// threads of the SSH runtime have no windows to speak of.
        process: u32,
        thread: u32,
        found: bool,
        success: bool,
    }

    unsafe extern "system" fn each(hwnd: HWND, walk: LPARAM) -> i32 {
        let walk = &mut *(walk as *mut Walk);
        if hwnd.is_null()
            || IsWindowVisible(hwnd) != TRUE
            // An owned window is a dialog or a tooltip: it keeps its own frame.
            || !GetWindow(hwnd, GW_OWNER).is_null()
        {
            return 1;
        }
        let mut pid = 0;
        let tid = GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != walk.process && tid != walk.thread {
            return 1;
        }
        // Anything without a caption of its own has no title to colour.
        if (GetWindowLongW(hwnd, GWL_STYLE) as u32) & WS_CAPTION == 0 {
            return 1;
        }
        let dark = i32::from(walk.want.dark);
        let caption = colorref(walk.want.caption);
        let text = colorref(walk.want.text);
        // Windows 10 before build 18985 uses attribute 19. Explicit caption,
        // text and border colours are Windows 11 additions and remain optional.
        let dark_applied = set_dark_attribute(|attribute| set(hwnd, attribute, &dark));
        set(hwnd, DWMWA_CAPTION_COLOR as u32, &caption);
        set(hwnd, DWMWA_TEXT_COLOR as u32, &text);
        set(hwnd, DWMWA_BORDER_COLOR as u32, &caption);
        // Recalculate the non-client frame without moving, resizing or focusing
        // the window. Win10 may otherwise keep the previous caption cached.
        let refreshed = SetWindowPos(
            hwnd,
            core::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        ) != 0;
        walk.found = true;
        walk.success &= dark_applied && refreshed;
        1
    }

    unsafe fn set(hwnd: HWND, attribute: u32, value: &impl Copy) -> bool {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            value as *const _ as *const core::ffi::c_void,
            core::mem::size_of_val(value) as u32,
        ) >= 0
    }

    let mut walk = Walk {
        want,
        process: unsafe { GetCurrentProcessId() },
        thread: unsafe { GetCurrentThreadId() },
        found: false,
        success: true,
    };
    unsafe {
        EnumWindows(Some(each), &mut walk as *mut Walk as LPARAM);
    }
    walk.found && walk.success
}

/// Older Win10 releases reject attribute 20 but accept its predecessor, 19.
#[cfg(any(windows, test))]
fn set_dark_attribute(mut set: impl FnMut(u32) -> bool) -> bool {
    set(20) || set(19)
}

/// A colour in the order Windows spells a `COLORREF`: `0x00BBGGRR`.
#[cfg(windows)]
fn colorref(color: Color32) -> u32 {
    u32::from(color.r()) | (u32::from(color.g()) << 8) | (u32::from(color.b()) << 16)
}

/// There is no frame to keep in step: the systems with a native one of their
/// own already follow the appearance the interface asks for.
#[cfg(not(windows))]
fn apply(_want: Want) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte order is the classic mistake, and it stays invisible until a
    /// blue interface turns orange.
    #[cfg(windows)]
    #[test]
    fn a_caption_colour_is_a_windows_colorref() {
        assert_eq!(colorref(Color32::from_rgb(0x12, 0x34, 0x56)), 0x0056_3412);
        assert_eq!(colorref(Color32::BLACK), 0);
        assert_eq!(colorref(Color32::RED), 0x0000_00ff);
        assert_eq!(colorref(Color32::BLUE), 0x00ff_0000);
    }

    fn want() -> Want {
        Want {
            caption: Color32::BLACK,
            text: Color32::WHITE,
            dark: true,
        }
    }

    #[test]
    fn win10_falls_back_only_when_modern_attribute_fails() {
        let mut attributes = Vec::new();
        assert!(set_dark_attribute(|attribute| {
            attributes.push(attribute);
            attribute == 19
        }));
        assert_eq!(attributes, [20, 19]);
        attributes.clear();
        assert!(set_dark_attribute(|attribute| {
            attributes.push(attribute);
            true
        }));
        assert_eq!(attributes, [20]);
        assert!(!set_dark_attribute(|_| false));
    }

    #[test]
    fn startup_retries_even_after_success_then_stops_at_idle() {
        let mut chrome = Chrome::default();
        let start = std::time::Instant::now();
        let mut attempts = 0;
        for step in 0..ATTEMPTS {
            chrome.update(want(), true, start + RETRY_DELAY * u32::from(step), |_| {
                attempts += 1;
                true
            });
        }
        assert_eq!(attempts, ATTEMPTS);
        assert_eq!(chrome.applied, Some(want()));
        assert!(chrome
            .update(want(), true, start + RETRY_DELAY * 10, |_| panic!(
                "idle frame retried"
            ))
            .is_none());
    }

    #[test]
    fn failed_or_unavailable_window_retries_and_focus_or_theme_restarts_it() {
        let mut chrome = Chrome::default();
        let start = std::time::Instant::now();
        assert_eq!(
            chrome.update(want(), false, start, |_| false),
            Some(RETRY_DELAY)
        );
        assert_eq!(chrome.applied, None);
        chrome.update(want(), false, start + RETRY_DELAY / 2, |_| {
            panic!("retried too early")
        });
        chrome.update(want(), false, start + RETRY_DELAY, |_| true);
        assert_eq!(chrome.applied, Some(want()));
        chrome.attempts_left = 0;
        assert_eq!(
            chrome.update(want(), true, start + RETRY_DELAY * 2, |_| true),
            Some(RETRY_DELAY)
        );
        chrome.attempts_left = 0;
        let light = Want {
            caption: Color32::WHITE,
            text: Color32::BLACK,
            dark: false,
        };
        chrome.update(light, true, start + RETRY_DELAY * 3, |_| true);
        assert_eq!(chrome.applied, Some(light));
        assert_eq!(chrome.attempts_left, ATTEMPTS - 1);
    }
}
