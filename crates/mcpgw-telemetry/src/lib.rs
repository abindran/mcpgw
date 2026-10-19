//! `mcpgw-telemetry` — tracing, metrics and the audit log writer.
//!
//! [`init`] installs the process-wide subscriber (`pretty`, `compact` or
//! `json` lines on stderr, filtered by `MCPGW_LOG`). [`instrument_request`]
//! wraps one client request in a `request` span carrying `request_id`,
//! `method` and `tool`, and emits exactly one `request` event when it ends
//! with `duration_ms` and `outcome`.

use std::{
    fmt::Display,
    future::Future,
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use tracing::{Instrument, Subscriber};
use tracing_subscriber::{EnvFilter, fmt::MakeWriter};

/// Environment variable holding the log filter (`warn`, `mcpgw=debug`, ...).
pub const LOG_ENV: &str = "MCPGW_LOG";
/// Environment variable selecting the log format.
pub const LOG_FORMAT_ENV: &str = "MCPGW_LOG_FORMAT";

/// Log line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Multi-line, human-oriented.
    Pretty,
    /// One line per event, human-oriented (the default).
    #[default]
    Compact,
    /// One JSON object per event, with the current span's fields.
    Json,
}

impl std::str::FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "pretty" => Ok(Self::Pretty),
            "compact" | "text" => Ok(Self::Compact),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "unknown log format {other:?}; use pretty, compact or json"
            )),
        }
    }
}

/// Build a subscriber writing `format` lines to `writer`, filtered by
/// `filter` (an `EnvFilter` directive string), without colours.
pub fn subscriber<W>(
    format: LogFormat,
    filter: &str,
    writer: W,
) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    build(format, filter, writer, false)
}

fn build<W>(
    format: LogFormat,
    filter: &str,
    writer: W,
    ansi: bool,
) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("warn"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(ansi && format != LogFormat::Json)
        .with_target(true);
    match format {
        LogFormat::Pretty => Box::new(builder.pretty().finish()),
        LogFormat::Compact => Box::new(builder.compact().finish()),
        LogFormat::Json => Box::new(
            builder
                .json()
                .with_current_span(true)
                .with_span_list(false)
                .flatten_event(true)
                .finish(),
        ),
    }
}

/// Install the global subscriber on stderr. The filter comes from
/// `MCPGW_LOG` (default `default_filter`); the format from `format`, else
/// `MCPGW_LOG_FORMAT`, else compact. Returns an error if the format name is
/// invalid; installing twice is a no-op.
pub fn init(format: Option<LogFormat>, default_filter: &str) -> Result<(), String> {
    let format = match format {
        Some(f) => f,
        None => match std::env::var(LOG_FORMAT_ENV) {
            Ok(v) if !v.is_empty() => v.parse()?,
            _ => LogFormat::default(),
        },
    };
    let filter = std::env::var(LOG_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default_filter.to_string());
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let _ = tracing::subscriber::set_global_default(build(format, &filter, std::io::stderr, ansi));
    Ok(())
}

/// A new request id: 16 lowercase hex characters, unique within the process
/// and random across processes.
pub fn request_id() -> String {
    static PREFIX: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let prefix = *PREFIX.get_or_init(|| {
        let mut b = [0u8; 4];
        let _ = getrandom::fill(&mut b);
        u32::from_le_bytes(b)
    });
    let n = COUNTER.fetch_add(1, Ordering::Relaxed) as u32;
    format!("{prefix:08x}{n:08x}")
}

/// Run one client request inside a `request` span and log its completion.
///
/// The span has `request_id`, `method` (`tools/call`, ...) and `tool` (empty
/// when not applicable); the closing event adds `duration_ms` and `outcome`
/// (`ok` / `error`, with `error` set).
pub async fn instrument_request<T, E, F>(
    method: &'static str,
    tool: Option<&str>,
    fut: F,
) -> Result<T, E>
where
    E: Display,
    F: Future<Output = Result<T, E>>,
{
    let id = request_id();
    let span = tracing::info_span!(
        "request",
        request_id = %id,
        method,
        tool = tool.unwrap_or(""),
    );
    let start = Instant::now();
    let out = fut.instrument(span.clone()).await;
    let duration_ms = (start.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
    span.in_scope(|| match &out {
        Ok(_) => tracing::info!(duration_ms, outcome = "ok", "request"),
        Err(e) => tracing::warn!(duration_ms, outcome = "error", error = %e, "request"),
    });
    out
}

/// A `MakeWriter` that appends to a shared buffer (tests and diagnostics).
#[derive(Clone, Default)]
pub struct BufferWriter(pub std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl BufferWriter {
    /// Everything written so far, as text.
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("buffer")).into_owned()
    }
}

impl Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buffer").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = BufferWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_hex() {
        let a = request_id();
        let b = request_id();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(a[..8], b[..8]);
    }

    #[test]
    fn formats_parse() {
        assert_eq!("JSON".parse::<LogFormat>(), Ok(LogFormat::Json));
        assert_eq!("pretty".parse::<LogFormat>(), Ok(LogFormat::Pretty));
        assert!("xml".parse::<LogFormat>().is_err());
    }

    #[tokio::test]
    async fn one_json_line_per_request_with_method_tool_duration() {
        let buf = BufferWriter::default();
        let _g = tracing::subscriber::set_default(subscriber(LogFormat::Json, "info", buf.clone()));
        let ok: Result<u8, String> = instrument_request("tools/call", Some("fs__read"), async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            Ok(1)
        })
        .await;
        assert_eq!(ok, Ok(1));
        let err: Result<(), String> =
            instrument_request("tools/list", None, async { Err("boom".to_string()) }).await;
        assert!(err.is_err());

        let lines: Vec<serde_json::Value> = buf
            .contents()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        let call = &lines[0];
        assert_eq!(call["message"], "request");
        assert_eq!(call["span"]["method"], "tools/call");
        assert_eq!(call["span"]["tool"], "fs__read");
        assert_eq!(call["span"]["request_id"].as_str().unwrap().len(), 16);
        assert_eq!(call["outcome"], "ok");
        assert!(call["duration_ms"].as_f64().unwrap() >= 5.0, "{call}");
        assert_eq!(lines[1]["outcome"], "error");
        assert_eq!(lines[1]["error"], "boom");
        assert_eq!(lines[1]["level"], "WARN");
    }

    #[tokio::test]
    async fn compact_format_is_human_readable() {
        let buf = BufferWriter::default();
        let _g =
            tracing::subscriber::set_default(subscriber(LogFormat::Compact, "info", buf.clone()));
        let _: Result<(), String> =
            instrument_request("tools/call", Some("t"), async { Ok(()) }).await;
        let out = buf.contents();
        assert!(
            out.contains("INFO request: mcpgw_telemetry: request duration_ms="),
            "{out}"
        );
        assert!(out.contains("method=\"tools/call\" tool=\"t\""), "{out}");
        assert!(
            !out.contains('\u{1b}'),
            "no colour codes off a terminal: {out:?}"
        );
    }
}
