# AGENTS.md

## Project purpose

An [OpenAction](https://openaction.amankhanna.me) plugin for [OpenDeck](https://github.com/nekename/OpenDeck) that shows local [Claude Code](https://claude.com/product/claude-code) usage on a stream-deck-style key. Personal fork of an OpenRouter-credits plugin, migrated to Claude — see "Why local files, not an API" below for the reasoning.

Three actions read the same transcript files Claude Code writes to `~/.claude/projects/**/*.jsonl` on this machine and total up token usage / estimated cost over a time window:

- `com.achoobert.claudeusage.today` — today's estimated USD cost.
- `com.achoobert.claudeusage.month` — month-to-date estimated USD cost.
- `com.achoobert.claudeusage.tokens` — today's total token count.

## Why local files, not an API

Anthropic has no public "remaining balance" endpoint (confirmed via search when this was migrated from OpenRouter — the Console shows it, there's no API). The Admin `usage_report`/`cost_report` API exists but needs an org-admin key (`sk-ant-admin...`), which most individual users don't have. The user this plugin was built for is not an org admin, but does run Claude Code on the same machine as OpenDeck — so this plugin reads Claude Code's own local transcript files instead of calling any API. No key, no network call, no admin access needed; the tradeoff is it only sees usage that happened on this machine, and costs are an estimate from a hardcoded pricing table (see below), not a real invoice.

## Architecture

- **Rust backend** (`src/main.rs`) using the official `openaction` crate (2.x). This is the recommended OpenAction language. Do not rewrite in another language unless asked.
- **Three actions**, one struct each (`TodayAction`, `MonthAction`, `TokensAction`), sharing a common `render_metric` helper. `assets/manifest.json` lists all three under the `Claude` category.
- **Property inspector** (`assets/pi.html`): static text only, no settings. There's nothing to configure — no API key, no per-instance state.
- **Data source**: `~/.claude/projects/<project>/<session>.jsonl` — Claude Code's own transcript files. `dirs::home_dir()` locates the home dir cross-platform. Each JSONL line is one event; usage-bearing lines have `message.model` and `message.usage.{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}` plus a top-level `timestamp` (RFC3339 UTC). Most lines are other event types (tool calls, meta, attachments) and are skipped — `parse_usage_line` returns `None` for anything that doesn't parse as a usage event, not an error.
- **Perf shortcut**: before opening a transcript file, `scan_usage` checks its mtime against the window start (`since`) and skips it if the file hasn't been touched since — an append-only transcript with no recent writes can't contain rows inside the window.
- **Pricing table** (`model_pricing`): a hardcoded `$/MTok` (input, output) table keyed by model id, covering current Claude models plus common legacy/dated ids that might still appear in older transcripts. Unknown models contribute to the token total but $0 to cost (logged at debug level) rather than erroring — new models will need a new table entry to price correctly. Cache tokens are billed at the standard multiples of the input rate: `cache_creation_input_tokens` × 1.25, `cache_read_input_tokens` × 0.1 (`CACHE_WRITE_MULTIPLIER`/`CACHE_READ_MULTIPLIER`).
- **Rendering**: unchanged from the OpenRouter version — an SVG string sent to the key via `instance.set_image(...)` as a base64 data URI. `render_svg(label, amount)` now takes the label as a parameter (`"TODAY"`/`"MONTH"`/`"TOKENS"`) instead of a hardcoded `"CREDITS"`.

## How it stays live

- A `tokio::spawn` background task ticks every 60s (`tick_instances`), which calls `tick_action` once per action UUID.
- `tick_action` fetches `visible_instances(uuid)` and skips entirely if none are visible — no filesystem scan happens for a metric no key is currently showing.
- Usage totals are cached **per `Metric`** (`Today`/`Month`), not per key instance, since multiple keys can show the same window (e.g. "Today's Spend" and "Today's Tokens" both read `Metric::Today`) — the first `render_metric` call in a tick repopulates the shared cache; the rest hit it. `REFETCH_INTERVAL` (5 min) governs staleness.
- Pressing a key forces an immediate rescan (`force: true` in `get_usage`), bypassing the cache.

## Key code conventions and gotchas

- **`INSTANCES`-style per-instance state is gone.** The OpenRouter version tracked per-instance API keys in a `Mutex<HashMap<InstanceId, InstanceState>>`. This version has no per-instance settings, so the cache is keyed by `Metric` instead — simpler, and correctly shares one scan across keys showing the same window.
- **`std::sync::MutexGuard` is not `Send`**: still applies to `USAGE_CACHE`. Don't hold the lock across an `.await` — the existing code only touches it inside a scoped block before or after the async filesystem work.
- **SVG format strings**: use `r##"..."##` raw strings, never `r#"..."#`. The hex colors (`fill="#ffffff"`) contain the byte sequence `"#` which prematurely terminates `r#"..."#` and causes confusing `prefix ... is unknown` compile errors.
- **Truncate error text char-safely**: use `message.chars().take(16).collect()`, not `&message[..16]` (panics on multi-byte boundaries).
- **Timestamps are UTC, windows are local.** `start_of_today`/`start_of_month` compute local-time boundaries (`chrono::Local`) then convert to UTC for comparison against transcript timestamps, so "today" means the user's local calendar day, not UTC's.
- **Pure helpers are testable**: `parse_usage_line`, `model_pricing`, `estimate_cost`, `format_currency`, `format_tokens`, `render_svg*`, `to_data_uri` are side-effect free. Add unit tests for any changes to them — especially new pricing table entries.
- Tabs for indentation (matches the `openaction` crate examples and this repo).
- **Makefile arch mapping**: `uname -m` on Apple Silicon prints `arm64`, but Rust/`CodePaths` target triples use `aarch64`. The `Makefile` remaps this (`ARCH := $(patsubst arm64,aarch64,$(UNAME_M))`) — don't revert to a bare `uname -m` triple or local `make install` produces a binary name OpenDeck can't find on Apple Silicon.

## Commands

```sh
cargo build --release     # build
cargo test --release      # unit tests (parsing/pricing/formatting logic)
cargo clippy --release    # lint; must be clean before finishing
make stage                # assemble com.achoobert.claudeusage.sdPlugin/
make package               # stage + zip com.achoobert.claudeusage.zip
make install               # stage + copy into OpenDeck plugins dir
make clean                 # remove build artifacts, staged plugin dir, and package zip
```

OpenDeck plugins directory: Linux `~/.config/opendeck/plugins/` (or `$XDG_CONFIG_HOME/opendeck/plugins/`), macOS `~/Library/Application Support/OpenDeck/plugins/` (case-insensitive on APFS, same as `~/Library/Application Support/opendeck/plugins/`).

## Distribution notes

This is a personal local fork — it is **not** published to the OpenAction marketplace, and the bundle id (`com.achoobert.claudeusage`) is not registered in the `OpenActionAPI/plugins` catalogue. The `.github/workflows/*.yaml` files inherited from the OpenRouter version (PR-label-driven release automation, multi-platform binary builds, marketplace catalogue sync) are out of scope for this fork's goal (local install only) and haven't been updated to match the new bundle id/package name — treat them as unmaintained until/unless this plugin is ever published for real.

## Docs maintenance

Keep `AGENTS.md` and `README.md` accurate and up to date automatically. When you change behavior, commands, dependencies, settings, or architecture, update the relevant section of both files in the same change. Do not wait to be asked.
