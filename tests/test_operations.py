from uuid import uuid4

import pytest

from anchor.domain.operations import OperationStatus, ToolOperation


def operation(**updates):
    values = dict(operation_id=uuid4(), claim_id=uuid4(), node_run_id=uuid4(), run_id=uuid4(),
                  tool_ref="mail.send", arguments={"recipient": "user@example.test"})
    values.update(updates)
    return ToolOperation.register(**values)


def test_request_hash_is_stable_across_argument_order():
    first = ToolOperation.hash_request("tool", {"a": 1, "nested": {"x": True, "y": 2}})
    second = ToolOperation.hash_request("tool", {"nested": {"y": 2, "x": True}, "a": 1})
    assert first == second


def test_operation_rejects_mutated_request_and_incomplete_outcomes():
    item = operation()
    with pytest.raises(ValueError, match="hash"):
        ToolOperation.model_validate(item.model_dump() | {"tool_ref": "changed"})
    with pytest.raises(ValueError, match="result_ref"):
        ToolOperation.model_validate(item.model_dump() | {"status": OperationStatus.SUCCEEDED})
    with pytest.raises(ValueError, match="error_code"):
        ToolOperation.model_validate(item.model_dump() | {"status": OperationStatus.OUTCOME_UNKNOWN})
    with pytest.raises(ValueError, match="cannot claim"):
        ToolOperation.model_validate(item.model_dump() | {
            "status": OperationStatus.OUTCOME_UNKNOWN,
            "error_code": "disconnected",
            "reconciliation_ref": "provider://evidence/1",
        })
