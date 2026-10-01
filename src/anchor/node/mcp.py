"""Plugin MCP connections use the framework client and the node's sandbox."""

from __future__ import annotations

import shutil
from pathlib import Path
from typing import Any

from anchor.runtime.execenv import NodeSandbox


def http_toolset(name: str, server: dict, *, interactive: bool = False) -> Any:
    from pydantic_ai.mcp import MCPToolset, SSETransport, StreamableHttpTransport

    auth = None
    if server.get("auth") == "oauth" or server.get("oauth_resource"):
        from fastmcp.client.auth import OAuth
        from key_value.aio.stores.filetree import FileTreeStore

        class AuthorizedOAuth(OAuth):
            async def redirect_handler(self, authorization_url: str) -> None:
                if not interactive:
                    raise ValueError("MCP authorization required; run anchor-library --root <data-root> "
                                     f"authorize {server['_anchor_plugin_id']} {server['_anchor_server_name']}")
                await super().redirect_handler(authorization_url)

        directory = Path(server["_anchor_auth_dir"])
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        directory.chmod(0o700)
        auth = AuthorizedOAuth(mcp_url=server["url"], client_name="Anchor",
                              token_storage=FileTreeStore(data_directory=directory))
    transport = SSETransport if server.get("type") == "sse" else StreamableHttpTransport
    return MCPToolset(transport(server["url"], headers=server.get("headers"), auth=auth), id=name,
        init_timeout=300 if interactive else server.get("startup_timeout_sec", 30),
                      read_timeout=server.get("tool_timeout_sec", 300))


def toolsets_for(servers: tuple[tuple[str, dict], ...], sandbox: NodeSandbox, *,
                 interactive: bool = False, defer_loading: bool = True) -> tuple[Any, ...]:
    if not servers:
        return ()
    from pydantic_ai.mcp import MCPToolset, StdioTransport

    result = []
    for name, server in servers:
        if "command" in server:
            plugin_dir = Path(server["_anchor_plugin_dir"])
            cwd = Path(server.get("cwd") or plugin_dir)
            command = server["command"]
            if not Path(command).is_absolute():
                local = (cwd / command).resolve()
                command = str(local) if local.is_file() else shutil.which(command) or command
            wrapped, args = sandbox.isolated_process(
                (command, *server.get("args", [])), plugin_id=server["_anchor_plugin_id"],
                plugin_dir=plugin_dir, cwd=cwd, env=server.get("env", {}))
            toolset = MCPToolset(StdioTransport(command=wrapped, args=args, env={}, keep_alive=False),
                                 id=name, init_timeout=server.get("startup_timeout_sec", 30),
                                 read_timeout=server.get("tool_timeout_sec", 300))
        else:
            if not sandbox.network:
                raise ValueError(f"MCP server {name}: HTTP transport requires node network=true")
            toolset = http_toolset(name, server, interactive=interactive)
        # MCP servers can expose dozens of tools. Keep their definitions out of the
        # initial model request and let PydanticAI's auto-injected ToolSearch capability
        # reveal only the tools that match the current task. The connected toolset is
        # still available to the runtime, so discovery does not change MCP lifecycle or
        # sandbox boundaries.
        exposed = toolset.prefixed(name)
        result.append(exposed.defer_loading() if defer_loading else exposed)
    return tuple(result)
