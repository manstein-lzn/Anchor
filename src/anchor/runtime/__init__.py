"""Framework-independent runtime protocols."""

from .protocols import GeneralHarness, HarnessResult, WorkflowService
from .local import DeterministicHarness, InProcessWorkflowService
from .watchdog import AdaptiveWatchdog, RunHealth, WatchdogDecision
from anchor.domain.models import DiagnosticRequest, ProgressEvidence

__all__ = [
    "DeterministicHarness",
    "GeneralHarness",
    "HarnessResult",
    "InProcessWorkflowService",
    "WorkflowService",
    "AdaptiveWatchdog",
    "DiagnosticRequest",
    "ProgressEvidence",
    "RunHealth",
    "WatchdogDecision",
]
"""Runtime protocols and replaceable execution adapters."""

from .capabilities import (AgentCapability, CapabilityRegistry, ModelProfile, ToolCapability,
                           VerifierCapability)
from .artifacts import ArtifactStore, LocalArtifactStore
from .config import RuntimeConfig, load_runtime_config
from .model_gateway import ModelGateway, ModelResponse, build_model_gateway
from .secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider, SecretProvider
from .sinks import ArtifactCheckpointSink, VerificationCheckpointSink
from anchor.domain.propagation import plan_ready_nodes
from .context import build_input_snapshot, canonical_json, input_hash
from .worker_loop import run_worker_loop
from .agent_tools import AgentToolLoop
from .integrity import IntegrityError, IntegrityIssue, check_run, require_clean
from .model_gateway import ToolFunction
from .tool_gateway import (BubblewrapBackend, SubprocessBackend, ToolCallResult,
                           ToolDenied, ToolGateway)
from .memory import LocalMemoryStore, MemoryRecord, MemoryStore
from .settings import AnchorSettings

__all__ = [
    "AgentCapability",
    "AnchorSettings",
    "ArtifactCheckpointSink",
    "ArtifactStore",
    "CapabilityRegistry",
    "ChainedSecretProvider",
    "EnvironmentSecretProvider",
    "JsonFileSecretProvider",
    "ModelGateway",
    "ModelProfile",
    "ModelResponse",
    "LocalArtifactStore",
    "RuntimeConfig",
    "SecretProvider",
    "ToolCapability",
    "VerifierCapability",
    "VerificationCheckpointSink",
    "build_model_gateway",
    "load_runtime_config",
    "plan_ready_nodes",
    "build_input_snapshot",
    "canonical_json",
    "input_hash",
    "run_worker_loop",
    "AgentToolLoop",
    "BubblewrapBackend",
    "IntegrityError",
    "IntegrityIssue",
    "LocalMemoryStore",
    "MemoryRecord",
    "MemoryStore",
    "SubprocessBackend",
    "ToolCallResult",
    "ToolDenied",
    "ToolFunction",
    "ToolGateway",
    "check_run",
    "require_clean",
]
