//! A separate Win32 screen buffer keeps server output on the original console.
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Console::*, Threading::WaitForSingleObject},
};

static INPUT_MODE: OnceLock<u32> = OnceLock::new();
static ACTIVE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
pub(super) static GONE: AtomicBool = AtomicBool::new(false);
pub(super) static STOPS: AtomicU64 = AtomicU64::new(0);

fn ok(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn input() -> HANDLE {
    std::io::stdin().as_raw_handle()
}
fn output() -> HANDLE {
    std::io::stderr().as_raw_handle()
}
fn mode(h: HANDLE) -> io::Result<u32> {
    let mut mode = 0;
    // SAFETY: mode is writable and the console API validates the handle.
    unsafe {
        ok(GetConsoleMode(h, &mut mode))?;
    }
    Ok(mode)
}

pub(super) fn foreground() -> bool {
    mode(input()).is_ok() && mode(output()).is_ok()
}
pub(super) fn arm() -> io::Result<()> {
    let mode = mode(input())?;
    let _ = INPUT_MODE.set(mode);
    // SAFETY: the standard console input remains open. Keep Ctrl+C processed.
    unsafe {
        ok(SetConsoleMode(
            input(),
            (mode | ENABLE_PROCESSED_INPUT) & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT),
        ))
    }
}
pub(super) fn apply() {}
pub(crate) fn console_size() -> io::Result<(u16, u16)> {
    let active = ACTIVE.load(Ordering::Acquire);
    let h = if active.is_null() { output() } else { active };
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: the console API validates the handle and fills info.
    unsafe {
        ok(GetConsoleScreenBufferInfo(h, &mut info))?;
    }
    let rect = info.srWindow;
    Ok((
        (rect.Right - rect.Left + 1) as u16,
        (rect.Bottom - rect.Top + 1) as u16,
    ))
}
pub(super) fn size() -> Option<(usize, usize)> {
    console_size().ok().map(|(w, h)| (w.into(), h.into()))
}

pub(super) fn key(ms: i32) -> io::Result<Option<u8>> {
    // SAFETY: stdin is the console this view armed.
    match unsafe { WaitForSingleObject(input(), ms as u32) } {
        WAIT_TIMEOUT => return Ok(None),
        WAIT_OBJECT_0 => (),
        _ => return Err(io::Error::last_os_error()),
    }
    let mut record = INPUT_RECORD::default();
    let mut read = 0;
    // SAFETY: one live input record and count are writable.
    unsafe {
        ok(ReadConsoleInputW(input(), &mut record, 1, &mut read))?;
    }
    if read == 0 || u32::from(record.EventType) != KEY_EVENT {
        return Ok(None);
    }
    // SAFETY: KEY_EVENT selects the initialized key record and UnicodeChar.
    let key = unsafe { record.Event.KeyEvent };
    if key.bKeyDown == 0 {
        return Ok(None);
    }
    // SAFETY: the wide API supplied UnicodeChar.
    let c = unsafe { key.uChar.UnicodeChar };
    Ok(match key.wVirtualKeyCode {
        0x26 => Some(b'k'),
        0x28 => Some(b'j'),
        _ => u8::try_from(c).ok(),
    })
}

pub(super) struct Capture(OwnedHandle);
pub(super) fn capture() -> io::Result<Capture> {
    // SAFETY: null security and reserved arguments request the default console buffer.
    let h = unsafe {
        CreateConsoleScreenBuffer(
            GENERIC_READ | GENERIC_WRITE,
            3,
            std::ptr::null(),
            CONSOLE_TEXTMODE_BUFFER,
            std::ptr::null(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the newly created handle now has exactly one owner.
    let buffer = Capture(unsafe { OwnedHandle::from_raw_handle(h) });
    // SAFETY: the new console buffer stays alive in buffer.
    unsafe {
        ok(SetConsoleMode(
            h,
            mode(h)? | ENABLE_VIRTUAL_TERMINAL_PROCESSING | ENABLE_PROCESSED_OUTPUT,
        ))?;
    }
    Ok(buffer)
}
pub(super) fn enter(cap: &Capture) -> io::Result<()> {
    let h = cap.0.as_raw_handle();
    // SAFETY: cap owns the buffer until leave.
    unsafe {
        ok(SetConsoleActiveScreenBuffer(h))?;
    }
    ACTIVE.store(h, Ordering::Release);
    Ok(())
}
pub(super) fn draw(bytes: &[u8]) -> io::Result<()> {
    let text: Vec<u16> = std::str::from_utf8(bytes)
        .map_err(io::Error::other)?
        .encode_utf16()
        .collect();
    let mut written = 0;
    // SAFETY: text holds the given number of UTF-16 units and the view holds the capture lock.
    unsafe {
        ok(WriteConsoleW(
            ACTIVE.load(Ordering::Acquire),
            text.as_ptr().cast(),
            text.len() as u32,
            &mut written,
            std::ptr::null(),
        ))
    }
}
pub(super) fn leave(_cap: Capture, _: bool) -> io::Result<()> {
    ACTIVE.store(std::ptr::null_mut(), Ordering::Release);
    // SAFETY: the original output handle stayed open throughout the view.
    unsafe { ok(SetConsoleActiveScreenBuffer(output())) }
}
pub(super) fn restore(cap: Option<Capture>, utf8: bool) -> io::Result<()> {
    let result = cap.map_or(Ok(()), |cap| leave(cap, utf8));
    restore_raw();
    result
}
pub(super) fn restore_raw() {
    GONE.store(true, Ordering::Release);
    // SAFETY: only the console mode changed by arm and the active screen are restored.
    unsafe {
        if !ACTIVE
            .swap(std::ptr::null_mut(), Ordering::AcqRel)
            .is_null()
        {
            SetConsoleActiveScreenBuffer(output());
        }
        if let Some(&m) = INPUT_MODE.get() {
            SetConsoleMode(input(), m);
        }
    }
}
pub(super) fn restore_on_panic(_: bool) {
    restore_raw();
}
