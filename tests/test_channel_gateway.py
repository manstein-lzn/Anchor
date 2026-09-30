import asyncio
from pathlib import Path

from anchor.channel import ChannelEvent, EventLedger
from plugins.wecom import ws_gateway


def test_event_ledger_deduplicates_completed_events_and_retries_failures(tmp_path: Path):
    ledger = EventLedger(tmp_path / "events.sqlite")
    event = ChannelEvent("wecom", "m1", "alice", "alice", text="hello")

    assert ledger.claim(event)
    assert not ledger.claim(event)
    ledger.fail(event, "temporary")
    assert ledger.claim(event)
    ledger.prepare_reply(event, "answer")
    assert not ledger.claim(event)
    assert ledger.pending_reply(event) == "answer"
    ledger.fail_delivery(event, "timeout")
    assert not ledger.claim(event)
    assert ledger.pending_reply(event) == "answer"
    ledger.complete(event)
    assert ledger.pending_reply(event) is None
    assert not ledger.claim(event)


def test_websocket_entrypoint_loads_dotenv_without_overriding_shell(tmp_path, monkeypatch):
    env_file = tmp_path / ".env"
    env_file.write_text("WECOM_BOT_ID=from-file\nWECOM_BOT_SECRET=secret-from-file\n", encoding="utf-8")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("WECOM_BOT_ID", "from-shell")
    monkeypatch.delenv("WECOM_BOT_SECRET", raising=False)
    loaded = {}

    async def capture_run(self):
        loaded["bot_id"] = ws_gateway._required("WECOM_BOT_ID")
        loaded["secret"] = ws_gateway._required("WECOM_BOT_SECRET")

    monkeypatch.setattr(ws_gateway.WeComWebSocketGateway, "run", capture_run)
    asyncio.run(ws_gateway.main())

    assert loaded == {"bot_id": "from-shell", "secret": "secret-from-file"}
