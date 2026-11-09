//! Keep the native caption in theme, with an application-drawn fallback when
//! the compositor cannot set its colours (including Windows 10).

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
pub struct Chrome {
    custom: bool,
    desired: Option<Want>,
    applied: Option<Want>,
    focused: bool,
    attempts_left: u8,
    next_attempt: Option<std::time::Instant>,
}

impl Default for Chrome {
    fn default() -> Self {
        Self {
            custom: startup_custom_caption(),
            desired: None,
            applied: None,
            focused: false,
            attempts_left: 0,
            next_attempt: None,
        }
    }
}

/// Decide before creating the HWND, so Win10 never shows a native white caption
/// while waiting for the runtime colour retries. Share the choice with Chrome.
pub fn startup_viewport(viewport: egui::ViewportBuilder) -> egui::ViewportBuilder {
    configure_viewport(viewport, startup_custom_caption())
}

fn configure_viewport(viewport: egui::ViewportBuilder, custom: bool) -> egui::ViewportBuilder {
    if custom {
        viewport.with_decorations(false)
    } else {
        viewport
    }
}

fn startup_custom_caption() -> bool {
    #[cfg(windows)]
    {
        static CUSTOM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *CUSTOM.get_or_init(|| {
            use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
            // GetVersionEx depends on the executable's compatibility manifest.
            // RtlGetVersion reports the actual build (Win11 starts at 22000).
            #[link(name = "ntdll")]
            unsafe extern "system" {
                fn RtlGetVersion(version: *mut OSVERSIONINFOW) -> i32;
            }
            let mut version = OSVERSIONINFOW {
                dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
                ..Default::default()
            };
            let succeeded = unsafe { RtlGetVersion(&mut version) } >= 0;
            needs_custom_caption(
                succeeded.then_some((version.dwMajorVersion, version.dwBuildNumber)),
            )
        })
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(any(windows, test))]
fn needs_custom_caption(version: Option<(u32, u32)>) -> bool {
    !matches!(version, Some((major, build)) if major > 10 || (major == 10 && build >= 22000))
}

const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
const ATTEMPTS: u8 = 5;

impl Chrome {
    pub fn follow(&mut self, ctx: &egui::Context, want: Want) {
        self.follow_with(ctx, want, std::time::Instant::now(), apply);
    }

    fn follow_with(
        &mut self,
        ctx: &egui::Context,
        want: Want,
        now: std::time::Instant,
        apply: impl FnMut(Want) -> bool,
    ) {
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
        if self.custom {
            self.desired = Some(want);
            return;
        }
        if let Some(delay) = self.update(want, focused, now, apply) {
            ctx.request_repaint_after(delay);
        }
        // Win10 cannot set an exact caption colour. A successful dark-mode
        // attribute alone does not mean the frame follows the application.
        if cfg!(windows) && self.attempts_left == 0 && self.applied != Some(want) {
            self.custom = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
            ctx.request_repaint();
        }
    }

    /// Draw the caption when the native compositor cannot use our colours.
    pub fn show(&self, ui: &mut egui::Ui, want: Want) {
        if !self.custom || ui.input(|i| i.viewport().fullscreen == Some(true)) {
            return;
        }
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        egui::Panel::top("application-caption")
            .exact_size(32.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(want.caption))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let buttons_left = rect.right() - 3.0 * 46.0;
                let drag_rect =
                    egui::Rect::from_min_max(rect.min, egui::pos2(buttons_left, rect.bottom()));
                let response = ui.interact(
                    drag_rect,
                    ui.id().with("drag"),
                    egui::Sense::click_and_drag(),
                );
                if response.double_clicked() {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                } else if response.drag_started_by(egui::PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                let mut logo =
                    ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        rect.min + egui::vec2(8.0, 7.0),
                        egui::vec2(18.0, 18.0),
                    )));
                super::theme::logo(&mut logo, 18.0);
                let title = ui.input(|i| i.viewport().title.clone()).unwrap_or_else(|| {
                    format!("{} {}", crate::config::APP_NAME, crate::config::APP_VERSION)
                });
                ui.painter().with_clip_rect(drag_rect).text(
                    rect.min + egui::vec2(34.0, 16.0),
                    egui::Align2::LEFT_CENTER,
                    title,
                    egui::FontId::proportional(12.0),
                    want.text,
                );
                for (index, label, command) in [
                    (
                        0,
                        crate::t!("Minimize"),
                        egui::ViewportCommand::Minimized(true),
                    ),
                    (
                        1,
                        if maximized {
                            crate::t!("Restore")
                        } else {
                            crate::t!("Maximize")
                        },
                        egui::ViewportCommand::Maximized(!maximized),
                    ),
                    (2, crate::t!("Close"), egui::ViewportCommand::Close),
                ] {
                    let button = egui::Rect::from_min_size(
                        egui::pos2(buttons_left + index as f32 * 46.0, rect.top()),
                        egui::vec2(46.0, 32.0),
                    );
                    let response = ui.interact(button, ui.id().with(index), egui::Sense::click());
                    let mut color = want.text;
                    if response.hovered() {
                        let fill = if index == 2 {
                            color = Color32::WHITE;
                            Color32::from_rgb(196, 43, 28)
                        } else {
                            ui.visuals().widgets.hovered.bg_fill
                        };
                        ui.painter().rect_filled(button, 0.0, fill);
                    }
                    let centre = button.center();
                    let icon = egui::Rect::from_center_size(centre, egui::vec2(10.0, 10.0));
                    let stroke = egui::Stroke::new(1.0, color);
                    match index {
                        0 => {
                            ui.painter().hline(icon.x_range(), centre.y, stroke);
                        }
                        1 => {
                            if maximized {
                                ui.painter().rect_stroke(
                                    icon.translate(egui::vec2(2.0, -2.0)),
                                    0.0,
                                    stroke,
                                    egui::StrokeKind::Inside,
                                );
                            }
                            ui.painter()
                                .rect_stroke(icon, 0.0, stroke, egui::StrokeKind::Inside);
                        }
                        _ => {
                            ui.painter()
                                .line_segment([icon.left_top(), icon.right_bottom()], stroke);
                            ui.painter()
                                .line_segment([icon.right_top(), icon.left_bottom()], stroke);
                        }
                    }
                    if response.clicked() {
                        ui.ctx().send_viewport_cmd(command);
                    }
                    response.on_hover_text(label);
                }
            });
    }

    /// Borderless windows need explicit resize handles, including all corners.
    pub fn resize_frame(&self, ui: &egui::Ui) {
        if !self.custom
            || ui.input(|i| {
                i.viewport().maximized == Some(true) || i.viewport().fullscreen == Some(true)
            })
        {
            return;
        }
        let rect = ui.ctx().viewport_rect();
        let edge = 5.0;
        use egui::{CursorIcon as C, ResizeDirection as D};
        for (index, (x, y, direction, cursor)) in [
            (0, 0, D::NorthWest, C::ResizeNwSe),
            (1, 0, D::North, C::ResizeVertical),
            (2, 0, D::NorthEast, C::ResizeNeSw),
            (0, 1, D::West, C::ResizeHorizontal),
            (2, 1, D::East, C::ResizeHorizontal),
            (0, 2, D::SouthWest, C::ResizeNeSw),
            (1, 2, D::South, C::ResizeVertical),
            (2, 2, D::SouthEast, C::ResizeNwSe),
        ]
        .into_iter()
        .enumerate()
        {
            let axis = |min: f32, max: f32, part| match part {
                0 => (min, min + edge),
                1 => (min + edge, max - edge),
                _ => (max - edge, max),
            };
            let (left, right) = axis(rect.left(), rect.right(), x);
            let (top, bottom) = axis(rect.top(), rect.bottom(), y);
            egui::Area::new(egui::Id::new(("frame-resize", index)))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(left, top))
                .movable(false)
                .show(ui.ctx(), |ui| {
                    let (_, response) = ui.allocate_exact_size(
                        egui::vec2(right - left, bottom - top),
                        egui::Sense::drag(),
                    );
                    if response.drag_started_by(egui::PointerButton::Primary) {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
                    }
                    response.on_hover_cursor(cursor);
                });
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
                "Native title bar colours unavailable; using an application-drawn title bar on Windows"
            );
        }
        self.next_attempt = (self.attempts_left > 0).then_some(now + RETRY_DELAY);
        self.next_attempt.map(|due| due - now)
    }
}

/// Paint every window of ours that has a title bar.
///
/// `true` when visible captioned windows were found and accepted dark mode
/// and the exact caption colours. Otherwise use the application-drawn frame.
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
        // Windows 10 before build 18985 uses attribute 19. Explicit caption
        // and text colours require Windows 11; failure selects the custom frame.
        let dark_applied = set_dark_attribute(|attribute| set(hwnd, attribute, &dark));
        let caption_applied = set(hwnd, DWMWA_CAPTION_COLOR as u32, &caption);
        let text_applied = set(hwnd, DWMWA_TEXT_COLOR as u32, &text);
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
        walk.success &= dark_applied && caption_applied && text_applied && refreshed;
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

    #[test]
    fn win10_starts_with_the_themed_caption_without_runtime_replacement() {
        for (version, custom) in [
            (Some((10, 17763)), true),
            (Some((10, 19045)), true),
            (Some((10, 21999)), true),
            (Some((10, 22000)), false),
            (Some((10, 26100)), false),
            (Some((11, 0)), false),
            (None, true),
        ] {
            assert_eq!(needs_custom_caption(version), custom);
            let viewport = configure_viewport(egui::ViewportBuilder::default(), custom);
            assert_eq!(
                viewport.decorations,
                if custom { Some(false) } else { None }
            );
            if !custom {
                continue;
            }
            let ctx = egui::Context::default();
            let mut chrome = Chrome {
                custom,
                ..Default::default()
            };
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                chrome.follow_with(ui.ctx(), want(), std::time::Instant::now(), |_| {
                    panic!("Win10 must not wait for DWM retries")
                });
                chrome.show(ui, want());
            });
            output.textures_delta.clear();
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.fill == want().caption && rect.rect.height() == 32.0)));
            assert!(!output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Decorations(_))));
            assert_eq!(chrome.attempts_left, 0);
        }
        assert_eq!(
            Chrome::default().custom,
            startup_viewport(egui::ViewportBuilder::default()).decorations == Some(false)
        );
    }

    #[cfg(windows)]
    #[test]
    fn unavailable_caption_colours_switch_to_custom_frame() {
        for supported in [false, true] {
            let ctx = egui::Context::default();
            let mut chrome = Chrome {
                custom: false,
                ..Default::default()
            };
            let start = std::time::Instant::now();
            for step in 0..ATTEMPTS {
                ctx.begin_pass(egui::RawInput::default());
                chrome.follow_with(&ctx, want(), start + RETRY_DELAY * u32::from(step), |_| {
                    supported
                });
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                let hides_frame = output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .contains(&egui::ViewportCommand::Decorations(false));
                assert_eq!(hides_frame, !supported && step == ATTEMPTS - 1);
            }
            assert_eq!(chrome.custom, !supported);
            if chrome.custom {
                ctx.begin_pass(egui::RawInput::default());
                let light = Want {
                    caption: Color32::WHITE,
                    text: Color32::BLACK,
                    dark: false,
                };
                chrome.follow_with(&ctx, light, start + RETRY_DELAY * 10, |_| {
                    panic!("custom frame called DWM")
                });
                assert_eq!(chrome.desired, Some(light));
                ctx.end_pass().textures_delta.clear();
            }
        }
    }

    fn caption_frame(
        ctx: &egui::Context,
        want: Want,
        maximized: bool,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 450.0),
            )),
            events,
            ..Default::default()
        };
        raw.viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .maximized = Some(maximized);
        let chrome = Chrome {
            custom: true,
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| {
            chrome.show(ui, want);
            chrome.resize_frame(ui);
        });
        output.textures_delta.clear();
        output
    }

    #[test]
    fn custom_caption_tracks_colours_and_window_buttons() {
        for want in [
            want(),
            Want {
                caption: Color32::WHITE,
                text: Color32::BLACK,
                dark: false,
            },
        ] {
            let ctx = egui::Context::default();
            let output = caption_frame(&ctx, want, false, vec![]);
            assert!(output.shapes.iter().any(|clipped| matches!(&clipped.shape,
                egui::Shape::Rect(rect) if rect.fill == want.caption && rect.rect.height() == 32.0)));
            for (index, maximized, expected) in [
                (0, false, egui::ViewportCommand::Minimized(true)),
                (1, false, egui::ViewportCommand::Maximized(true)),
                (1, true, egui::ViewportCommand::Maximized(false)),
                (2, false, egui::ViewportCommand::Close),
            ] {
                let ctx = egui::Context::default();
                caption_frame(&ctx, want, maximized, vec![]);
                let pos = egui::pos2(800.0 - 138.0 + index as f32 * 46.0 + 23.0, 16.0);
                for pressed in [true, false] {
                    let output = caption_frame(
                        &ctx,
                        want,
                        maximized,
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::default(),
                            },
                        ],
                    );
                    if !pressed {
                        assert!(
                            output.viewport_output[&egui::ViewportId::ROOT]
                                .commands
                                .contains(&expected),
                            "caption button did not send {expected:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn custom_frame_can_drag_and_resize_from_its_edges() {
        for (start, end, expected) in [
            (
                egui::pos2(200.0, 16.0),
                egui::pos2(220.0, 16.0),
                egui::ViewportCommand::StartDrag,
            ),
            (
                egui::pos2(2.0, 200.0),
                egui::pos2(20.0, 200.0),
                egui::ViewportCommand::BeginResize(egui::ResizeDirection::West),
            ),
            (
                egui::pos2(798.0, 448.0),
                egui::pos2(780.0, 430.0),
                egui::ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast),
            ),
        ] {
            let ctx = egui::Context::default();
            for _ in 0..2 {
                caption_frame(&ctx, want(), false, vec![]);
            }
            let mut commands = Vec::new();
            for events in [
                vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                vec![egui::Event::PointerMoved(end)],
            ] {
                let output = caption_frame(&ctx, want(), false, events);
                commands.extend(
                    output.viewport_output[&egui::ViewportId::ROOT]
                        .commands
                        .clone(),
                );
            }
            assert!(
                commands.contains(&expected),
                "frame did not send {expected:?}: {commands:?}"
            );
        }
    }

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
