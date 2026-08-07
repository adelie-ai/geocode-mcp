#![deny(warnings)]

// In-process proof of D10 for geocode-mcp: whatever a tool handler does with
// a caller's place name or coordinates, it never becomes a span field, at
// any level, and it never reaches an event at INFO or above.
//
// `tests/telemetry_stdio.rs` proves the complementary thing against the
// real, installed subscriber: what reaches rendered console *text*. Neither
// alone is enough -- lesson 7 (mcp-core#40) is that a console-text test
// cannot see a span-field leak, because a span records its fields at
// creation and nothing prints them unless an event fires inside that span.
//
// Every test below drives its call through `support::mock_service`, so it
// reaches the *live* body of `geocode_location_with_base` /
// `reverse_geocode_location_with_base` -- not just geocode-mcp's own
// parameter validation. An earlier version of this file used
// validation-rejected requests (a required field omitted) to stay off the
// network; review found that this left both outbound-request `debug!`
// sites, and the response-parsing branches, completely unexercised by the
// leak-detection tests -- a leak added inside either body was uncaught by
// the full suite. Pointing the service at a local mock instead reaches the
// real code while still touching no live service (mcp-core#40 lesson 9).

mod support;

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use mcp_core::telemetry::metrics::{self, Label};
use mcp_core::{McpService, ServerCore, Session};
use serde_json::json;
use tracing::Level;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use geocode_mcp::{GeocodeService, server_config};
use support::{ToolProbe, tool_probes};

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

/// Drive one `tools/call` for `probe` against a `GeocodeService` pointed at
/// a mock server that answers `probe.mock_path` with `status`/`body`, and
/// capture what the dispatch emitted. The mock server setup happens inside
/// the same Tokio runtime `capture` builds, since `httpmock`'s async API
/// requires one.
fn capture_probe_call(probe: &ToolProbe, status: u16, body: &'static str) -> Recorded {
    let mock_path = probe.mock_path;
    let tool = probe.tool;
    let arguments = probe.arguments.clone();

    capture(|| async move {
        let (_server, service) = support::mock_service(mock_path, status, body).await;
        let core = ServerCore::new(server_config(), Arc::new(service));
        let mut session = Session::new(core);
        session
            .handle_message(
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            )
            .await;
        session
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments},
            }))
            .await;
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

/// No span field and no INFO-or-louder event field may contain any of
/// `probe`'s sentinels.
fn assert_no_leak(recorded: &Recorded, probe: &ToolProbe, scenario: &str) {
    for span in &recorded.spans {
        for (key, value) in &span.fields {
            for sentinel in &probe.sentinels {
                assert!(
                    !value.contains(sentinel.as_str()),
                    "[{scenario}] {}'s sentinel reached span {:?} field {key:?}: {value:?}",
                    probe.tool,
                    span.name
                );
            }
        }
    }
    for event in &recorded.events {
        if event.level > Level::INFO {
            continue;
        }
        for (key, value) in &event.fields {
            for sentinel in &probe.sentinels {
                assert!(
                    !value.contains(sentinel.as_str()),
                    "[{scenario}] {}'s sentinel reached a {} line, field {key:?}: {value:?}",
                    probe.tool,
                    event.level
                );
            }
        }
    }
}

/// Every one of `probe`'s sentinels must still be reachable at DEBUG, or the
/// no-leak assertion above cannot tell a real fix from a line that was
/// simply deleted.
fn assert_debug_positive_control(recorded: &Recorded, probe: &ToolProbe, scenario: &str) {
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
            "[{scenario}] {}'s sentinel {sentinel:?} must still be reachable at DEBUG, or the \
             no-leak assertion cannot tell a real fix from a line that was simply deleted; the \
             events were {:?}",
            probe.tool,
            recorded.event_summary()
        );
    }
}

/// AC (mcp-core#40 lesson 8): every tool this server advertises has a
/// sentinel probe in `support::tool_probes()`. A tool added without one is
/// silently unaudited by the tests below, so this fails first and names the
/// gap.
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

/// AC: every tool handler is instrumented -- a span opens for each on a
/// real, successful call, nested under mcp-core's own `mcp.tools.call`
/// span. Without this, the leak tests below could pass simply because
/// nothing was instrumented at all.
#[test]
fn each_probed_tool_handler_opens_its_own_span() {
    for probe in tool_probes() {
        let recorded = capture_probe_call(&probe, 200, support::SUCCESS_BODY);
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

/// AC (mcp-core#40 lesson 9): the success path through the real outbound
/// call -- not a validation-rejection shortcut -- leaves no probe sentinel
/// in any span field or INFO-or-louder event, for every tool. This is what
/// actually reaches `geocode_location_with_base` /
/// `reverse_geocode_location_with_base` and both outbound-request `debug!`
/// sites; a leak added inside either body is only caught here.
#[test]
fn tool_call_success_leaves_no_probe_sentinel_in_any_span_field() {
    for probe in tool_probes() {
        let recorded = capture_probe_call(&probe, 200, support::SUCCESS_BODY);
        assert_no_leak(&recorded, &probe, "success");
        assert_debug_positive_control(&recorded, &probe, "success");
    }
}

/// AC (mcp-core#40 lesson 9): the failure branch through the real outbound
/// call also leaves no probe sentinel above DEBUG, for every tool, and
/// classifies correctly against `geocode.upstream_failures`.
///
/// Two upstream shapes:
/// - No results (`{"features":[]}`): `GeocodeError::LocationNotFound`'s
///   `Display` embeds the caller's own place name or coordinate -- the
///   scenario a real upstream response most naturally carries content in.
///   Rule 8.2 makes this a decline, not a fault, so it must not move the
///   counter.
/// - An HTTP error status (429): classified as a fault
///   (`upstream_failure_reason` -> `"http_error"`), so it must increment
///   `geocode.upstream_failures`, labelled by tool and reason -- proven
///   here against a real mocked response, not the synthetically
///   constructed errors `src/service.rs`'s own unit tests use.
#[test]
fn tool_call_failure_branches_leave_no_probe_sentinel_and_classify_correctly() {
    let _guard = lock_metrics();

    for probe in tool_probes() {
        let labels = [
            Label::new("tool", probe.tool),
            Label::new("reason", "http_error"),
        ];

        // No results: a decline. Must not move the counter.
        let before_decline = counter_total("geocode.upstream_failures", &labels);
        let recorded = capture_probe_call(&probe, 200, support::NO_RESULTS_BODY);
        assert_no_leak(&recorded, &probe, "no-results");
        assert_debug_positive_control(&recorded, &probe, "no-results");
        assert_eq!(
            counter_total("geocode.upstream_failures", &labels),
            before_decline,
            "[no-results] a 'no results' decline must not increment geocode.upstream_failures \
             for {:?}",
            probe.tool
        );

        // HTTP error: a fault. Must increment the counter.
        let before_fault = counter_total("geocode.upstream_failures", &labels);
        let recorded = capture_probe_call(&probe, 429, "");
        assert_no_leak(&recorded, &probe, "http-error");
        assert_eq!(
            counter_total("geocode.upstream_failures", &labels),
            before_fault + 1,
            "[http-error] an upstream HTTP error must increment geocode.upstream_failures \
             labelled tool={:?} reason=\"http_error\"",
            probe.tool
        );
    }
}

/// The metrics registry [`mcp_core::telemetry::metrics`] records into is
/// process-global, and `cargo test` runs a file's tests concurrently by
/// default. This guards the test above that reads it.
static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_metrics() -> std::sync::MutexGuard<'static, ()> {
    METRICS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn counter_total(name: &str, labels: &[Label]) -> u64 {
    metrics::global()
        .snapshot()
        .counters
        .iter()
        .find(|counter| counter.name == name && same_labels(&counter.labels, labels))
        .map_or(0, |counter| counter.total)
}

fn same_labels(recorded: &[Label], wanted: &[Label]) -> bool {
    recorded.len() == wanted.len()
        && wanted.iter().all(|want| {
            recorded
                .iter()
                .any(|have| have.key() == want.key() && have.value() == want.value())
        })
}
