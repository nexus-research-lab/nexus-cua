# Nexus CUA

Nexus CUA is a model-neutral Computer Use runtime for controlling native desktop
applications on macOS and Windows. It provides a stable, capability-bounded
protocol above replaceable platform drivers.

The repository intentionally does not contain an agent loop, model provider,
chat product, or Nexus product semantics. Products bring their own model,
permission UI, approval policy, and audit experience.

## Integration surfaces

- Embedded products use the versioned local protocol through a private Unix
  socket or Windows named pipe.
- Shell-oriented agents use the `nexus-cua` CLI.
- MCP is an optional compatibility surface for third-party agents; Nexus itself
  uses its round-scoped `nexus computer` command and never exposes a driver MCP
  directly to the model.

## Architecture boundary

```text
Product / Agent
      |
SDK / CLI / optional MCP
      |
Nexus CUA protocol and authorization runtime
      |
Replaceable driver adapter
      |
macOS / Windows
```

Nexus CUA implements its own public-API desktop drivers. It does not wrap or
download another Computer Use runtime. Foundation libraries may provide safe
language bindings to operating-system APIs, but driver behavior, authorization,
state management, protocol design, and release artifacts are owned here.

## Status

Early development. The public protocol is not stable until the first tagged
`v0.1.0` release.

## License

MIT. See [LICENSE](LICENSE).
