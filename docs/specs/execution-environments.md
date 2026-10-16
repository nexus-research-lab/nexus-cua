# Execution Environments and Desktop Workers

Nexus Computer Use Runtime controls one operating-system desktop session. It is
not a virtual-machine manager. A trusted host may place that runtime on the
user's current desktop, another logged-in desktop host, or an isolated VM, but
those environments have different truthful capabilities and must not share one
undifferentiated "sandbox" claim.

## Default: the user's local desktop

The default Nexus profile runs the sidecar in the user's current macOS or
Windows session. It can observe the exact windows and file-manager surfaces the
user can already see. No VM, copied home directory, mounted shadow filesystem,
or synthetic desktop is inserted between the runtime and the user.

This profile supports:

- bounded concurrent capability sessions in one native runtime;
- occlusion-independent exact-window observation where the public capture API
  can provide it;
- non-foreground semantic actions where AX or UI Automation truthfully targets
  the selected element; and
- one serialized foreground-input lane per OS desktop session.

The lane fails fast under contention, so one Agent worker cannot create an
unbounded foreground backlog for another. A host may also authorize restoration
of the exact previous foreground owner after temporary input. Restoration is
skipped if the user or system changed foreground after dispatch; it is not a
focus-stealing guarantee.

It does not support machine checkpoint, reset, or clone. Implementing those
operations against a real user desktop would be destructive and misleading.
Closing a capability session resets only CUA authority and transient artifacts;
it does not roll back applications, files, or operating-system state.

## Agent workers versus desktop workers

Many Agent workers may hold separate, host-authorized sessions against one
runtime. That is session multiplexing, not independent desktops. Their reads
can progress through bounded native actors, while foreground keyboard and mouse
input remains serialized because the OS exposes one foreground input stream.

Independent foreground work requires independent desktop workers. A desktop
worker is one runtime plus one OS desktop session and has a host-issued opaque
identity, health epoch, capacity, and capability set. A scheduler above this
repository must provide:

- bounded worker and pending-lease counts;
- owner and task affinity for the lifetime of an action/reconciliation chain;
- capability-aware placement rather than retrying on a different worker;
- one foreground mutation lane per desktop session;
- epoch invalidation on worker restart or reconnect; and
- no cross-worker replay of an admitted or indeterminate mutation.

The optional `nexus-cua-host` crate now implements this mechanism without
scheduling models: bounded worker registration, least-loaded capability-aware
placement, finite leases, exact worker/epoch affinity, fail-fast capacity, and
lease invalidation on unregister/reconnect. It creates no pending placement
queue. Nexus still owns product-level owner/task policy, provisioning, and the
decision to register a local, remote, or isolated worker.

## Remote observation and action transport

The sidecar transport remains local-only: a private Unix socket or owner-only
Windows named pipe. It never opens a TCP listener and its trusted-host token is
never sent to an Agent or remote coordinator.

Remote use terminates at a trusted host gateway on the desktop worker:

```text
Agent -> authenticated Nexus command -> desktop-host gateway
      -> private local CUA client -> local nexus-cua sidecar -> native OS
```

The gateway reissues host policy, deadline, request identity, and artifact
projection locally. It returns ordinary observation data and checksum-bound
image artifacts. A network disconnect does not cancel an admitted mutation;
the same request identity must reconcile on the same worker epoch.

`nexus-cua-host` supplies a concrete transport-neutral gateway and a bounded
length-prefixed JSON stream protocol for a host-provided TLS, mTLS, SSH, or RPC
connection. Every request carries gateway authentication plus exact lease and
worker epoch. The selected worker's existing dispatcher retains idempotent
results. Remote observations never expose worker-local paths: they return a
short-lived artifact ticket bound to the same lease/epoch, and the separate
fetch revalidates runtime ownership, byte length, and SHA-256 before returning
base64 bytes. The crate intentionally opens no unauthenticated TCP listener.

The workspace reference harness runs the controller and desktop worker as two
separate instances of the same executable over loopback mutual TLS with
temporary certificates. It covers certificate rejection before framing,
gateway authentication before worker resolution, exact lease/epoch affinity,
path-free artifact verification, post-admission disconnect reconciliation,
epoch rotation, and frame bounds. Run it with
`make remote-validate REMOTE_SOURCE_REVISION=<40-character-git-sha>`. The
create-new output is a sanitized `nexus.cua.remote-transport.v1` summary; the
temporary private keys, gateway capability, lease, and bootstrap file are
deleted when the controller exits.

## Network and filesystem policy

The core protocol exposes no direct network client and no direct filesystem
read/write operation. Those channels are denied by absence. This does not hide
the user's desktop: a locally authorized file-manager window can still show the
same names and icons that the user sees, and an application can still exercise
its own existing network behavior through its UI.

Direct host file tools, browser/network tools, and mounted VM directories are
separate host capabilities with separate policy. A desktop-worker descriptor
must therefore distinguish:

- GUI-visible application state;
- direct filesystem authority;
- direct network authority; and
- explicitly mounted or synchronized paths in an isolated environment.

An adapter must not infer direct file or network authority from a CUA session.

## Optional isolated VM profile

A VM is optional and off by default. It is useful for parallel foreground
workers, untrusted tasks, reproducible test images, and rollback. It is not a
prerequisite for controlling the user's real desktop.

Only an isolated backend that actually owns the machine image may advertise:

- machine snapshot;
- reset to a named checkpoint;
- clone from an immutable base; or
- network/filesystem policy enforcement below the application layer.

Those operations belong to a host-side environment provider. The provider
must bind every snapshot to the worker epoch and base-image digest, bound clone
and retained-snapshot counts, declare mount/network policy before startup, and
return unsupported on the local-user-desktop profile. `nexus-cua` remains the
same execution runtime inside the guest.

The host crate defines this closed provider interface and descriptors now.
Direct network policy is `not_enforced`, `deny_all`, a bounded normalized host
allowlist, or explicit isolated-provider `unrestricted`. Direct filesystem
policy is `not_enforced`, `no_direct_access`, or a bounded set of opaque mounts
with read-only flags; public host paths are absent. Snapshot, reset, and clone
return provider-owned opaque capabilities bound to the source worker epoch and
base-image digest. The built-in local desktop provider truthfully returns
unsupported for all three. No VM backend is bundled or silently emulated.

## Capability matrix

| Capability | Local user desktop | Remote physical desktop | Isolated VM |
| --- | --- | --- | --- |
| See the user's current desktop files through UI | Yes | Only what that desktop exposes | Only explicit mounts/sync |
| Background exact-window observation | Platform-dependent, fail closed | Platform-dependent, fail closed | Platform-dependent, fail closed |
| Background semantic action | When AX/UIA supports exact targeting | Same | Same |
| Foreground pixel input | Serialized | Serialized per remote desktop | Serialized per VM |
| Independent parallel foreground workers | No | One per independent OS session | Yes, bounded by VM pool |
| Direct filesystem/network authority | Not provided by CUA | Not provided by CUA | Separate environment policy |
| Machine snapshot/reset/clone | Unsupported | Provider-specific | Negotiated provider capability |

This split keeps ordinary desktop use invisible and lightweight while leaving a
clean extension point for isolation. An unavailable VM capability is reported
as unavailable; it is never emulated by copying screenshots or restarting a CUA
session.
