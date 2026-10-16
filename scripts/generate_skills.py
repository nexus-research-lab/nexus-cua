#!/usr/bin/env python3
"""Generate and verify host-specific Skills from one bounded catalogue."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CATALOGUE = ROOT / "skills" / "catalogue.json"
COMMON = ROOT / "skills" / "common" / "instructions.md"
MCP_OUTPUT = ROOT / "skills" / "mcp" / "SKILL.md"
CLI_OUTPUT = ROOT / "skills" / "cli" / "SKILL.md"
CLI_MCP_OUTPUT = ROOT / "crates" / "cli" / "assets" / "mcp-skill.md"
CLI_ASSET_OUTPUT = ROOT / "crates" / "cli" / "assets" / "cli-skill.md"
MCP_SERVER = ROOT / "crates" / "mcp" / "src" / "server.rs"
LEAF = re.compile(r"^[a-z][a-z0-9_]*$")
TOOL_ANNOTATION = re.compile(r'name\s*=\s*"([a-z][a-z0-9_]*)"')


def load_catalogue() -> dict[str, object]:
    value = json.loads(CATALOGUE.read_text(encoding="utf-8"))
    if value.get("schema_version") != 1:
        raise ValueError("unsupported Skill catalogue schema_version")
    tools = value.get("tools")
    if not isinstance(tools, list) or not tools:
        raise ValueError("Skill catalogue tools must be a non-empty list")
    leaves: list[str] = []
    for tool in tools:
        if not isinstance(tool, dict):
            raise ValueError("Skill catalogue tool must be an object")
        leaf = tool.get("leaf")
        kind = tool.get("kind")
        vision = tool.get("vision")
        if not isinstance(leaf, str) or not LEAF.fullmatch(leaf):
            raise ValueError(f"invalid MCP tool leaf: {leaf!r}")
        if kind not in {"read", "mutation", "reconcile"}:
            raise ValueError(f"invalid tool kind for {leaf}")
        if not isinstance(vision, bool):
            raise ValueError(f"vision flag must be boolean for {leaf}")
        leaves.append(leaf)
    if len(leaves) != len(set(leaves)):
        raise ValueError("Skill catalogue contains duplicate tool leaves")
    if "reconcile_mutation" not in leaves:
        raise ValueError("Skill catalogue must expose reconcile_mutation")
    return value


def verify_server_catalogue(catalogue: dict[str, object]) -> None:
    source = MCP_SERVER.read_text(encoding="utf-8")
    actual = set(TOOL_ANNOTATION.findall(source))
    tools = catalogue["tools"]
    assert isinstance(tools, list)
    expected = {str(tool["leaf"]) for tool in tools}
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        raise ValueError(f"MCP catalogue drift: missing={missing}, extra={extra}")


def binding_digest(binding: str) -> str:
    digest_input = (
        CATALOGUE.read_bytes()
        + b"\0"
        + COMMON.read_bytes()
        + b"\0"
        + binding.encode("ascii")
    )
    return hashlib.sha256(digest_input).hexdigest()


def validate_rendered(rendered: str, binding: str) -> str:
    if len(rendered.encode("utf-8")) > 24 * 1024:
        raise ValueError(f"generated {binding} Skill exceeds the 24 KiB bound")
    if "nexus_cua_" in rendered or "nexus-cua-mcp" in rendered:
        raise ValueError(f"generated {binding} Skill contains a deleted public identity")
    if not rendered.startswith("---\nname: nexuscua-computer-use\ndescription: "):
        raise ValueError(f"generated {binding} Skill frontmatter is incomplete")
    if any(marker in rendered for marker in ("TODO", "TBD", "Replace with")):
        raise ValueError(f"generated {binding} Skill contains an unfinished placeholder")
    return rendered


def render_mcp(catalogue: dict[str, object], common: str) -> str:
    tools = catalogue["tools"]
    assert isinstance(tools, list)
    alias = catalogue["mcp_alias"]
    version = catalogue["product_version"]
    if alias != "nexuscua":
        raise ValueError("official MCP Skill alias must be nexuscua")
    digest = binding_digest("mcp")

    grouped: dict[str, list[str]] = {"read": [], "mutation": [], "reconcile": []}
    for tool in tools:
        assert isinstance(tool, dict)
        grouped[str(tool["kind"])].append(f"`mcp__{alias}__{tool['leaf']}`")

    lines = [
        "---",
        "name: nexuscua-computer-use",
        "description: Control exact authorized macOS or Windows application windows through the configured nexuscua MCP server when visible native desktop interaction is required; do not use it for browser DOM or ordinary filesystem tasks.",
        "---",
        "",
        f"<!-- generated: product={version} catalogue_sha256={digest} -->",
        "",
        common.rstrip(),
        "",
        "## MCP binding",
        "",
        "Use only the configured `nexuscua` MCP alias. Do not call a shell command,",
        "private sidecar protocol, deleted prefixed tool name, or second interface as a",
        "fallback. A tool absent from the host allowlist is unavailable, not permission",
        "to reach around the adapter.",
        "",
        "Read and verification tools: " + ", ".join(grouped["read"]) + ".",
        "",
        "Mutation tools (each requires its own retained `request_id`): "
        + ", ".join(grouped["mutation"])
        + ".",
        "",
        "Read-only reconciliation tool: " + ", ".join(grouped["reconcile"]) + ".",
        "",
        "The `vision` profile may omit the generic `perform_action`; use the advertised",
        "flat tools. Never infer that a hidden tool is callable.",
        "",
    ]
    return validate_rendered("\n".join(lines), "MCP")


def render_cli(catalogue: dict[str, object], common: str) -> str:
    tools = catalogue["tools"]
    assert isinstance(tools, list)
    version = catalogue["product_version"]
    digest = binding_digest("cli")
    mutations = [
        f"`nexuscua action {tool['leaf']}`"
        for tool in tools
        if isinstance(tool, dict)
        and tool["kind"] == "mutation"
        and tool["leaf"] not in {"launch_application", "terminate_application"}
    ]
    lines = [
        "---",
        "name: nexuscua-computer-use",
        "description: Control exact authorized macOS or Windows application windows through the host-configured nexuscua CLI binding when visible native desktop interaction is required; do not use it for browser DOM or ordinary filesystem tasks.",
        "---",
        "",
        f"<!-- generated: product={version} catalogue_sha256={digest} -->",
        "",
        common.rstrip(),
        "",
        "## CLI binding",
        "",
        "Use only the installed `nexuscua` command and the non-secret registration ID",
        "supplied by the trusted host. Never inspect provider state, discover an endpoint,",
        "read a token, invoke the private service transport, or switch to the MCP binding.",
        "The trusted host owns initial authorization and service lifecycle; do not invoke",
        "human authorization-management commands from this Skill.",
        "",
        "Start with `nexuscua session open --registration <id> --json`; use",
        "`nexuscua session status --registration <id> --json` to refresh status.",
        "Before an unfamiliar operation, run `nexuscua session catalogue --registration",
        "<id> --json` and follow the exact current input schema returned by the service.",
        "",
        "Read operations:",
        "",
        "- `nexuscua app list|list-launchable --registration <id> --json`",
        "- `nexuscua window list|observe|zoom --registration <id> ... --json`",
        "- `nexuscua display list|observe --registration <id> ... --json`",
        "- `nexuscua clipboard read --registration <id> --json`",
        "- `nexuscua verify --registration <id> --input-stdin --json`",
        "",
        "Cold launch and cooperative termination use `nexuscua app launch` and",
        "`nexuscua app terminate`; pass only stable identities or opaque refs returned by",
        "this binding. Every other mutation uses one of: " + ", ".join(mutations) + ".",
        "",
        "Mutation and verify argument objects are bounded JSON delivered directly on stdin.",
        "Do not write argument files or echo sensitive clipboard/text values through an",
        "intermediate shell command. After an uncertain mutation, run",
        "`nexuscua mutation reconcile --registration <id> --request-id <same-id> --json`.",
        "",
    ]
    rendered = validate_rendered("\n".join(lines), "CLI")
    forbidden = ("authority plan", "authority apply")
    if any(value in rendered for value in forbidden):
        raise ValueError("generated CLI Skill contains a forbidden human authority command")
    return rendered


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        catalogue = load_catalogue()
        verify_server_catalogue(catalogue)
        common = COMMON.read_text(encoding="utf-8")
        rendered_mcp = render_mcp(catalogue, common)
        rendered_cli = render_cli(catalogue, common)
        if args.check:
            if (
                not MCP_OUTPUT.is_file()
                or MCP_OUTPUT.read_text(encoding="utf-8") != rendered_mcp
                or not CLI_MCP_OUTPUT.is_file()
                or CLI_MCP_OUTPUT.read_text(encoding="utf-8") != rendered_mcp
                or not CLI_OUTPUT.is_file()
                or CLI_OUTPUT.read_text(encoding="utf-8") != rendered_cli
                or not CLI_ASSET_OUTPUT.is_file()
                or CLI_ASSET_OUTPUT.read_text(encoding="utf-8") != rendered_cli
            ):
                raise ValueError("generated MCP/CLI Skill is missing or stale")
        else:
            MCP_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
            MCP_OUTPUT.write_text(rendered_mcp, encoding="utf-8")
            CLI_MCP_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
            CLI_MCP_OUTPUT.write_text(rendered_mcp, encoding="utf-8")
            CLI_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
            CLI_OUTPUT.write_text(rendered_cli, encoding="utf-8")
            CLI_ASSET_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
            CLI_ASSET_OUTPUT.write_text(rendered_cli, encoding="utf-8")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"skill generation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
