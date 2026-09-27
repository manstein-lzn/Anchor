from datetime import datetime

import pytest

from anchor.scheduling import next_after, occurrences, validate


def test_local_once_weekly_and_monthly_rules():
    now = datetime(2026, 9, 27, 10)
    once = validate({"type": "once", "at": "2026-09-27T11:30"}, now)
    assert next_after(once, now) == datetime(2026, 9, 27, 11, 30)

    weekly = validate({"type": "weekly", "weekdays": [0, 4], "time": "09:15"}, now)
    assert next_after(weekly, now) == datetime(2026, 9, 28, 9, 15)

    monthly = validate({"type": "monthly", "day": 31, "time": "09:00"}, now)
    assert next_after(monthly, datetime(2026, 9, 30, 10)) == datetime(2026, 10, 31, 9)


def test_interval_occurrences_are_bounded_to_the_requested_local_window():
    rule = validate({"type": "interval", "seconds": 30}, datetime(2026, 1, 1))
    items = list(occurrences(rule, datetime(2026, 1, 1),
                             datetime(2026, 1, 1, 0, 0, 31),
                             datetime(2026, 1, 1, 0, 2)))
    assert items == [datetime(2026, 1, 1, 0, 1), datetime(2026, 1, 1, 0, 1, 30)]


@pytest.mark.parametrize("rule", [
    {"type": "once", "at": "2026-09-27T09:00"},
    {"type": "interval", "seconds": 0},
    {"type": "weekly", "weekdays": [], "time": "09:00"},
    {"type": "monthly", "day": 32, "time": "09:00"},
    {"type": "daily", "time": "09:00+08:00"},
])
def test_invalid_or_past_schedule_rules_are_rejected(rule):
    with pytest.raises(ValueError):
        validate(rule, datetime(2026, 9, 27, 10))
