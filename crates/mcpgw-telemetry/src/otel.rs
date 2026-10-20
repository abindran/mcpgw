//! OpenTelemetry export over OTLP (M3-11): traces, metrics and logs.
//!
//! Configured by the standard variables, which win over the config file:
//! `OTEL_EXPORTER_OTLP_ENDPOINT` (e.g. `http://127.0.0.1:4318`; signal
//! paths `/v1/traces`, `/v1/metrics`, `/v1/logs` are appended),
//! `OTEL_EXPORTER_OTLP_PROTOCOL` (`http/protobuf`, the default, or
//! `http/json`), `OTEL_SERVICE_NAME` (default `mcpgw`). The SDK's own
//! variables also apply (`OTEL_BSP_SCHEDULE_DELAY`,
//! `OTEL_METRIC_EXPORT_INTERVAL`, …).
//!
//! What's exported, from the process's `tracing` data, filtered by
//! `MCPGW_OTEL_FILTER` (default `info`), independent of the stderr filter:
//!
//! - **traces:** the `request` span of each client call (named after the
//!   method and tool, kind server; its parent is the client's
//!   `traceparent` when it sends one), and a `backend …` span (kind client)
//!   for the call to the backend, whose `traceparent` goes to the backend in
//!   `_meta`;
//! - **metrics:** `tracing` events with `monotonic_counter.*`, `counter.*`
//!   or `histogram.*` fields (tool calls, call and start durations);
//! - **logs:** `tracing` events.

use std::collections::HashMap;

use opentelemetry::{
    KeyValue,
    propagation::TextMapPropagator,
    trace::{TraceContextExt, TracerProvider as _},
};
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    Resource, logs::SdkLoggerProvider, metrics::SdkMeterProvider,
    propagation::TraceContextPropagator, trace::SdkTracerProvider,
};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Where and how to export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpConfig {
    /// Base URL, e.g. `http://127.0.0.1:4318`.
    pub endpoint: String,
    /// `http/protobuf` or `http/json`.
    pub json: bool,
    /// `service.name`.
    pub service: String,
}

impl OtlpConfig {
    /// From the `OTEL_*` variables, else `configured` (the config file's
    /// endpoint). `None` when neither names an endpoint.
    pub fn from_env(configured: Option<&str>) -> Option<OtlpConfig> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let endpoint =
            var("OTEL_EXPORTER_OTLP_ENDPOINT").or_else(|| configured.map(str::to_string))?;
        Some(OtlpConfig {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            json: var("OTEL_EXPORTER_OTLP_PROTOCOL").is_some_and(|p| p == "http/json"),
            service: var("OTEL_SERVICE_NAME").unwrap_or_else(|| "mcpgw".into()),
        })
    }
}

/// Keeps the exporters alive; flushes and shuts them down when dropped.
pub struct OtelGuard {
    traces: SdkTracerProvider,
    metrics: SdkMeterProvider,
    logs: SdkLoggerProvider,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        let _ = self.traces.force_flush();
        let _ = self.traces.shutdown();
        let _ = self.metrics.force_flush();
        let _ = self.metrics.shutdown();
        let _ = self.logs.force_flush();
        let _ = self.logs.shutdown();
    }
}

/// The OpenTelemetry layers to add to the subscriber.
pub(crate) type Layers =
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>;

/// Build the exporters and the layers that feed them.
pub(crate) fn layers(cfg: &OtlpConfig) -> Result<(Layers, OtelGuard), String> {
    use tracing_subscriber::Layer;
    let protocol = if cfg.json {
        Protocol::HttpJson
    } else {
        Protocol::HttpBinary
    };
    let url = |signal: &str| format!("{}/v1/{signal}", cfg.endpoint);
    let resource = Resource::builder()
        .with_service_name(cfg.service.clone())
        .with_attributes([KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))])
        .build();
    let err = |what: &str, e: &dyn std::fmt::Display| format!("OTLP {what} exporter: {e}");
    // a collector on this machine is reached directly, never through a
    // configured HTTP proxy
    let client = || {
        let local = [
            "http://127.0.0.1",
            "http://localhost",
            "http://[::1]",
            "https://127.0.0.1",
            "https://localhost",
            "https://[::1]",
        ]
        .iter()
        .any(|p| cfg.endpoint.starts_with(p));
        let mut b =
            reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(10));
        if local {
            b = b.no_proxy();
        }
        b.build()
    };
    let client = client().map_err(|e| err("HTTP client for the", &e))?;

    let spans = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(protocol)
        .with_endpoint(url("traces"))
        .build()
        .map_err(|e| err("trace", &e))?;
    let traces = SdkTracerProvider::builder()
        .with_batch_exporter(spans)
        .with_resource(resource.clone())
        .build();
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(protocol)
        .with_endpoint(url("metrics"))
        .build()
        .map_err(|e| err("metric", &e))?;
    let metrics = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(resource.clone())
        .build();
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_http_client(client)
        .with_protocol(protocol)
        .with_endpoint(url("logs"))
        .build()
        .map_err(|e| err("log", &e))?;
    let logs = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource)
        .build();
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let filter = || {
        let f = std::env::var("MCPGW_OTEL_FILTER")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "info".into());
        tracing_subscriber::EnvFilter::try_new(f)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };
    // the exporters' own HTTP client logs must not loop back into them
    let quiet = |f: tracing_subscriber::EnvFilter| {
        [
            "reqwest",
            "hyper",
            "hyper_util",
            "h2",
            "opentelemetry",
            "opentelemetry_sdk",
            "opentelemetry_otlp",
        ]
        .iter()
        .fold(f, |f, t| {
            f.add_directive(format!("{t}=off").parse().expect("directive"))
        })
    };
    let layers = tracing_opentelemetry::layer()
        .with_tracer(traces.tracer("mcpgw"))
        .with_filter(quiet(filter()))
        .and_then(
            tracing_opentelemetry::MetricsLayer::new(metrics.clone()).with_filter(quiet(filter())),
        )
        .and_then(
            opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&logs)
                .with_filter(quiet(filter())),
        )
        .boxed();
    Ok((
        layers,
        OtelGuard {
            traces,
            metrics,
            logs,
        },
    ))
}

/// Make `span`'s parent the remote span in `traceparent` (W3C Trace
/// Context), e.g. a client's. Invalid values are ignored.
pub fn set_parent(span: &tracing::Span, traceparent: &str, tracestate: Option<&str>) {
    let mut carrier = HashMap::new();
    carrier.insert("traceparent".to_string(), traceparent.to_string());
    if let Some(s) = tracestate {
        carrier.insert("tracestate".to_string(), s.to_string());
    }
    let cx = TraceContextPropagator::new().extract(&carrier);
    if cx.span().span_context().is_valid() {
        let _ = span.set_parent(cx);
    }
}

/// The current span as a W3C `traceparent`, if it is being traced.
pub fn traceparent() -> Option<String> {
    let cx = tracing::Span::current().context();
    let sc = cx.span().span_context().clone();
    sc.is_valid().then(|| {
        format!(
            "00-{}-{}-{:02x}",
            sc.trace_id(),
            sc.span_id(),
            sc.trace_flags().to_u8()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn continues_a_remote_trace_and_propagates_it() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let sub = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("t")));
        let client = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let backend_tp = tracing::subscriber::with_default(sub, || {
            let req = tracing::info_span!(
                "request",
                otel.name = "tools/call fx__echo",
                otel.kind = "server"
            );
            set_parent(&req, client, None);
            let _e = req.enter();
            let backend = tracing::info_span!("backend", otel.kind = "client");
            let _b = backend.enter();
            traceparent()
        });
        let tp = backend_tp.expect("traced");
        assert!(
            tp.starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-"),
            "{tp}"
        );
        assert!(!tp.contains("00f067aa0ba902b7"), "a new span id: {tp}");
        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans.len(), 2);
        let req = spans
            .iter()
            .find(|s| s.name == "tools/call fx__echo")
            .unwrap();
        assert_eq!(
            req.span_context.trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(req.parent_span_id.to_string(), "00f067aa0ba902b7");
        assert!(
            tp.contains(
                &spans
                    .iter()
                    .find(|s| s.name == "backend")
                    .unwrap()
                    .span_context
                    .span_id()
                    .to_string()
            )
        );
        // garbage parents are ignored
        set_parent(&tracing::Span::none(), "nope", None);
    }

    #[test]
    fn config_from_env_or_file() {
        // (the OTEL_* variables are unset in tests)
        assert!(
            OtlpConfig::from_env(None).is_none()
                || std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok()
        );
        if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_err() {
            let c = OtlpConfig::from_env(Some("http://127.0.0.1:4318/")).unwrap();
            assert_eq!(
                (c.endpoint.as_str(), c.json, c.service.as_str()),
                ("http://127.0.0.1:4318", false, "mcpgw")
            );
        }
    }
}
