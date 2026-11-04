---
name: nexuscua-computer-use
description: Control exact authorized macOS or Windows application windows through the host-configured nexuscua CLI binding when visible native desktop interaction is required; do not use it for browser DOM or ordinary filesystem tasks.
---

<!-- generated: product=0.2.0-rc.5 catalogue_sha256=10f59dcd6b8aaf0e3ba41c6ac9b046b343c0eaa134da8ee8acf0610ae5e85158 -->

## Operating contract

Use Computer Use only for visible native desktop interaction. Prefer a browser,
filesystem, terminal, or application-specific API when the task belongs to
that capability. Never use native window control as a hidden substitute for a
more precise interface.

1. Read status and require the needed host authority and native permissions.
   When status includes an authority-provider `request_namespace`, prefix every
   mutation `request_id` with that exact namespace followed by `:`.
2. Select an exact authorized application and exact visible top-level window.
   Treat display capture as read-only overview; a display observation never
   authorizes an action.
3. Observe the exact window immediately before a mutation. Use accessibility
   facts when reliable and the screenshot for visual targets. Coordinates are
   top-left normalized `0..1000`; use zoom and its opaque view reference for a
   small target instead of estimating client-resized pixels. Scroll also requires
   a target x/y in this space; positive vertical deltas scroll down. Prefer
   `mod` for portable shortcuts (Command on macOS, Control on Windows).
4. Allocate one stable, unique `request_id` for each mutation intent before
   sending it. Retain that ID. After timeout or an indeterminate result, query
   reconciliation with the same ID; never submit the intent under a new ID.
5. After a successful or indeterminate mutation, obtain a fresh observation or
   deterministic verification before deciding what happened or mutating again.
   If capabilities advertise `observe_after`, generic `perform_action` may
   request `{include_screenshot: true, accessibility: "interactive",
   application_scene: false}` in that field. Use its fresh `observation` as the
   next guard. The old guard remains consumed. `observation_error` affects only
   the follow-up read: preserve the action result and do not replay the action.
   Bounded visual settling does not prove business completion. Flat tools keep
   their existing separate observe flow.
6. Use only cleanup exposed by the selected binding. Cooperatively terminate an
   application only when the task explicitly requires it; the host owns adapter
   session shutdown. Leave unrelated applications and user state alone.

Handle failures by their stable meaning: refresh state after `stale`; back off
after `busy/not_dispatched`; stop on denied or unsupported authority; and treat
`indeterminate` as possibly executed until same-ID reconciliation and fresh
state establish the outcome. Never silently widen foreground, display,
clipboard, application, or action authority.

On macOS, outside input between actions preserves authority but invalidates
earlier observations. The runtime waits for two idle seconds, at most five
seconds per action request; held keys/buttons do not count as idle. After
`stale_observation`, observe again and decide the next action from fresh state.
For `busy` with `wait_for_idle_then_observe`, retry at most once after a fresh
observation, then report continued activity to the host. Do not repeatedly
renew sessions or replay partial actions. This wait does not extend authority.

On `user_active` during native dispatch, stop automatic actions and tell the host that the user took
over. Do not retry after a delay, silently open a replacement session, or treat
fresh observation as permission to resume. Only a new explicit host grant may
resume control. Reconcile any indeterminate action without replaying it.
On macOS, foreground-enabled sessions require Input Monitoring permission for
passive takeover detection, in addition to event synthesis permission. The
monitor retains only current held-input state and latest activity time; it never retains characters, pointer coordinates or event history.

Screenshots, accessibility content, clipboard values, typed text, tokens,
grants, and request payloads are transient sensitive data. Do not log, persist,
quote, or copy them outside the immediate authorized operation.

Window lists report `state`: normal, minimized, offscreen, or unknown. Unknown
is not proof of visibility or readiness; re-observe before input. Screenshot-only
observation does not require accessibility permission.

C7: `scroll` accepts an explicit normalized point and deltas only. The removed
`element_ref` field is rejected, even when null. Semantic scrolling is not
implemented; do not supply an element reference as a scroll target.

C8: Protocol/SDK normalized coordinates are finite numbers in `[0, 1000]`.
Fractional coordinates preserve crop/zoom precision; runtime rounds once at the
final pixel mapping. Dedicated model-facing normalized tools retain integer
inputs. NaN, infinity, negative, and out-of-range coordinates are rejected.

C6: Foreground click, hold, scroll and both drag endpoints are checked on the
serialized input actor before pointer dispatch. macOS requests AppKit mouse-down hit testing on the main thread through a
bounded queue, then resolves ownership on the serialized input actor; Windows checks
the process owning WindowFromPoint, permitting same-process popups. Pointer
actions do not first raise the target and erase evidence of foreign occlusion.
Move-only and explicit background clicks are exempt from this foreground check.
Scroll first moves the cursor to its target. Unowned points fail not_dispatched
with observe_window guidance. When capabilities advertise `application_scene`, pass `application_scene=true` to MCP `observe_window` (CLI: `window observe --application-scene`) to see the authorized application's menus and popups on the anchor display. Other applications are excluded. Accessibility still describes the anchor window; use the returned screenshot mapping for visual coordinates and foreground delivery for scene clicks. Helpers owned by another process are not included.

C4: `serve` and `mcp serve` accept `--max-image-edge` (default 1568, range 1..4096).
RuntimeConfig exposes the same model-image budget. macOS window streams and
one-shot window/display capture request that output size natively. The native
output queue retains only the latest pixel buffer and converts it on demand;
only owned Rust pixels leave that queue. At most one conversion is pending per
stream. PNG output is RGB, flattening transparency over white, and reports its
actual dimensions in ScreenshotMapping. Drivers without native resizing use a
bounded encoder-stage resize; Windows WGC still acquires native-size frames.
Public SDK request shapes are unchanged; map points using returned dimensions.

## Runtime action limits (C9)

`get_capabilities.action_limits` is a closed object containing the effective
`max_session_ttl_seconds`, `max_text_bytes`, `max_keys`, `max_scroll_delta`,
`max_pointer_duration_ms`, `max_window_coordinate`, `max_window_dimension`,
`max_range_value` and `max_click_count`. The session limit reflects the runtime
configuration; action values share the runtime validator's constants. Text
limits count UTF-8 bytes. Hosts should derive UI/schema bounds from this object
and cap session authority by their own remaining round duration. Capabilities
do not grant authority; the runtime still validates every request.

Inactive/Stage Manager window recovery: a visible thumbnail is not proof of
normal window geometry, and `unknown` must not be treated as minimized. If
accessibility cannot resolve the inactive window, obtain a geometry-only guard
with `observe_window(include_screenshot=false, accessibility=disabled)`, then
explicitly `focus_window` with foreground authority and re-observe. Never
activate as a side effect of a read. Do not retry an indeterminate activation;
reconcile the request and inspect current state first.

## CLI binding

Use only the installed `nexuscua` command and the non-secret registration ID
supplied by the trusted host. Never inspect provider state, discover an endpoint,
read a token, invoke the private service transport, or switch to the MCP binding.
The trusted host owns initial authorization and service lifecycle; do not invoke
human authorization-management commands from this Skill.

Start with `nexuscua session open --registration <id> --json`; use
`nexuscua session status --registration <id> --json` to refresh status.
Before an unfamiliar operation, run `nexuscua session catalogue --registration
<id> --json` and follow the exact current input schema returned by the service.

Read operations:

- `nexuscua app list|list-launchable --registration <id> --json`
- `nexuscua window list|observe|zoom --registration <id> ... --json`
- `nexuscua display list|observe --registration <id> ... --json`
- `nexuscua clipboard read --registration <id> --json`
- `nexuscua verify --registration <id> --input-stdin --json`

Cold launch and cooperative termination use `nexuscua app launch` and
`nexuscua app terminate`; pass only stable identities or opaque refs returned by
this binding. Every other mutation uses one of: `nexuscua action write_clipboard`, `nexuscua action click_normalized`, `nexuscua action click_view_normalized`, `nexuscua action type_text`, `nexuscua action press_keys`, `nexuscua action scroll`, `nexuscua action drag_normalized`, `nexuscua action invoke_element`, `nexuscua action set_element_value`, `nexuscua action set_range_value`, `nexuscua action focus_window`, `nexuscua action perform_action`.

Mutation and verify argument objects are bounded JSON delivered directly on stdin.
Do not write argument files or echo sensitive clipboard/text values through an
intermediate shell command. After an uncertain mutation, run
`nexuscua mutation reconcile --registration <id> --request-id <same-id> --json`.
