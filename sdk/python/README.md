# Nexus Computer Use Runtime Python client

This is the official typed Python 3.10–3.13 client for a separately installed,
version-pinned Nexus Computer Use Runtime sidecar. It has no runtime package
downloader and does not expose the private transport token.

Install the local development package with `python -m pip install ./sdk/python`
and run [`examples/quickstart.py`](examples/quickstart.py) with an explicit
sidecar endpoint, token file, and deterministic fixture application name. The
example performs trusted discovery, opens a finite bounded session, observes
one exact window, invokes the fixture counter, observes fresh state, and closes
the session without raw JSON or platform identifiers.

Use `select_application` and `select_window` when resolving human selectors.
They prefer exact identities and reject ambiguous substring matches instead of
silently selecting the first process or platform helper window.

If `perform_action` raises `MutationIndeterminateError`, retain
`error.request` and pass it to `reconcile_action` with a longer timeout. Do not
construct a new mutation to recover from an indeterminate result.
