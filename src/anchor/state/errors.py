class DuplicateEvent(Exception):
    """An event idempotency key was reused with different content."""


class ConcurrencyConflict(Exception):
    """A state transition used a stale revision."""


class GraphVersionConflict(Exception):
    """A graph/version identity was reused with different content."""


class AdmissionConflict(Exception):
    """A trigger occurrence key was reused with a different request."""


class OperationConflict(Exception):
    """An operation identity or lifecycle transition conflicts with stored state."""
