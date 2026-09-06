"""Long-lived interval/cron scheduler process."""
from __future__ import annotations
import asyncio
import logging
from anchor.runtime.scheduler import run_interval_scheduler
from anchor.runtime.settings import AnchorSettings
from anchor.state.relational import RelationalStateStore

def _objective(trigger, now):
    return (f"Scheduled trigger {trigger.id} at {now.isoformat()}", {"trigger_id": str(trigger.id), "scheduled_at": now.isoformat()})

async def serve():
    settings = AnchorSettings()
    url = settings.require_database_url()
    store=RelationalStateStore(url); stop=asyncio.Event()
    try:
        await run_interval_scheduler(store, objective_factory=_objective,
                                     interval=settings.scheduler_interval, stop=stop)
    finally: store.close()

def main():
    logging.basicConfig(level=AnchorSettings().log_level)
    try: asyncio.run(serve())
    except KeyboardInterrupt: logging.getLogger("anchor.scheduler").info("scheduler stopped")

if __name__ == "__main__": main()
