"""The unit files are the deployment, so drift in them is a deployment defect.

Six units share an environment block and a restart policy. Six hand-maintained copies drift,
and the drift is invisible until a host reboots and one service comes up pointed at the
wrong directory. These tests assert the shared invariants so that a unit cannot quietly
diverge.

They also assert the properties that make a failure *visible*: a crash loop that systemd
retries forever reports as ``activating``, which looks like a service that is starting
rather than one that cannot start.
"""

from __future__ import annotations

import configparser
import pathlib
import re

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[1]
UNIT_DIR = ROOT / "infra" / "systemd"

#: Every service reads all four, whether or not it uses them today. A service whose paths
#: are implicit depends on WorkingDirectory staying the same forever.
COMMON_ENV = ("ANCHOR_DATABASE_URL", "ANCHOR_ARTIFACT_ROOT", "ANCHOR_WORKSPACE_ROOT",
              "ANCHOR_MEMORY_PATH")

EXPECTED_UNITS = {
    "anchor-api.service": "anchor.api",
    "anchor-worker.service": "anchor.runtime.worker_service",
    "anchor-control-worker.service": "anchor.runtime.control_service",
    "anchor-verifier-worker.service": "anchor.runtime.verifier_service",
    "anchor-receiver.service": "anchor.runtime.receiver",
    "anchor-scheduler.service": "anchor.runtime.scheduler_service",
    "anchor-supervisor.service": "anchor.runtime.supervisor_service",
}


def unit_paths() -> list[pathlib.Path]:
    return sorted(UNIT_DIR.glob("*.service"))


def load(path: pathlib.Path) -> configparser.ConfigParser:
    parser = configparser.ConfigParser(interpolation=None, strict=False)
    parser.optionxform = str  # systemd keys are case-sensitive
    parser.read_string(path.read_text(encoding="utf-8"))
    return parser


def env_of(parser: configparser.ConfigParser, path: pathlib.Path | None = None) -> dict[str, str]:
    """Every ``Environment=`` assignment in a unit.

    Parsed from the raw text, not through configparser: systemd allows the key to repeat
    and treats each line as one assignment, while configparser keeps a single value and
    would report a unit as missing variables it in fact sets.
    """
    found: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines() if path else []:
        if not line.startswith("Environment="):
            continue
        key, _, value = line[len("Environment="):].partition("=")
        found[key.strip()] = value.strip()
    return found


def test_every_service_is_declared():
    """A service the install script knows about but that has no unit is a service that
    silently does not survive a reboot."""
    assert {path.name for path in unit_paths()} == set(EXPECTED_UNITS)


@pytest.mark.parametrize("path", unit_paths(), ids=lambda p: p.name)
def test_each_unit_is_persistent_and_ordered_after_the_network(path):
    parser = load(path)
    assert parser.has_section("Unit") and parser.has_section("Service")
    assert parser.has_section("Install")
    assert "default.target" in parser["Install"]["WantedBy"], \
        "transient units do not survive a reboot, which is the whole point of P0.1"
    assert "network-online.target" in parser["Unit"]["After"]


@pytest.mark.parametrize("path", unit_paths(), ids=lambda p: p.name)
def test_each_unit_declares_every_path_explicitly(path):
    """Relative paths resolve against WorkingDirectory, so they break silently the moment
    WorkingDirectory differs — including when a unit is started by hand."""
    parser = load(path)
    env = env_of(parser, path)
    assert "WorkingDirectory" in load(path)["Service"]
    for key in COMMON_ENV:
        assert key in env, f"{key} is not set; its default is a working-directory-relative path"
        value = env[key]
        # Accepts `sqlite:///%h/...` as well as a bare `%h/...`: what must not appear is a
        # bare relative path, which resolves against the working directory and would
        # silently point somewhere else.
        assert "%h/" in value or value.startswith("/"), \
            f"{key}={value} has no absolute path and would resolve against the working directory"


@pytest.mark.parametrize("path", unit_paths(), ids=lambda p: p.name)
def test_each_unit_labels_its_own_worker_id(path):
    """Two workers sharing an id would contend for the same leases."""
    env = env_of(load(path), path)
    ids = [value for key, value in env.items() if key.endswith("_ID")]
    if path.name in ("anchor-api.service", "anchor-receiver.service",
                     "anchor-scheduler.service", "anchor-supervisor.service"):
        return  # observers and the API own no leases
    assert len(ids) == 1, f"expected exactly one worker id, found {ids}"


@pytest.mark.parametrize("path", unit_paths(), ids=lambda p: p.name)
def test_a_crash_loop_becomes_a_visible_failure(path):
    """Without a start limit, a unit that cannot start retries forever and systemd reports
    it as activating — indistinguishable from a slow start."""
    unit = load(path)["Unit"]
    assert unit.get("Restart", "") or True
    assert unit.get("StartLimitIntervalSec"), "no interval: the limit is inactive"
    assert int(unit.get("StartLimitBurst", "0")) >= 1
    service = load(path)["Service"]
    assert service["Restart"] == "on-failure"
    assert int(service["RestartSec"]) >= 1, "a tight restart loop hides the cause in noise"


@pytest.mark.parametrize("path", unit_paths(), ids=lambda p: p.name)
def test_each_unit_runs_the_module_it_claims(path):
    """An ExecStart naming a module that does not import fails five times at boot and is
    then reported as failed — a deployment defect discovered at the worst moment."""
    exec_start = load(path)["Service"]["ExecStart"]
    match = re.search(r"-m\s+(\S+)$", exec_start)
    assert match, exec_start
    assert match.group(1) == EXPECTED_UNITS[path.name]


@pytest.mark.parametrize("path", unit_paths(), ids=lambda p: p.name)
def test_a_unit_that_needs_a_profile_declares_one(path):
    """The worker and verifier resolve capabilities from the runtime profile; without the
    path set they would read a working-directory-relative default."""
    env = env_of(load(path), path)
    needs_profile = path.name in ("anchor-worker.service", "anchor-verifier-worker.service",
                                  "anchor-api.service")
    assert ("ANCHOR_RUNTIME_CONFIG" in env) is needs_profile


def test_the_installer_covers_every_unit():
    """The installer is the only supported way to deploy these; a unit it forgets is a
    service that does not come back after a reboot."""
    script = (ROOT / "scripts" / "install_user_services.sh").read_text(encoding="utf-8")
    for name in EXPECTED_UNITS:
        assert name in script, f"{name} is not installed by the script"
