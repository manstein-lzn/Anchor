from __future__ import annotations

from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sqlite3
from typing import Any, Iterator
import uuid
import re


SCHEMA_VERSION = 2
ITEM_GROUPS = (
    ("situation", "confirmed_facts"), ("situation", "active_hypotheses"),
    ("situation", "unresolved_conflicts"), ("situation", "blockers"),
    ("experience", "decisions"), ("experience", "failed_paths"),
    ("intent", "open_questions"),
)


class DurableError(RuntimeError):
    pass


class ConflictError(DurableError):
    pass


class NotFoundError(DurableError):
    pass


class DurableStore:
    """One session Task and its immutable Checkpoint chain."""

    def __init__(self, state_path: str | Path, *, create: bool = False) -> None:
        self.db_path = Path(state_path).expanduser().resolve()
        existed = self.db_path.is_file()
        if not create and not self.db_path.is_file():
            raise NotFoundError(f"Anchor State not found: {self.db_path}")
        if create:
            self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(self.db_path, timeout=30, isolation_level=None)
        self.conn.row_factory = sqlite3.Row
        self.conn.execute("PRAGMA foreign_keys = ON")
        if create and not existed:
            self._create_schema(self.conn)
            self.conn.execute("INSERT INTO meta(key,value) VALUES('schema_version',?)", (str(SCHEMA_VERSION),))
        else:
            self._require_schema()

    def close(self) -> None:
        self.conn.close()

    @contextmanager
    def transaction(self) -> Iterator[sqlite3.Connection]:
        try:
            self.conn.execute("BEGIN IMMEDIATE")
            yield self.conn
            self.conn.execute("COMMIT")
        except Exception:
            if self.conn.in_transaction:
                self.conn.execute("ROLLBACK")
            raise

    def begin(
        self,
        *,
        session_id: str,
        proposal_hash: str,
        title: str,
        contract: dict[str, Any],
        candidate: dict[str, Any],
    ) -> dict[str, Any]:
        session_id = _text(session_id, "session_id")
        proposal_hash = _sha256(proposal_hash, "proposal_hash")
        title = _text(title, "title")
        contract = _normalize_contract(contract)
        normalized = _normalize_candidate(candidate, expected_kind="planning", session_id=session_id)
        if normalized["frontier"]["source_hash"] != proposal_hash:
            raise ValueError("planning frontier.source_hash must match proposal_hash")
        fingerprint = _digest({"title": title, "contract": contract, "candidate": normalized})

        with self.transaction() as conn:
            row = conn.execute("SELECT * FROM task WHERE singleton=1").fetchone()
            if row is not None:
                task = dict(row)
                if task["session_id"] != session_id:
                    raise ConflictError("Anchor State belongs to a different session")
                if task["proposal_hash"] != proposal_hash:
                    raise ConflictError("this session already has a different Anchor Task")
                if task["initialization_hash"] != fingerprint:
                    raise ConflictError("proposal hash was reused with different initialization content")
                return self._recovery(task)

            now = _now()
            task_id = _new_id("task")
            payload = _checkpoint_payload(task_id, 0, None, normalized)
            event_id = _new_id("checkpoint")
            content_hash = _digest(payload)
            conn.execute(
                """INSERT INTO task(
                       singleton,task_id,session_id,proposal_hash,initialization_hash,title,
                       contract_json,contract_hash,lifecycle_status,state_version,created_at,updated_at
                   ) VALUES(1,?,?,?,?,?,?,?,?,?,?,?)""",
                (task_id, session_id, proposal_hash, fingerprint, title, _json(contract), _digest(contract), "active", 0, now, now),
            )
            conn.execute(
                """INSERT INTO checkpoints(
                       task_id,checkpoint_version,event_id,frontier_hash,cognition_hash,
                       payload_json,content_hash,created_at
                   ) VALUES(?,?,?,?,?,?,?,?)""",
                (
                    task_id,
                    0,
                    event_id,
                    _digest(payload["frontier"]),
                    _digest(payload["cognition"]),
                    _json(payload),
                    content_hash,
                    now,
                ),
            )
        return self.recover(session_id=session_id, task_id=task_id)

    def recover(self, *, session_id: str, task_id: str | None = None) -> dict[str, Any]:
        task = self._task(session_id=session_id, task_id=task_id)
        return self._recovery(task)

    def update(
        self,
        *,
        session_id: str,
        task_id: str,
        expected_version: int,
        candidate: dict[str, Any],
    ) -> dict[str, Any]:
        if isinstance(expected_version, bool) or not isinstance(expected_version, int) or expected_version < 0:
            raise ValueError("expected_version must be a non-negative integer")
        session_id = _text(session_id, "session_id")
        task_id = _text(task_id, "task_id")
        normalized = _normalize_candidate(candidate, expected_kind="compact", session_id=session_id)
        candidate_task = candidate.get("task_id")
        if candidate_task is not None and candidate_task != task_id:
            raise ConflictError("checkpoint candidate Task identity mismatch")
        frontier_hash = _digest(normalized["frontier"])
        cognition_hash = _digest(normalized["cognition"])

        with self.transaction() as conn:
            task = self._task(session_id=session_id, task_id=task_id)
            existing = conn.execute(
                "SELECT * FROM checkpoints WHERE task_id=? AND frontier_hash=?",
                (task_id, frontier_hash),
            ).fetchone()
            if existing is not None:
                if existing["cognition_hash"] != cognition_hash:
                    raise ConflictError("checkpoint frontier already committed with different cognition")
                return _event(existing)
            if int(task["state_version"]) != expected_version:
                raise ConflictError("Task state_version is stale")

            latest = self._latest(task_id)
            if latest is None:
                raise DurableError("Checkpoint 0 is missing")
            latest_payload = _verified_payload(latest)
            if normalized["missing_transition_certificate"]:
                normalized["transition_certificate"] = _carry_transition(latest_payload["cognition"], normalized["cognition"])
            _validate_transition(normalized["transition_certificate"], latest_payload["cognition"])
            _validate_recall_references(self.conn, task_id, normalized["transition_certificate"], normalized["cognition"])
            checkpoint_version = int(latest["checkpoint_version"]) + 1
            parent = {
                "checkpoint_version": int(latest["checkpoint_version"]),
                "content_hash": str(latest["content_hash"]),
            }
            payload = _checkpoint_payload(task_id, checkpoint_version, parent, normalized)
            now = _now()
            event_id = _new_id("checkpoint")
            content_hash = _digest(payload)
            conn.execute(
                """INSERT INTO checkpoints(
                       task_id,checkpoint_version,event_id,frontier_hash,cognition_hash,
                       payload_json,content_hash,created_at
                   ) VALUES(?,?,?,?,?,?,?,?)""",
                (task_id, checkpoint_version, event_id, frontier_hash, cognition_hash, _json(payload), content_hash, now),
            )
            changed = conn.execute(
                "UPDATE task SET state_version=state_version+1,updated_at=? WHERE singleton=1 AND task_id=? AND state_version=?",
                (now, task_id, expected_version),
            )
            if changed.rowcount != 1:
                raise ConflictError("Task changed while committing Checkpoint")
            row = conn.execute("SELECT * FROM checkpoints WHERE event_id=?", (event_id,)).fetchone()
        return _event(row)

    def recall(
        self,
        *,
        session_id: str,
        task_id: str,
        locator: str,
        content_hash: str | None = None,
    ) -> dict[str, Any]:
        """Resolve one exact item from an immutable Checkpoint."""
        task = self._task(session_id=session_id, task_id=task_id)
        match = re.fullmatch(r"checkpoint:(\d+):item:([^:]+)", _text(locator, "locator"))
        if match is None:
            raise ValueError("recall locator must be checkpoint:<version>:item:<id>")
        version, item_id = int(match.group(1)), match.group(2)
        row = self.conn.execute(
            "SELECT * FROM checkpoints WHERE task_id=? AND checkpoint_version=?",
            (task["task_id"], version),
        ).fetchone()
        if row is None:
            raise NotFoundError(f"Checkpoint {version} not found")
        payload = _verified_payload(row)
        item = next((candidate for section, field in ITEM_GROUPS for candidate in payload["cognition"][section][field] if candidate["id"] == item_id), None)
        if item is None:
            raise NotFoundError(f"Checkpoint item not found: {locator}")
        item_hash = _digest(item)
        if content_hash is not None and _sha256(content_hash, "content_hash") != item_hash:
            raise ConflictError("recalled item content hash mismatch")
        return {
            "schema": "anchor.recall.v1",
            "task_id": task["task_id"],
            "locator": locator,
            "checkpoint_version": version,
            "checkpoint_hash": row["content_hash"],
            "item": item,
            "content_hash": item_hash,
        }

    def _recovery(self, task: dict[str, Any]) -> dict[str, Any]:
        latest = self._latest(str(task["task_id"]))
        if latest is None:
            raise DurableError("Checkpoint 0 is missing")
        checkpoint = _checkpoint(latest)
        version = int(latest["checkpoint_version"])
        if int(task["state_version"]) != version:
            raise DurableError("Task state_version does not match the latest Checkpoint")
        if version == 0:
            if checkpoint["parent"] is not None:
                raise DurableError("Checkpoint 0 must not have a parent")
        else:
            previous = self.conn.execute(
                "SELECT * FROM checkpoints WHERE task_id=? AND checkpoint_version=?",
                (task["task_id"], version - 1),
            ).fetchone()
            if previous is None:
                raise DurableError("Checkpoint parent is missing")
            _checkpoint(previous)
            expected_parent = {"checkpoint_version": version - 1, "content_hash": previous["content_hash"]}
            if checkpoint["parent"] != expected_parent:
                raise DurableError("Checkpoint parent link mismatch")
        cognition = checkpoint["cognition"]
        contract = json.loads(task["contract_json"])
        if _digest(contract) != task["contract_hash"]:
            raise DurableError("Task Contract content hash mismatch")
        recovery_checkpoint = {**checkpoint, "cognition": {**checkpoint["cognition"], "accepted_next_action": checkpoint["cognition"]["intent"]["accepted_next_action"]}}
        return {
            "schema": "anchor.recovery.v1",
            "task_id": task["task_id"],
            "task": {
                "task_id": task["task_id"],
                "session_id": task["session_id"],
                "title": task["title"],
                "lifecycle_status": task["lifecycle_status"],
                "state_version": int(task["state_version"]),
            },
            "contract": {"content": contract, "content_hash": task["contract_hash"]},
            "checkpoint": recovery_checkpoint,
            "control_status": task["lifecycle_status"],
            "next_action": cognition["intent"]["accepted_next_action"],
        }

    def _task(self, *, session_id: str, task_id: str | None = None) -> dict[str, Any]:
        session_id = _text(session_id, "session_id")
        row = self.conn.execute("SELECT * FROM task WHERE singleton=1").fetchone()
        if row is None:
            raise NotFoundError("Anchor Task not found")
        task = dict(row)
        if task["session_id"] != session_id:
            raise ConflictError("Anchor State belongs to a different session")
        if task_id is not None and task["task_id"] != _text(task_id, "task_id"):
            raise ConflictError("Anchor Task identity mismatch")
        return task

    def _latest(self, task_id: str) -> sqlite3.Row | None:
        return self.conn.execute(
            "SELECT * FROM checkpoints WHERE task_id=? ORDER BY checkpoint_version DESC LIMIT 1",
            (task_id,),
        ).fetchone()

    def _require_schema(self) -> None:
        try:
            version = self._meta("schema_version")
        except sqlite3.Error as exc:
            raise DurableError("invalid Anchor State") from exc
        if version != str(SCHEMA_VERSION):
            raise DurableError(f"unsupported Anchor schema {version or 'unknown'}")

    def _meta(self, key: str) -> str | None:
        row = self.conn.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
        return str(row[0]) if row else None

    @staticmethod
    def _create_schema(conn: sqlite3.Connection) -> None:
        conn.executescript(
            """
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS task(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                task_id TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL UNIQUE,
                proposal_hash TEXT NOT NULL UNIQUE,
                initialization_hash TEXT NOT NULL,
                title TEXT NOT NULL,
                contract_json TEXT NOT NULL,
                contract_hash TEXT NOT NULL,
                lifecycle_status TEXT NOT NULL,
                state_version INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS checkpoints(
                task_id TEXT NOT NULL REFERENCES task(task_id),
                checkpoint_version INTEGER NOT NULL,
                event_id TEXT NOT NULL UNIQUE,
                frontier_hash TEXT NOT NULL,
                cognition_hash TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY(task_id,checkpoint_version),
                UNIQUE(task_id,frontier_hash)
            );
            """
        )


def _normalize_contract(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("schema") != "anchor.contract.v1":
        raise ValueError("contract schema must be anchor.contract.v1")
    contract = {
        "schema": "anchor.contract.v1",
        "goal": _text(value.get("goal"), "contract.goal"),
        "execution_plan": _text(value.get("execution_plan"), "contract.execution_plan"),
    }
    for field in (
        "rationale",
        "acceptance_criteria",
        "constraints",
        "non_goals",
        "verification_commands",
        "allowed_paths",
        "risks",
    ):
        contract[field] = _strings(value.get(field), f"contract.{field}")
    if not contract["acceptance_criteria"]:
        raise ValueError("contract.acceptance_criteria must not be empty")
    return contract


def _normalize_candidate(value: Any, *, expected_kind: str, session_id: str) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("schema") != "anchor.checkpoint-candidate.v1":
        raise ValueError("checkpoint candidate schema must be anchor.checkpoint-candidate.v1")
    frontier = _normalize_frontier(value.get("frontier"))
    if frontier["kind"] != expected_kind:
        raise ValueError(f"checkpoint frontier.kind must be {expected_kind}")
    if frontier["session_id"] != session_id:
        raise ConflictError("checkpoint frontier session identity mismatch")
    cognition, legacy = _normalize_cognition(value.get("cognition"))
    missing_certificate = value.get("transition_certificate") is None
    certificate = _normalize_transition(value.get("transition_certificate"), cognition, required=(expected_kind == "compact" and not legacy and not missing_certificate))
    return {
        "schema": "anchor.checkpoint-candidate.v1",
        "frontier": frontier,
        "cognition": cognition,
        "transition_certificate": certificate,
        "missing_transition_certificate": missing_certificate,
        "provenance": _normalize_provenance(value.get("provenance"), expected_kind),
    }


def _normalize_frontier(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("checkpoint frontier must be an object")
    kind = value.get("kind")
    session_id = _text(value.get("session_id"), "checkpoint frontier.session_id")
    if kind == "planning":
        return {"kind": kind, "session_id": session_id, "source_hash": _sha256(value.get("source_hash"), "checkpoint frontier.source_hash")}
    if kind == "compact":
        split = value.get("is_split_turn")
        if not isinstance(split, bool):
            raise ValueError("checkpoint frontier.is_split_turn must be boolean")
        return {
            "kind": kind,
            "session_id": session_id,
            "first_kept_entry_id": _text(value.get("first_kept_entry_id"), "checkpoint frontier.first_kept_entry_id"),
            "episode_hash": _sha256(value.get("episode_hash"), "checkpoint frontier.episode_hash"),
            "is_split_turn": split,
        }
    raise ValueError("checkpoint frontier.kind must be planning or compact")


def _normalize_cognition(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("checkpoint cognition must be an object")
    if value.get("schema") == "anchor.cognition.v2":
        return _legacy_cognition(value), True
    if value.get("schema") != "anchor.cognition.v3":
        raise ValueError("checkpoint cognition schema must be anchor.cognition.v3")
    situation = value.get("situation")
    experience = value.get("experience")
    intent = value.get("intent")
    if not isinstance(situation, dict) or not isinstance(experience, dict) or not isinstance(intent, dict):
        raise ValueError("checkpoint cognition sections are required")
    cognition = {
        "schema": "anchor.cognition.v3",
        "situation": {"current_understanding": _text(value.get("current_understanding", situation.get("current_understanding")), "cognition.situation.current_understanding")},
        "experience": {},
        "intent": {"current_directive": _text(value.get("current_directive", intent.get("current_directive")), "cognition.intent.current_directive"), "accepted_next_action": _text(value.get("accepted_next_action", intent.get("accepted_next_action")), "cognition.intent.accepted_next_action"), "next_plan": _strings(intent.get("next_plan"), "cognition.intent.next_plan")},
        "knowledge_index": _references(value.get("knowledge_index", [])),
    }
    if not cognition["intent"]["next_plan"]:
        raise ValueError("cognition.intent.next_plan must not be empty")
    for section, field in ITEM_GROUPS:
        target = cognition.setdefault(section, {})
        target[field] = _items((situation if section == "situation" else experience if section == "experience" else intent).get(field, []), f"cognition.{section}.{field}")
    return cognition, False


def _legacy_cognition(value: dict[str, Any]) -> dict[str, Any]:
    refs = _strings(value.get("evidence_refs", []), "legacy evidence_refs")
    def items(field: str, kind: str) -> list[dict[str, Any]]:
        return [{"id": f"{kind}-{_digest(text)[-12:]}", "statement": text, "sources": refs or ["legacy:checkpoint"], "relevance": kind} for text in _strings(value.get(field, []), f"legacy {field}")]
    return {
        "schema": "anchor.cognition.v3",
        "situation": {"current_understanding": _text(value.get("current_understanding"), "legacy current_understanding"), "confirmed_facts": items("confirmed_facts", "confirmed_facts"), "active_hypotheses": items("active_hypotheses", "active_hypotheses"), "unresolved_conflicts": items("unresolved_conflicts", "unresolved_conflicts"), "blockers": items("blockers", "blockers")},
        "experience": {"decisions": items("decisions", "decisions"), "failed_paths": items("failed_paths", "failed_paths")},
        "intent": {"current_directive": _text(value.get("current_directive"), "legacy current_directive"), "accepted_next_action": _text(value.get("accepted_next_action"), "legacy accepted_next_action"), "next_plan": _strings(value.get("next_plan"), "legacy next_plan"), "open_questions": items("open_questions", "open_questions")},
        "knowledge_index": [{"id": f"ref-{_digest(ref)[-12:]}", "cue": "Legacy evidence reference", "locator": ref, "source": ref} for ref in refs],
    }


def _items(value: Any, field: str) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        raise ValueError(f"{field} must be item[]")
    result = []
    for item in value:
        if not isinstance(item, dict):
            raise ValueError(f"{field} must be item[]")
        sources = _strings(item.get("sources"), f"{field}.sources")
        if not sources:
            raise ValueError(f"{field}.sources must not be empty")
        result.append({"id": _text(item.get("id"), f"{field}.id"), "statement": _text(item.get("statement", item.get("text")), f"{field}.statement"), "sources": sources, "relevance": _text(item.get("relevance"), f"{field}.relevance")})
    ids = [item["id"] for item in result]
    if len(ids) != len(set(ids)):
        raise ValueError(f"{field} contains duplicate item ids")
    return result


def _references(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        raise ValueError("cognition.knowledge_index must be reference[]")
    result = []
    for ref in value:
        if not isinstance(ref, dict):
            raise ValueError("cognition.knowledge_index must be reference[]")
        item = {"id": _text(ref.get("id"), "knowledge_index.id"), "cue": _text(ref.get("cue"), "knowledge_index.cue"), "locator": _text(ref.get("locator"), "knowledge_index.locator"), "source": _text(ref.get("source"), "knowledge_index.source")}
        if ref.get("content_hash") is not None:
            item["content_hash"] = _sha256(ref["content_hash"], "knowledge_index.content_hash")
        result.append(item)
    return result


def _normalize_transition(value: Any, cognition: dict[str, Any], *, required: bool) -> dict[str, Any]:
    if value is None:
        if required:
            raise ValueError("checkpoint transition_certificate is required")
        return {"schema": "anchor.transition.v1", "dispositions": []}
    if not isinstance(value, dict) or value.get("schema") != "anchor.transition.v1" or not isinstance(value.get("dispositions"), list):
        raise ValueError("checkpoint transition_certificate schema is invalid")
    seen: set[str] = set()
    next_ids = {item["id"] for section, field in ITEM_GROUPS for item in cognition[section][field]}
    result = []
    for entry in value["dispositions"]:
        if not isinstance(entry, dict) or entry.get("item_id") in seen:
            raise ValueError("transition disposition coverage is invalid")
        item_id = _text(entry.get("item_id"), "transition.item_id")
        seen.add(item_id)
        disposition = entry.get("disposition")
        if disposition not in {"carry", "revise", "resolve", "supersede", "demote", "archive"}:
            raise ValueError("transition disposition is invalid")
        item = {"item_id": item_id, "disposition": disposition, "reason": _text(entry.get("reason"), "transition.reason"), "sources": _strings(entry.get("sources"), "transition.sources")}
        if not item["sources"]:
            raise ValueError("transition.sources must not be empty")
        if disposition in {"revise", "supersede"}:
            item["replacement_id"] = _text(entry.get("replacement_id"), "transition.replacement_id")
            if item["replacement_id"] not in next_ids:
                raise ValueError("transition replacement is missing")
        if disposition == "demote":
            item["reference"] = _text(entry.get("reference"), "transition.reference")
            if re.fullmatch(r"checkpoint:\d+:item:[^:]+", item["reference"]) is None:
                raise ValueError("transition.reference must identify an immutable Checkpoint item")
        result.append(item)
    return {"schema": "anchor.transition.v1", "dispositions": result}


def _validate_transition(certificate: dict[str, Any], previous: dict[str, Any]) -> None:
    previous_ids = {item["id"] for section, field in ITEM_GROUPS for item in previous[section][field]}
    covered = {entry["item_id"] for entry in certificate["dispositions"]}
    if previous_ids != covered:
        raise ConflictError("transition certificate does not cover the previous active cognition")


def _validate_recall_references(conn: sqlite3.Connection, task_id: str, certificate: dict[str, Any], cognition: dict[str, Any]) -> None:
    locators = {reference["locator"] for reference in cognition["knowledge_index"]}
    for entry in certificate["dispositions"]:
        if entry["disposition"] != "demote":
            continue
        reference = entry["reference"]
        if reference not in locators:
            raise ConflictError("demoted item is missing from the Knowledge Index")
        match = re.fullmatch(r"checkpoint:(\d+):item:([^:]+)", reference)
        row = conn.execute(
            "SELECT * FROM checkpoints WHERE task_id=? AND checkpoint_version=?",
            (task_id, int(match.group(1))),
        ).fetchone()
        if row is None:
            raise ConflictError("demotion Checkpoint does not exist")
        payload = _verified_payload(row)
        if not any(item["id"] == match.group(2) for section, field in ITEM_GROUPS for item in payload["cognition"][section][field]):
            raise ConflictError("demotion Checkpoint item does not exist")


def _carry_transition(previous: dict[str, Any], next_cognition: dict[str, Any]) -> dict[str, Any]:
    next_ids = {item["id"] for section, field in ITEM_GROUPS for item in next_cognition[section][field]}
    dispositions = []
    for section, field in ITEM_GROUPS:
        for item in previous[section][field]:
            active = item["id"] in next_ids
            dispositions.append({"item_id": item["id"], "disposition": "carry" if active else "archive", "reason": "Compatibility transition generated by the durable boundary.", "sources": ["compatibility:checkpoint"]})
    return {"schema": "anchor.transition.v1", "dispositions": dispositions}


def _normalize_provenance(value: Any, kind: str) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("kind") != kind:
        raise ValueError("checkpoint provenance.kind must match frontier.kind")
    provenance = {"kind": kind, "model": _text(value.get("model"), "checkpoint provenance.model")}
    if value.get("confirmed_by") is not None:
        provenance["confirmed_by"] = _text(value["confirmed_by"], "checkpoint provenance.confirmed_by")
    if kind == "planning" and provenance.get("confirmed_by") != "user":
        raise ValueError("planning Checkpoint must be confirmed by the user")
    return provenance


def _checkpoint_payload(task_id: str, version: int, parent: dict[str, Any] | None, candidate: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema": "anchor.checkpoint.v1",
        "task_id": task_id,
        "checkpoint_version": version,
        "parent": parent,
        "frontier": candidate["frontier"],
        "cognition": candidate["cognition"],
        "transition_certificate": candidate["transition_certificate"],
        "provenance": candidate["provenance"],
    }


def _checkpoint(row: sqlite3.Row) -> dict[str, Any]:
    payload = _verified_payload(row)
    return {**payload, "receipt": {"event_id": row["event_id"], "content_hash": row["content_hash"]}}


def _event(row: sqlite3.Row | None) -> dict[str, Any]:
    if row is None:
        raise DurableError("committed Checkpoint is missing")
    payload = _verified_payload(row)
    return {"event_id": row["event_id"], "content_hash": row["content_hash"], "payload": payload}


def _verified_payload(row: sqlite3.Row) -> dict[str, Any]:
    payload = json.loads(row["payload_json"])
    if _digest(payload) != row["content_hash"]:
        raise DurableError(f"Checkpoint content hash mismatch: {row['event_id']}")
    if _digest(payload["frontier"]) != row["frontier_hash"] or not _cognition_hash_matches(payload["cognition"], row["cognition_hash"]):
        raise DurableError(f"Checkpoint index hash mismatch: {row['event_id']}")
    return payload


def _cognition_hash_matches(cognition: dict[str, Any], expected: str) -> bool:
    if _digest(cognition) == expected:
        return True
    if cognition.get("schema") == "anchor.cognition.v3":
        # Compatibility read view for callers that still expect the old flat next_action.
        view = {**cognition, "accepted_next_action": cognition["intent"]["accepted_next_action"]}
        return _digest(view) == expected
    return False


def _strings(value: Any, field: str) -> list[str]:
    if not isinstance(value, list) or any(not isinstance(item, str) or not item.strip() for item in value):
        raise ValueError(f"{field} must be string[]")
    return list(dict.fromkeys(item.strip() for item in value))


def _text(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{field} must be a non-empty string")
    return value.strip()


def _sha256(value: Any, field: str) -> str:
    value = _text(value, field)
    if len(value) != 71 or not value.startswith("sha256:") or any(char not in "0123456789abcdef" for char in value[7:]):
        raise ValueError(f"{field} must be a sha256 digest")
    return value


def _new_id(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex[:12]}"


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _digest(value: Any) -> str:
    return "sha256:" + hashlib.sha256(_json(value).encode()).hexdigest()
