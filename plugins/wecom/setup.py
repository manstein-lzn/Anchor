"""Install the ordinary assistant Graph without replacing an existing operator definition."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
from pathlib import Path


def install(root: Path) -> Path:
    source = Path(__file__).resolve().parents[2] / "examples" / "graphs" / "wecom-assistant.json"
    target = root / "workspaces" / "wecom-assistant" / "graph.json"
    target.parent.mkdir(parents=True, exist_ok=True)
    if not target.exists():
        shutil.copyfile(source, target)
    # The channel declaration is part of the Plugin bundle.  Keep an operator-managed copy intact,
    # but make a fresh data root immediately able to mount the Plugin on a Graph node.
    plugin_source = Path(__file__).resolve().parent
    plugin_target = root / "library" / "plugins" / "wecom"
    if not plugin_target.exists():
        plugin_target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(plugin_source, plugin_target,
                        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    return target


def check(root: Path) -> list[str]:
    """Local readiness check; never print credentials or contact the platform."""
    from anchor.runtime.secrets import load_dotenv
    load_dotenv()
    issues = []
    for name in ("WECOM_BOT_ID", "WECOM_BOT_SECRET", "ANCHOR_WECOM_GRAPH", "ANCHOR_WECOM_USERS",
                 "ANCHOR_API_KEY", "ANCHOR_API_KEYS"):
        if not os.environ.get(name, "").strip():
            issues.append(f"缺少 {name}")
    try:
        keys = json.loads(os.environ.get("ANCHOR_API_KEYS", "[]"))
        key = os.environ.get("ANCHOR_API_KEY", "")
        if not isinstance(keys, list) or key not in keys or len(key.encode()) < 32:
            issues.append("ANCHOR_API_KEY 必须与 ANCHOR_API_KEYS 中一把至少 32 字节的密钥相同")
    except ValueError:
        issues.append("ANCHOR_API_KEYS 必须是 JSON 数组")
    if importlib.util.find_spec("aibot") is None:
        issues.append("缺少 channels 依赖：pip install -e '.[channels,mcp]'")
    from anchor.simple.graph import load
    try:
        graph = load(root / "workspaces" / os.environ.get("ANCHOR_WECOM_GRAPH", "wecom-assistant") / "graph.json")
        if os.environ.get("ANCHOR_WECOM_REPLY_NODE", "assistant") not in graph.nodes:
            issues.append("回复节点不存在")
    except (OSError, ValueError) as exc:
        issues.append(f"助手 Graph 不可用：{type(exc).__name__}")
    return issues


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(".local/demo"))
    parser.add_argument("--check", action="store_true", help="仅检查本地配置，不连接平台、不显示凭证")
    args = parser.parse_args()
    if args.check:
        issues = check(args.root)
        print("\n".join(issues) if issues else "本地配置检查通过；请启动服务并进行企业微信私聊实测。")
        raise SystemExit(1 if issues else 0)
    print(f"助手 Graph：{install(args.root).resolve()}（已有定义不会被覆盖）")
    print("在根目录 .env 配置 ANCHOR_WECOM_GRAPH=wecom-assistant、ANCHOR_WECOM_USERS、机器人凭证及 API key。")
