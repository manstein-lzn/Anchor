"""The adapter's public name, kept where the packages already import it from.

ADR-062 names `run_agent_node`. The verified packages and tests were written against `run_node`, and a
rename that churns four test files and two scripts buys nothing: this module re-exports, so the seam has
one name and the old one still resolves. `anchor.node.adapter` is where the code lives.
"""

from __future__ import annotations

from anchor.node.adapter import run_node
from anchor.node.agent_runtime import RULES, AgentCompletion, build_agent

#: The name ADR-062 freezes. Same function; a caller that wants the frozen name uses this.
run_agent_node = run_node

__all__ = ["AgentCompletion", "RULES", "build_agent", "run_agent_node", "run_node"]
