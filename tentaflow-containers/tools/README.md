# Tools

Function calling, MCP servers, and external integrations.

## Status

This category does not expose any user-facing tool manifests yet, but it now
contains shared deployment assets such as the self-hosted iroh relay stack.

## Structure

- `_services/*.toml` - declarative tool manifests used by the catalog and GUI
- `docker/<engine>/` - Docker-based tool runtimes

## How to add a tool

1. Create `_services/<engine-id>.toml` according to `tentaflow-containers/_schema/SCHEMA.md`
2. For a Docker runtime, add `docker/<engine-id>/Dockerfile` and any runtime files it needs
3. Run `cargo build` in `tentaflow-core/` to validate the manifest and regenerate GUI data

## Current infrastructure assets

- `docker/iroh-relay/` - self-hosted iroh relay + pkarr DNS deployment bundle

## Future candidates

- MCP Filesystem Server
- MCP Git Server
- MCP Web Fetch
- Web Search (SearxNG)
- Web Search (Brave)
- Calculator
- Code Interpreter
- Web Scraper
- SQL Query Tool
- Calendar API
