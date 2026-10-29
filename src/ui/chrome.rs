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

/// The frame colours the operating system was last given.
#[derive(Default)]
pub struct Chrome {
    applied: Option<Want>,
}

impl Chrome {
    /// Keep the native frame in step with the interface.
    ///
    /// Asking costs a system call per window, and the colours move when the
    /// theme does - twice a session, usually - so a frame that already agrees is
    /// left alone. Nothing is remembered until some window has taken the
    /// colours: the first frames run before there is one to paint.
    pub fn follow(&mut self, want: Want) {
        if !self.differs(want) {
            return;
        }
        if apply(want) {
            self.applied = Some(want);
        }
    }

    /// True when the frame has to be told: it either disagrees or was never asked.
    fn differs(&self, want: Want) -> bool {
        self.applied != Some(want)
    }
}

/// Paint every window of ours that has a title bar.
///
/// `true` once at least one window has taken the colours.
#[cfg(windows)]
fn apply(want: Want) -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible,
        GWL_STYLE, GW_OWNER, WS_CAPTION,
    };

    /// The windows of this process, and what the walk did to them.
    struct Walk {
        want: Want,
        /// A window belongs to us when this process owns it or this thread does:
        /// eframe paints from the thread that owns the window, and the worker
        /// threads of the SSH runtime have no windows to speak of.
        process: u32,
        thread: u32,
        touched: bool,
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
        // A Windows 10 has no caption colour and answers with an error, which is
        // why none of these is allowed to fail the frame: the immersive flag on
        // its own still keeps a dark interface from wearing a white hat.
        set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &dark);
        set(hwnd, DWMWA_CAPTION_COLOR as u32, &caption);
        set(hwnd, DWMWA_TEXT_COLOR as u32, &text);
        // The hairline around the glass, so a light title does not ring a dark
        // window either.
        set(hwnd, DWMWA_BORDER_COLOR as u32, &caption);
        walk.touched = true;
        1
    }

    unsafe fn set(hwnd: HWND, attribute: u32, value: &impl Copy) {
        let _ = DwmSetWindowAttribute(
            hwnd,
            attribute,
            value as *const _ as *const core::ffi::c_void,
            core::mem::size_of_val(value) as u32,
        );
    }

    let mut walk = Walk {
        want,
        process: unsafe { GetCurrentProcessId() },
        thread: unsafe { GetCurrentThreadId() },
        touched: false,
    };
    unsafe {
        EnumWindows(Some(each), &mut walk as *mut Walk as LPARAM);
    }
    walk.touched
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

    /// A frame that already agrees is left alone; any move in the colours, the
    /// title or the dark flag has to be worth another look.
    #[test]
    fn only_a_moved_frame_is_asked_again() {
        let want = Want {
            caption: Color32::from_rgb(0x17, 0x1b, 0x21),
            text: Color32::WHITE,
            dark: true,
        };
        let mut chrome = Chrome::default();
        assert!(chrome.differs(want), "nothing has been asked yet");
        chrome.applied = Some(want);
        assert!(!chrome.differs(want), "a frame that agrees is left alone");
        for changed in [
            Want {
                caption: Color32::WHITE,
                ..want
            },
            Want {
                text: Color32::BLACK,
                ..want
            },
            Want {
                dark: false,
                ..want
            },
        ] {
            assert!(chrome.differs(changed), "{changed:?} was not noticed");
        }
    }
}
