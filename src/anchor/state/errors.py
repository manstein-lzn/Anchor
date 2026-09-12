class DuplicateEvent(Exception):
    """An event idempotency key was reused with different content."""


class ConcurrencyConflict(Exception):
    """A state transition used a stale revision."""


class FencedAttempt(Exception):
    """The lease was released while its worker was executing, so the attempt is over.

    Distinct from :class:`ConcurrencyConflict` because the caller's response differs. A
    stale revision or a lease owned by somebody else is a bug to report. This is the design
    working: the run failed or was stopped, the fan-out ended the node, and the worker's
    result must not be committed. A worker should stop quietly rather than try to fail a
    node that already has a terminal state, which would raise again and fill the journal
    with a traceback for a run that behaved correctly.
    """


class GraphVersionConflict(Exception):
    """A graph/version identity was reused with different content."""


class AdmissionConflict(Exception):
    """A trigger occurrence key was reused with a different request."""


class OperationConflict(Exception):
    """An operation identity or lifecycle transition conflicts with stored state."""
