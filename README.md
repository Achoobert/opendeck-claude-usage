# Claude Usage OpenDeck plugin

An [OpenAction](https://openaction.amankhanna.me) plugin for [OpenDeck](https://github.com/nekename/OpenDeck) that shows your local [Claude Code](https://claude.com/product/claude-code) usage. The plugin is listed as **Claude Usage** in OpenDeck, under the `Claude` category, and offers three actions:

- **Today's Spend** — estimated USD cost of today's local Claude Code usage.
- **Month-to-Date Spend** — estimated USD cost of this month's local Claude Code usage.
- **Today's Tokens** — total tokens (input + output + cache) used today.

Each is a separate draggable action, so you can put any combination of them on your deck.

## How it works

Anthropic doesn't expose a "remaining balance" API (the Console is the only place that shows it), and the Admin usage/cost report API requires an org-admin key most individual users don't have. Instead, this plugin reads the same local transcript files Claude Code itself writes to `~/.claude/projects/**/*.jsonl` on this machine, sums the `usage` block of each assistant turn (`input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`) within the relevant time window, and estimates a dollar cost from a hardcoded per-model pricing table.

**This is an estimate**, not a billing reconciliation — it only sees local transcripts on this machine and a pricing table that can go stale as Anthropic ships new models or pricing changes. It requires no API key and makes no network calls.

The scan skips any transcript file whose modification time predates the window it's computing (today or this month), so only recently-active sessions are actually parsed.

## Requirements

- Rust (stable) to build.
- Claude Code used on the same machine OpenDeck runs on (reads `~/.claude/projects`).

## Build & install

```sh
make install
```

This builds the plugin, stages it into `com.achoobert.claudeusage.sdPlugin/`, and copies it into your OpenDeck plugins directory:

- Linux: `~/.config/opendeck/plugins/` (or `$XDG_CONFIG_HOME/opendeck/plugins/`)
- macOS: `~/Library/Application Support/OpenDeck/plugins/`

Restart OpenDeck (or reload plugins) and add **Today's Spend**, **Month-to-Date Spend**, or **Today's Tokens** from the `Claude` category.

### Manual install

Run `make stage` to produce `com.achoobert.claudeusage.sdPlugin/`, then copy that folder into your OpenDeck plugins directory (found via **Open config directory** in OpenDeck settings → `plugins/`).

## Packaging

Run `make package` to assemble the plugin bundle and zip it into `com.achoobert.claudeusage.zip`. This is a personal local build — there's no marketplace publishing pipeline set up for it.

## Configuration

None. There's no property inspector setting — the plugin just reads `~/.claude` on the machine it runs on. Press a key to force an immediate refresh; otherwise each key refreshes on a 5-minute cycle.

## Development

```sh
cargo test       # run the parsing/pricing/formatting unit tests
make stage       # build and assemble the .sdPlugin directory
cargo clippy     # lint
```
