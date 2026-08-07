# geocode-mcp

A Model Context Protocol (MCP) server that converts between place names and
geographic coordinates. Built in Rust on the shared `mcp-core` crate, it
exposes two tools over stdio.

All lookups go through the free [Photon](https://photon.komoot.io/) API
(powered by OpenStreetMap) -- no API key or configuration required.

## Tools

| Tool | Description |
|---|---|
| `geocode` | Resolve a location name to coordinates (latitude, longitude, country, region, place type). |
| `reverse_geocode` | Resolve a coordinate pair to the nearest place (name, country, region, place type). |

## Usage

```bash
geocode-mcp serve --mode stdio
```

The server reads JSON-RPC messages from stdin and writes responses to
stdout.

### Claude Desktop configuration

Add to your Claude Desktop MCP config:

```json
{
  "mcpServers": {
    "geocode": {
      "command": "/path/to/geocode-mcp",
      "args": ["serve", "--mode", "stdio"]
    }
  }
}
```

## Logging

`mcp-core`'s `run` installs the process subscriber; this crate calls nothing
to get it. Logs go to stderr, never stdout -- the stdio transport frames
JSON-RPC on stdout, and one log line there would corrupt the protocol
stream. `RUST_LOG` sets the level (default `info`); see `mcp-core`'s own
README for the full level contract, the request/tool-call spans, and the
standard `OTEL_*` environment variables.

What this server adds on top of what it inherits:

- A `debug!` line each time it calls out to Photon, for `geocode` and for
  `reverse_geocode`. A place name and a coordinate pair are tool arguments,
  so both stay at DEBUG and are never attached to a span; `RUST_LOG=debug`
  is what it takes to see them.
- `geocode.upstream_failures`, a counter labelled `tool` and `reason`
  (`http_error` or `network`), for a fault reaching outward. A "no results"
  answer or bad caller input is a decline, not a fault, and is not counted
  here.
- `mcp-core` already records a tool-call counter and a latency histogram by
  tool and outcome (`mcp.tools.call`, `mcp.tools.call.duration`); this
  server does not duplicate them.

An address and a coordinate are among the most sensitive values this fleet
handles -- they say where a person lives, works, or is right now. Neither
ever reaches an INFO line or a span field, at any level, with or without a
collector attached.

### The `otel` feature

Off by default. A pure passthrough -- `geocode-mcp -> mcp-core ->
adelie-telemetry` -- so this crate takes no direct dependency on
`adelie-telemetry` or on any opentelemetry crate. With the feature off,
`cargo tree` resolves no opentelemetry crate at all.

```bash
cargo build --features otel
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 ./target/debug/geocode-mcp serve --mode stdio
```

## Testing

```bash
just check                       # default features: fmt, lint, build, test
just check-otel                  # the same, built with --features otel
```

Network-dependent tests are gated behind `RUN_NETWORK_TESTS=1` so the
default suite is deterministic and offline. The `tests/telemetry_*.rs`
files are the telemetry acceptance suite: that stdout carries only
JSON-RPC at `RUST_LOG=trace`, that no place name or coordinate reaches an
INFO line or a span field for any tool this server advertises, on both the
success path and an upstream failure, and that a default build resolves no
opentelemetry crate.

`tests/support/mod.rs` holds one sentinel probe per tool. Each probe
carries valid arguments and points `GeocodeService` at a local mock server
instead of the live Photon API, so `tests/telemetry_span_fields.rs` reaches
the real outbound-request code, not just parameter validation.
`tests/telemetry_stdio.rs` spawns the real binary as a separate process,
which cannot be redirected to that mock, so it uses deliberately invalid
arguments instead -- sufficient for what it proves (process-level
stdout/stderr hygiene under the real, installed subscriber).

## License

Apache-2.0.
