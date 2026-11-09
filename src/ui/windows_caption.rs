//! Keep Windows 10's native caption on its standard rendering path.

use std::sync::OnceLock;

use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

pub(super) fn initialize(cc: &eframe::CreationContext<'_>) {
    if let Some(window) = cc.winit_window() {
        restore(window);
    }
}

fn restore(window: &winit::window::Window) {
    if needs_standard_caption() {
        // Disabling egui's sync does not undo winit's startup override. Light
        // clears DarkMode_Explorer and WCA_USEDARKMODECOLORS together, leaving
        // Windows to draw the native text, buttons, border and system menu.
        window.set_theme(Some(winit::window::Theme::Light));
    }
}

fn needs_standard_caption() -> bool {
    static NEEDED: OnceLock<bool> = OnceLock::new();
    *NEEDED.get_or_init(|| {
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // RtlGetVersion reports the real version without a compatibility manifest.
        let status = unsafe { RtlGetVersion(&mut version) };
        status >= 0 && is_windows_10(version.dwMajorVersion, version.dwBuildNumber)
    })
}

fn is_windows_10(major: u32, build: u32) -> bool {
    major == 10 && build < 22000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_caption_workaround_is_limited_to_windows_10() {
        for build in [10240, 17763, 18362, 19041, 19045] {
            assert!(is_windows_10(10, build));
        }
        for (major, build) in [(6, 9600), (10, 22000), (10, 22631), (10, 26100)] {
            assert!(!is_windows_10(major, build));
        }
    }
}
