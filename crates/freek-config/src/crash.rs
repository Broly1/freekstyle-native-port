//! Crash reports. Release builds on Windows have no console, so a panic
//! used to end the program (or freeze its window, from another thread)
//! with no message at all. With [`install`], a panic anywhere is added to
//! `crash.log` in the data folder, shown in a message box on Windows, and
//! the program ends.

use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::Path;

/// The log is started again past this size, so it never grows unbounded.
const MAX_LOG: u64 = 1 << 20;

/// Report panics for `program` ("Freekstyle", "Freekstyle launcher").
pub fn install(program: &'static str) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The usual message on stderr first, for a terminal or a log.
        default(info);
        let report = report(program, info);
        let path = crate::data_dir().join("crash.log");
        let saved = save(&path, &report).is_ok();
        show(program, info, saved.then_some(path.as_path()));
        // A thread that died would leave the rest running, the game's
        // window frozen: end it all, with Rust's panic exit code.
        std::process::exit(101);
    }));
}

fn message(info: &PanicHookInfo) -> String {
    let payload = info.payload();
    let text = payload.downcast_ref::<&str>().copied().or_else(|| payload.downcast_ref::<String>().map(String::as_str)).unwrap_or("(no message)");
    match info.location() {
        Some(l) => format!("{text} ({}:{})", l.file(), l.line()),
        None => text.to_string(),
    }
}

fn report(program: &str, info: &PanicHookInfo) -> String {
    let thread = std::thread::current();
    format!(
        "==== {} UTC: {program} {} crashed\nthread: {}\n{}\n\n{}\n",
        utc_now(),
        env!("CARGO_PKG_VERSION"),
        thread.name().unwrap_or("unnamed"),
        message(info),
        std::backtrace::Backtrace::force_capture()
    )
}

fn save(path: &Path, report: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let append = std::fs::metadata(path).is_ok_and(|m| m.len() < MAX_LOG);
    let mut f = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(path)?;
    f.write_all(report.as_bytes())
}

#[cfg(windows)]
fn show(program: &str, info: &PanicHookInfo, log: Option<&Path>) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: *mut core::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    const MB_ICONERROR: u32 = 0x10;
    const MB_TOPMOST: u32 = 0x4_0000;
    let mut text = format!("{program} crashed:\n\n{}", message(info));
    if let Some(log) = log {
        text += &format!("\n\nThe details are in\n{}\nPlease send that file along when you report this.", log.display());
    }
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, caption) = (wide(&text), wide(program));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call;
    // a null owner window is allowed.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_ICONERROR | MB_TOPMOST) };
}

#[cfg(not(windows))]
fn show(_: &str, _: &PanicHookInfo, log: Option<&Path>) {
    if let Some(log) = log {
        eprintln!("crash report saved to {}", log.display());
    }
}

/// "YYYY-MM-DD HH:MM:SS" in UTC, without a date library.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_starts_again_when_it_gets_big() {
        let path = std::env::temp_dir().join(format!("freek-crash-test-{}.log", std::process::id()));
        save(&path, "one\n").unwrap();
        save(&path, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        std::fs::write(&path, vec![b'x'; MAX_LOG as usize]).unwrap();
        save(&path, "three\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn utc_dates_look_right() {
        let s = utc_now();
        assert_eq!(s.len(), 19);
        assert!(s.starts_with("20"));
    }
}
