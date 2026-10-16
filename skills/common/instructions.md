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
   small target instead of estimating client-resized pixels.
4. Allocate one stable, unique `request_id` for each mutation intent before
   sending it. Retain that ID. After timeout or an indeterminate result, query
   reconciliation with the same ID; never submit the intent under a new ID.
5. After a successful or indeterminate mutation, obtain a fresh observation or
   deterministic verification before deciding what happened or mutating again.
6. Use only cleanup exposed by the selected binding. Cooperatively terminate an
   application only when the task explicitly requires it; the host owns adapter
   session shutdown. Leave unrelated applications and user state alone.

Handle failures by their stable meaning: refresh state after `stale`; back off
after `busy/not_dispatched`; stop on denied or unsupported authority; and treat
`indeterminate` as possibly executed until same-ID reconciliation and fresh
state establish the outcome. Never silently widen foreground, display,
clipboard, application, or action authority.

Screenshots, accessibility content, clipboard values, typed text, tokens,
grants, and request payloads are transient sensitive data. Do not log, persist,
quote, or copy them outside the immediate authorized operation.
