use tracing_subscriber::EnvFilter;

/// Writer that fans a record out to both the rotating file and stdout.
struct TeeWriter;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeWriter {
    type Writer = Tee;

    fn make_writer(&'a self) -> Self::Writer {
        Tee
    }
}

/// The rolling file appender, written **synchronously**.
///
/// It used to be wrapped in `tracing_appender::non_blocking`, which hands each
/// record to a background thread through a channel: a hard crash (access
/// violation, no unwinding) loses whatever the worker has not written yet.
/// That is exactly what happened on 2026-11-17 — the log stopped at "sweep
/// adopting stray folder windows" and the real last step stayed unknown, so
/// the crash had to be diagnosed from Event Viewer alone. A few hundred lines
/// per session do not justify that blind spot: `RollingFileAppender` writes
/// straight to the `File`, so every record is on disk before the next
/// statement runs.
static FILE: std::sync::Mutex<Option<tracing_appender::rolling::RollingFileAppender>> =
    std::sync::Mutex::new(None);

struct Tee;

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stdout().write_all(buf);
        if let Ok(mut guard) = FILE.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.write_all(buf);
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(mut guard) = FILE.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

/// Logging init: console + rotating file, compile-time trimmed in release.
/// Must run before everything else (see docs/M0_Tech_Design.md §6).
pub fn init(name: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_appender = tracing_appender::rolling::daily(log_dir(), format!("{name}.log"));
    if let Ok(mut guard) = FILE.lock() {
        *guard = Some(file_appender);
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(TeeWriter)
        .with_ansi(false)
        .init();
}

fn log_dir() -> std::path::PathBuf {
    let base = std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let dir = base.join("revolc").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
