# Channel Transport Primitives

This neutral crate replaces the ledger, private-state, Host HTTP callback,
receipt validation, settlement retry worker and private control primitives that
previously lived in `anchor-wecom-gateway`. WeCom keeps platform framing, media
parsing and delivery orchestration; Feishu owns its own corresponding protocol.
Neither adapter depends on the other adapter's business crate.

The extraction preserves the existing SQLite schema version 5, event JSON
fields and WeCom delivery identities. A separately leased, profile-bound state
directory belongs to each adapter instance; this is not a new store of Host or
Kernel product facts. Existing WeCom transport tests exercise the moved ledger
and filesystem invariants. Feishu fixtures cover the new protocol boundary.
