#![deny(warnings)]

// In-process proof of D10 for geocode-mcp: whatever `call_geocode` and
// `call_reverse_geocode` do with a caller's place name or coordinates, it
// never becomes a span field, at any level, and it never reaches an event
// at INFO or above.
//
// `tests/telemetry_stdio.rs` proves the same thing against the real,
// installed subscriber; this drives mcp-core's dispatch directly and reads
// back the spans and events it really emitted, the way mcp-core's own
// acceptance suite does for its dispatch path. A span field would not
// necessarily show up on an INFO-level *line* of console text (the fmt
// layer only renders a span's fields on a line when some event fires while
// that span is entered), so this checks span fields directly rather than
// relying on the console rendering to surface one.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use mcp_core::{ServerCore, Session};
use serde_json::{Value, json};
use tracing::Level;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use geocode_mcp::{GeocodeService, server_config};

/// A place name a caller might supply. See `tests/telemetry_stdio.rs` for
/// why it travels in `language` rather than `name`: the call is driven down
/// the validation-failure path (missing `name`) so it never reaches the
/// live Photon API, and `#[instrument(skip(self, args))]` skips the whole
/// `arguments` object, not individual keys within it, so which key carries
/// the sentinel does not change what this test proves.
const SENTINEL_ADDRESS: &str = "MARKER-9f3d1c2a-sentinel-address";

/// A coordinate a caller might supply, carried directly as `longitude` on a
/// request whose `latitude` is omitted.
const SENTINEL_LONGITUDE: f64 = -98.765432;

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

/// AC: no tool-call span field carries a place name or a coordinate, at any
/// level, and no INFO (or higher) event carries either — for both tools, on
/// the failure path (missing the other required field, which never reaches
/// the live Photon API).
#[test]
fn tool_call_leaves_no_address_or_coordinate_in_any_span_field() {
    let longitude_text = SENTINEL_LONGITUDE.to_string();
    let recorded = capture_dispatch(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "geocode", "arguments": {"count": 3, "language": SENTINEL_ADDRESS}},
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "reverse_geocode", "arguments": {"longitude": SENTINEL_LONGITUDE}},
        }),
    ]);

    for span in &recorded.spans {
        for (key, value) in &span.fields {
            assert!(
                !value.contains(SENTINEL_ADDRESS) && !value.contains(&longitude_text),
                "a place name or coordinate reached span {:?} field {key:?}: {value:?}",
                span.name
            );
        }
    }

    for event in &recorded.events {
        if event.level > Level::INFO {
            continue;
        }
        for (key, value) in &event.fields {
            assert!(
                !value.contains(SENTINEL_ADDRESS) && !value.contains(&longitude_text),
                "a place name or coordinate reached a {} line, field {key:?}: {value:?}",
                event.level
            );
        }
    }

    let address_at_debug = recorded.events.iter().any(|event| {
        event.level == Level::DEBUG
            && event
                .fields
                .values()
                .any(|value| value.contains(SENTINEL_ADDRESS))
    });
    assert!(
        address_at_debug,
        "the place name must still be reachable at DEBUG, or this test cannot tell a real fix \
         from a line that was simply deleted; the events were {:?}",
        recorded.event_summary()
    );

    let longitude_at_debug = recorded.events.iter().any(|event| {
        event.level == Level::DEBUG
            && event
                .fields
                .values()
                .any(|value| value.contains(&longitude_text))
    });
    assert!(
        longitude_at_debug,
        "the coordinate must still be reachable at DEBUG, or this test cannot tell a real fix \
         from a line that was simply deleted; the events were {:?}",
        recorded.event_summary()
    );
}

/// AC: the tool handlers are instrumented — a span opens for each, nested
/// under mcp-core's own `mcp.tools.call` span. Without this, the leak test
/// above could pass simply because nothing was instrumented at all.
#[test]
fn each_tool_handler_opens_its_own_span() {
    let recorded = capture_dispatch(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "geocode", "arguments": {"count": 3, "language": SENTINEL_ADDRESS}},
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "reverse_geocode", "arguments": {"longitude": SENTINEL_LONGITUDE}},
        }),
    ]);

    for expected in ["call_geocode", "call_reverse_geocode"] {
        assert!(
            recorded.spans.iter().any(|span| span.name == expected),
            "expected a {expected:?} span; the spans were {:?}",
            recorded
                .spans
                .iter()
                .map(|span| span.name)
                .collect::<Vec<_>>()
        );
    }
}
