---
name: deepseek-web
description: Search the live web and fetch source documents through DeepSeek while preserving complete, timestamped evidence files.
---

Use `search` for discovery and `fetch` to read the actual source. Search snippets, summaries and ranking are discovery evidence, not proof that the source was read. Even `need_content=true` may return incomplete or empty source content; fetch important originals before making detailed claims.

Both tools require `save_as`, a new JSON file path relative to `/workspace`, for example `evidence/search-models-01.json` or `evidence/source-01.json`. The complete API response, request, actual UTC `fetched_at`, HTTP status and success are saved there. Existing files are never overwritten; use a new evidence path for a retry. Tool responses carry `saved_as` and a bounded preview. When `response_truncated=true`, read the saved file to inspect the full content. Different evidence paths support concurrent calls.

Search accepts a 1-128 character query, optional `count` (1-50, default 10), `ori_question`, `localization` and `need_content` (default false). Use query operators such as `site:`, `lang:zh,en`, `after:YYYY-MM-DD` and `before:YYYY-MM-DD` when useful. Search dates may be missing or describe publication rather than the event itself. Verify dates in the original document, and distinguish publication date, event date and actual observation time.

Fetch accepts an HTTP or HTTPS source URL. Preserve the direct source URL in citations. A failed fetch does not prove the source's claims, and a search excerpt does not substitute for a successful fetch. `ok=false` is an observed failure, never empty successful research. Inspect `http_status` and the saved error before deciding what to do. Do not repeatedly retry 401, 402, 403 or 404. A timeout, 429 or upstream 5xx may justify a limited later retry with a fresh evidence path.

The host configures the endpoint and API credential. Never request, print or copy API credentials into prompts, source files or tool arguments. These tools use the separate DeepSeek Search/Fetch API; selecting a DeepSeek chat model alone does not enable them.
