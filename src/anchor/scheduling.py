"""Local-time recurrence rules for the single-process Graph scheduler."""

from __future__ import annotations

from datetime import datetime, time, timedelta


def validate(rule: dict, now: datetime) -> dict:
    kind = rule.get("type")
    if kind == "once":
        try:
            at = datetime.fromisoformat(rule["at"])
        except (KeyError, TypeError, ValueError) as exc:
            raise ValueError("once.at must be a future local datetime") from exc
        if at.tzinfo is not None or at <= now:
            raise ValueError("once.at must be a future local datetime without a timezone")
        return {"type": kind, "at": at.isoformat(timespec="seconds")}
    if kind == "interval":
        seconds = rule.get("seconds")
        if type(seconds) is not int or seconds < 1:
            raise ValueError("interval.seconds must be a positive integer")
        return {"type": kind, "seconds": seconds}
    try:
        at = time.fromisoformat(rule["time"])
        if at.tzinfo is not None or at.second or at.microsecond:
            raise ValueError("time must use local HH:MM precision")
        normalized = {"type": kind, "time": at.strftime("%H:%M")}
        if kind == "daily":
            return normalized
        if kind == "weekly":
            days = rule.get("weekdays")
            if (not isinstance(days, list) or not days or
                    any(type(day) is not int or day not in range(7) for day in days)):
                raise ValueError("weekly.weekdays must contain weekdays from 0 (Monday) to 6")
            return {**normalized, "weekdays": sorted(set(days))}
        if kind == "monthly":
            day = rule.get("day")
            if type(day) is not int or day not in range(1, 32):
                raise ValueError("monthly.day must be between 1 and 31")
            return {**normalized, "day": day}
    except (KeyError, TypeError, ValueError) as exc:
        raise ValueError(str(exc) or "time must use HH:MM") from exc
    raise ValueError("type must be once, interval, daily, weekly, or monthly")


def next_after(rule: dict, after: datetime) -> datetime:
    """The first local occurrence strictly after `after`; nonexistent monthly dates are skipped."""
    kind = rule["type"]
    if kind == "once":
        return datetime.fromisoformat(rule["at"])
    if kind == "interval":
        return after + timedelta(seconds=rule["seconds"])
    target = time.fromisoformat(rule["time"])
    day = after.date()
    for offset in range(367):
        candidate_day = day + timedelta(days=offset)
        if kind == "weekly" and candidate_day.weekday() not in rule["weekdays"]:
            continue
        if kind == "monthly":
            if candidate_day.day != rule["day"]:
                continue
        candidate = datetime.combine(candidate_day, target)
        if candidate > after:
            return candidate
    raise ValueError("no occurrence in the next year")


def occurrences(rule: dict, created: datetime, start: datetime, end: datetime):
    """Yield scheduled local datetimes in `[start, end)`."""
    if rule["type"] == "once":
        at = datetime.fromisoformat(rule["at"])
        if start <= at < end:
            yield at
        return
    if rule["type"] == "interval":
        step = timedelta(seconds=rule["seconds"])
        first = created + step
        cursor = start
        while cursor < end:
            day_end = min(datetime.combine(cursor.date() + timedelta(days=1), time.min), end)
            candidate = first
            if candidate < cursor:
                count = (cursor - candidate + step - timedelta(microseconds=1)) // step
                candidate += step * count
            while candidate < day_end:
                yield candidate
                candidate += step
            cursor = day_end
        return
    current = next_after(rule, start - timedelta(seconds=1))
    while current < end:
        if current >= created:
            yield current
        current = next_after(rule, current)
