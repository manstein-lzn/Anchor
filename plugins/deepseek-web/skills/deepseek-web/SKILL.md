---
name: deepseek-web
description: Search the live web and fetch source documents through DeepSeek while preserving complete, timestamped evidence files.
---

Discover this invocation's actual tool names with `anchor_tools` (filter `plugin:"deepseek-web"`), read the selected schema with `anchor_tool_detail`, then call the exact returned name with `anchor_tools_call`. The `search` and `fetch` names below describe operations, not names to guess from previous conversation history.

Use `search` for discovery and `fetch` to read the actual source. Search snippets, summaries and ranking are discovery evidence, not proof that the source was read. Even `need_content=true` may return incomplete or empty source content; fetch important originals before making detailed claims.

Both tools require `save_as`, a new JSON file path relative to `/workspace`, for example `evidence/search-models-01.json` or `evidence/source-01.json`. The complete API response, request, actual UTC `fetched_at`, HTTP status and success are saved there. Existing files are never overwritten; use a new evidence path for a retry. Tool responses carry `saved_as` and a bounded preview. When `response_truncated=true`, read the saved file to inspect the full content. Different evidence paths support concurrent calls.

Search accepts a 1-128 character query, optional `count` (1-50, default 10), `ori_question`, `localization` and `need_content` (default false). Use query operators such as `site:`, `lang:zh,en`, `after:YYYY-MM-DD` and `before:YYYY-MM-DD` when useful. Search dates may be missing or describe publication rather than the event itself. Verify dates in the original document, and distinguish publication date, event date and actual observation time.

Fetch accepts an HTTP or HTTPS source URL. Preserve the direct source URL in citations. A failed fetch does not prove the source's claims, and a search excerpt does not substitute for a successful fetch. `ok=false` is an observed failure, never empty successful research. `http_status_scope="service"` means `http_status` came from the Search/Fetch API, not independently from the source server: a service 404 does not prove the source page is missing. Inspect `failure.layer`, `failure.code` and the saved response before deciding what to do. `failure.retryable=true` only identifies a candidate for a limited later retry (timeout, 408, 429 or service 5xx), not an automatic retry or evidence of eventual success. Keep a fresh evidence path for each attempt. Do not repeatedly retry 401, 402, 403 or 404; search for another accessible original or report that this source could not be verified. Other transport errors and invalid responses require inspection, not blind retries. Each tool call sends exactly one API request.

The host configures the endpoint and API credential. Never request, print or copy API credentials into prompts, source files or tool arguments. These tools use the separate DeepSeek Search/Fetch API; selecting a DeepSeek chat model alone does not enable them.
