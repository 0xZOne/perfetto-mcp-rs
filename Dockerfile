# syntax=docker/dockerfile:1.7
#
# Build the perfetto-mcp-rs MCP server in a multi-stage container.
#
# Glama and similar registries spin this up to verify the server starts and
# responds to MCP introspection (`tools/list`). No trace file is required for
# that check — `load_trace` fetches `trace_processor_shell` from the Perfetto
# LUCI bucket on demand, which only matters once a real trace is loaded.

FROM rust:1-slim-bookworm AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      protobuf-compiler \
      ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 1000 mcp

COPY --from=builder /build/target/release/perfetto-mcp-rs /usr/local/bin/perfetto-mcp-rs

USER mcp
WORKDIR /home/mcp

ENTRYPOINT ["/usr/local/bin/perfetto-mcp-rs"]
