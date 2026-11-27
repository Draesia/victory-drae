//! The title's time of day on hotkeys, while our window has the focus:
//! F5 an hour back, F6 an hour on, F7 holds the time where it is (again:
//! lets it run). The time is shown for a few seconds in the panel's box.
//!
//! Only for the one build of the title whose clock was found:
//! `game_scarlett_master.exe` 1.0.2.1, mapped as
//! `victory-image-b99f876b74d4a77f.dll`. Its clock is four 32-bit fields at
//! image offset `0xAB5CDD0` (hour, minute, second, millisecond): the clock's
//! tick at `+0x1512200` reads hour and minute there and writes the time
//! back, and the title's set-clock-time function at `+0x1ab9b20` writes the
//! new time there and then sets the byte at `+0x7F028E9` to 1, which is
//! done here too. For any other build the module is not found and the keys
//! do nothing. A mission may set its own time, as the title's scripts do;
//! holding the time puts it back.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F5, VK_F6, VK_F7};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// The image of the one build this knows.
const IMAGE: &str = "victory-image-b99f876b74d4a77f.dll";
/// Hour, minute, second and millisecond, 32 bits each.
const CLOCK: usize = 0xAB5CDD0;
/// Set to 1 after a new time, as the title's own set-clock-time does.
const CHANGED: usize = 0x7F028E9;
/// How long the time stays shown after a key.
const SHOWN_FOR: Duration = Duration::from_secs(3);

struct State {
    /// The keys' states at the last look: F5, F6, F7.
    down: [bool; 3],
    /// The time held (F7), as hour and minute.
    held: Option<(u32, u32)>,
    /// Until when the time is shown.
    until: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State { down: [false; 3], held: None, until: None });

/// The title image's base, if it is the known build.
fn image() -> Option<usize> {
    let name: Vec<u16> = IMAGE.encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated name; the module is not unloaded while the title runs.
    let base = unsafe { GetModuleHandleW(name.as_ptr()) } as usize;
    (base != 0).then_some(base)
}

fn ours_in_front() -> bool {
    let mut process = 0;
    // SAFETY: plain queries; room for the answer.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut process) };
    process == std::process::id()
}

/// The clock's hour and minute.
fn read(base: usize) -> (u32, u32) {
    let at = (base + CLOCK) as *const u32;
    // SAFETY: the clock's fields in the title's image, which stays mapped.
    unsafe { (at.read_volatile(), at.add(1).read_volatile()) }
}

/// Sets the clock to `hour:minute:00.000` and tells the title it changed.
fn write(base: usize, hour: u32, minute: u32) {
    let at = (base + CLOCK) as *mut u32;
    // SAFETY: as in `read`; the title writes these fields itself, a field at a time.
    unsafe {
        at.write_volatile(hour);
        at.add(1).write_volatile(minute);
        at.add(2).write_volatile(0);
        at.add(3).write_volatile(0);
        ((base + CHANGED) as *mut u8).write_volatile(1);
    }
}

/// A frame was presented: the keys are looked at, a held time put back.
/// The line to show, while it is to be shown.
pub(super) fn frame() -> Option<String> {
    let base = image()?;
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let mut pressed = [false; 3];
    for (n, key) in [VK_F5, VK_F6, VK_F7].into_iter().enumerate() {
        // SAFETY: a plain query.
        let down = unsafe { GetAsyncKeyState(i32::from(key)) } < 0;
        pressed[n] = down && !state.down[n];
        state.down[n] = down;
    }
    if pressed.iter().any(|&p| p) && ours_in_front() {
        let (hour, minute) = read(base);
        if hour < 24 && minute < 60 {
            if pressed[0] || pressed[1] {
                let hour = if pressed[0] { (hour + 23) % 24 } else { (hour + 1) % 24 };
                write(base, hour, minute);
                if state.held.is_some() {
                    state.held = Some((hour, minute));
                }
            }
            if pressed[2] {
                state.held = match state.held {
                    Some(_) => None,
                    None => Some((hour, minute)),
                };
            }
            tracing::info!(held = state.held.is_some(), "time of day: {:02}:{:02}", read(base).0, read(base).1);
            state.until = Some(Instant::now() + SHOWN_FOR);
        }
    }
    // A held time is put back when something else changed it.
    if let Some((hour, minute)) = state.held {
        if read(base) != (hour, minute) {
            write(base, hour, minute);
        }
    }
    if state.until.is_some_and(|until| Instant::now() < until) {
        let (hour, minute) = read(base);
        let held = if state.held.is_some() { "  HELD" } else { "" };
        return Some(format!("TIME {hour:02}:{minute:02}{held}"));
    }
    state.until = None;
    None
}
