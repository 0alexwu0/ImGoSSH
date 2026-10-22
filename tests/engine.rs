//! End-to-end checks that do not need a network: the pseudo-console and the
//! terminal emulator have to agree about what the user sees.

use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use imgossh::ssh::client::{TermEvent, TermSink};
use imgossh::ssh::local::LocalPty;
use imgossh::terminal::Emulator;

fn marker() -> String {
    format!("IMGOSSTEST{}", std::process::id())
}

/// A pseudo-console wired to an emulator the same way a terminal tab is.
///
/// Shells ask questions before they print a prompt - PowerShell opens with
/// "where is the cursor?" - and stall forever unless the emulator's answers go
/// back into the channel. Harness::pump does exactly what a tab does.
struct Harness {
    pty: LocalPty,
    emu: Arc<Mutex<Emulator>>,
    replies: Receiver<Vec<u8>>,
}

impl Harness {
    fn spawn() -> Self {
        let emu = Arc::new(Mutex::new(Emulator::new(24, 80, 200)));
        let (reply_tx, replies) = channel::<Vec<u8>>();
        let sink = {
            let emu = emu.clone();
            TermSink::new(move |event| match event {
                TermEvent::Data(bytes) => {
                    let responses = emu.lock().unwrap().feed(&bytes);
                    if !responses.is_empty() {
                        let _ = reply_tx.send(responses);
                    }
                }
                TermEvent::Closed { .. } => {}
            })
        };
        let pty = LocalPty::spawn(None, 80, 24, sink).expect("spawning a local shell");
        Self { pty, emu, replies }
    }

    /// Hand anything the shell asked for back to it.
    fn pump(&self) {
        while let Ok(bytes) = self.replies.try_recv() {
            self.pty.send(bytes);
        }
    }

    fn text(&self) -> String {
        self.pump();
        self.emu.lock().unwrap().visible_text()
    }

    fn wait_for_text(&self, needle: &str, attempts: usize) -> bool {
        for _ in 0..attempts {
            if self.text().contains(needle) {
                return true;
            }
            sleep(Duration::from_millis(100));
        }
        false
    }

    fn wait_for_prompt(&self) {
        for _ in 0..50 {
            if !self.text().trim().is_empty() {
                return;
            }
            sleep(Duration::from_millis(100));
        }
    }
}

#[test]
fn local_shell_echoes_what_we_type() {
    let shell = Harness::spawn();
    assert!(shell.pty.is_alive(), "the shell should stay running");

    shell.wait_for_prompt();

    let needle = marker();
    let mut echoed = false;
    for _ in 0..10 {
        shell.pty.send(format!("echo {needle}\r").into_bytes());
        echoed = shell.wait_for_text(&needle, 10);
        if echoed {
            break;
        }
    }

    assert!(echoed, "the shell never echoed back: {:?}", shell.text());
    shell.pty.close();
}

#[test]
fn emulator_answers_device_attributes() {
    let mut emu = Emulator::new(24, 80, 0);
    let responses = emu.feed(b"\x1b[0c");
    let text = String::from_utf8_lossy(&responses).into_owned();
    assert!(
        text.starts_with("\x1b[?"),
        "expected a CSI ? response, got {text:?}"
    );
}

#[test]
fn emulator_answers_cursor_position_request() {
    let mut emu = Emulator::new(24, 80, 0);
    let responses = emu.feed(b"\x1b[6n");
    let text = String::from_utf8_lossy(&responses).into_owned();
    assert!(
        text.starts_with("\x1b[1;") && text.ends_with('R'),
        "expected a cursor position report, got {text:?}"
    );
}

#[test]
fn emulator_tracks_title_and_cursor() {
    let mut emu = Emulator::new(4, 20, 0);
    emu.feed(b"\x1b]2;prod-01\x07hello");
    assert_eq!(emu.title, "prod-01");
    let (row, col) = emu.cursor();
    assert_eq!((row, col), (0, 5));
    assert_eq!(emu.size(), (4, 20));
}

#[test]
fn scrollback_reaches_the_top_and_back() {
    let mut emu = Emulator::new(6, 40, 200);
    for index in 0..30 {
        emu.feed(format!("line {index}\r\n").as_bytes());
    }
    assert!(emu.scroll(4) > 0, "scrolling up should move the viewport");
    emu.scroll_to_bottom();
    assert_eq!(
        emu.scroll_offset, 0,
        "scroll_to_bottom must reset the offset"
    );
}
