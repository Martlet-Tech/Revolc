use tracing_subscriber::EnvFilter;

/// Writer that fans a record out to both the rotating file and stdout.
struct TeeWriter;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeWriter {
    type Writer = Tee;

    fn make_writer(&'a self) -> Self::Writer {
        Tee {
            file: FILE.get().cloned(),
        }
    }
}

/// Holds the non-blocking file writer handle.
static FILE: std::sync::OnceLock<tracing_appender::non_blocking::NonBlocking> = std::sync::OnceLock::new();

struct Tee {
    file: Option<tracing_appender::non_blocking::NonBlocking>,
}

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use tracing_subscriber::fmt::MakeWriter as _;
        let _ = std::io::stdout().write_all(buf);
        if let Some(f) = &self.file {
            let _ = f.make_writer().write_all(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Logging init: console + rotating file, compile-time trimmed in release.
/// Must run before everything else (see doc/M0_Tech_Design.md §6).
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_appender = tracing_appender::rolling::daily(log_dir(), "revolc.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
    // Leak the guard: logger must live for the whole process lifetime.
    std::mem::forget(guard);
    let _ = FILE.set(file_writer);

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
