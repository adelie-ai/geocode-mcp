#![deny(warnings)]

// In-process proof of D10 for geocode-mcp: whatever a tool handler does with
// a caller's place name or coordinates, it never becomes a span field, at
// any level, and it never reaches an event at INFO or above.
//
// `tests/telemetry_stdio.rs` proves the same thing against the real,
// installed subscriber; this drives mcp-core's dispatch directly and reads
// back the spans and events it really emitted, the way mcp-core's own
// acceptance suite does for its dispatch path. A span field would not
// necessarily show up on an INFO-level *line* of console text (the fmt
// layer only renders a span's fields on a line when some event fires while
// that span is entered), so this checks span fields directly rather than
// relying on the console rendering to surface one.
//
// mcp-core#40 lesson 8: table-driven over the whole tool list, not one
// tool. `support::tool_probes()` is the single table both this file and
// `tests/telemetry_stdio.rs` iterate; `tool_probe_table_covers_every_advertised_tool`
// below fails the moment a tool ships without a matching entry.

mod support;

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use mcp_core::{McpService, ServerCore, Session};
use serde_json::{Value, json};
use tracing::Level;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use geocode_mcp::{GeocodeService, server_config};
use support::tool_probes;

#[derive(Clone, Debug)]
struct RecordedSpan {
    name: &'static str,
    fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct RecordedEvent {
    level: Level,
    fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
struct Recorded {
    spans: Vec<RecordedSpan>,
    events: Vec<RecordedEvent>,
}

impl Recorded {
    fn event_summary(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| format!("{}{:?}", event.level, event.fields))
            .collect()
    }
}

fn capture<F, Fut>(body: F) -> Recorded
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(body());
    });
    capture.take()
}

fn capture_dispatch(messages: &[Value]) -> Recorded {
    let messages = messages.to_vec();
    capture(|| async move {
        let core = ServerCore::new(server_config(), Arc::new(GeocodeService::new()));
        let mut session = Session::new(core);
        for message in messages {
            session.handle_message(message).await;
        }
    })
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Recorded>>);

impl Capture {
    fn take(self) -> Recorded {
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .clone()
    }
}

impl<S> Layer<S> for Capture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = BTreeMap::new();
        attrs.record(&mut Collector(&mut fields));
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .spans
            .push(RecordedSpan {
                name: attrs.metadata().name(),
                fields,
            });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let name = ctx.span(id).map_or("<closed>", |span| span.name());
        let mut fields = BTreeMap::new();
        values.record(&mut Collector(&mut fields));
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .spans
            .push(RecordedSpan { name, fields });
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = BTreeMap::new();
        event.record(&mut Collector(&mut fields));
        self.0
            .lock()
            .expect("the capture lock is only held to push one record")
            .events
            .push(RecordedEvent {
                level: *event.metadata().level(),
                fields,
            });
    }
}

struct Collector<'a>(&'a mut BTreeMap<String, String>);

impl Visit for Collector<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

/// AC (mcp-core#40 lesson 8): every tool this server advertises has a
/// sentinel probe in `support::tool_probes()`. A tool added without one is
/// silently unaudited by the two tests below, so this fails first and names
/// the gap instead.
#[test]
fn tool_probe_table_covers_every_advertised_tool() {
    let advertised: Vec<String> = GeocodeService::new()
        .tools()
        .into_iter()
        .map(|t| t.name)
        .collect();
    let probed: Vec<&str> = tool_probes().iter().map(|p| p.tool).collect();

    for name in &advertised {
        assert!(
            probed.contains(&name.as_str()),
            "tool {name:?} is advertised by tools() but has no entry in \
             support::tool_probes() -- add one so its arguments are covered by the \
             leak-detection tests"
        );
    }
    for name in &probed {
        assert!(
            advertised.iter().any(|a| a == name),
            "support::tool_probes() lists {name:?}, which tools() does not advertise -- \
             remove or fix the stale entry"
        );
    }
}

/// AC: no tool-call span field carries a place name or a coordinate, at any
/// level, and no INFO (or higher) event carries either -- for every tool in
/// `support::tool_probes()`, on the failure path (missing the other
/// required field, which never reaches the live Photon API).
#[test]
fn tool_call_leaves_no_probe_sentinel_in_any_span_field() {
    let probes = tool_probes();
    let requests: Vec<Value> =
        std::iter::once(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .chain(probes.iter().enumerate().map(|(i, probe)| {
                json!({
                    "jsonrpc": "2.0",
                    "id": i + 2,
                    "method": "tools/call",
                    "params": {"name": probe.tool, "arguments": probe.arguments},
                })
            }))
            .collect();

    let recorded = capture_dispatch(&requests);

    for span in &recorded.spans {
        for (key, value) in &span.fields {
            for probe in &probes {
                for sentinel in &probe.sentinels {
                    assert!(
                        !value.contains(sentinel.as_str()),
                        "{}'s sentinel reached span {:?} field {key:?}: {value:?}",
                        probe.tool,
                        span.name
                    );
                }
            }
        }
    }

    for event in &recorded.events {
        if event.level > Level::INFO {
            continue;
        }
        for (key, value) in &event.fields {
            for probe in &probes {
                for sentinel in &probe.sentinels {
                    assert!(
                        !value.contains(sentinel.as_str()),
                        "{}'s sentinel reached a {} line, field {key:?}: {value:?}",
                        probe.tool,
                        event.level
                    );
                }
            }
        }
    }

    for probe in &probes {
        for sentinel in &probe.sentinels {
            let at_debug = recorded.events.iter().any(|event| {
                event.level == Level::DEBUG
                    && event
                        .fields
                        .values()
                        .any(|value| value.contains(sentinel.as_str()))
            });
            assert!(
                at_debug,
                "{}'s sentinel {sentinel:?} must still be reachable at DEBUG, or this test \
                 cannot tell a real fix from a line that was simply deleted; the events were \
                 {:?}",
                probe.tool,
                recorded.event_summary()
            );
        }
    }
}

/// AC: every tool handler is instrumented -- a span opens for each, nested
/// under mcp-core's own `mcp.tools.call` span. Without this, the leak test
/// above could pass simply because nothing was instrumented at all.
#[test]
fn each_probed_tool_handler_opens_its_own_span() {
    let probes = tool_probes();
    let requests: Vec<Value> =
        std::iter::once(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .chain(probes.iter().enumerate().map(|(i, probe)| {
                json!({
                    "jsonrpc": "2.0",
                    "id": i + 2,
                    "method": "tools/call",
                    "params": {"name": probe.tool, "arguments": probe.arguments},
                })
            }))
            .collect();

    let recorded = capture_dispatch(&requests);

    for probe in &probes {
        assert!(
            recorded
                .spans
                .iter()
                .any(|span| span.name == probe.handler_span),
            "expected a {:?} span for tool {:?}; the spans were {:?}",
            probe.handler_span,
            probe.tool,
            recorded
                .spans
                .iter()
                .map(|span| span.name)
                .collect::<Vec<_>>()
        );
    }
}
