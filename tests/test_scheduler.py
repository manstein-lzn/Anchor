from datetime import datetime, timezone
from uuid import uuid4
from anchor.runtime.scheduler import cron_slot, due_slot, occurrence_key

def test_interval_occurrence_is_deterministic():
    now=datetime(2026,1,1,tzinfo=timezone.utc)
    assert due_slot(now,60)==int(now.timestamp())//60
    trigger=uuid4(); assert occurrence_key(trigger,3)==f"schedule:{trigger}:3"

def test_cron_occurrence_is_stable():
    now=datetime(2026,1,1,12,34,56,tzinfo=timezone.utc)
    assert cron_slot(now, "*/5 * * * *") == int(datetime(2026,1,1,12,30,tzinfo=timezone.utc).timestamp())
