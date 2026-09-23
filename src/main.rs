use openaction::*;

use base64::Engine;
use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long a cached usage total stays fresh before a tick / key-press refetches it.
const REFETCH_INTERVAL: Duration = Duration::from_secs(300);
/// How often the ticker wakes up to look for stale metrics.
const TICK_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize, Clone, Default)]
struct EmptySettings;

/// Which usage window a key is showing. Shared across all instances of the
/// same metric so they don't each rescan the filesystem independently.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Metric {
	Today,
	Month,
}

#[derive(Clone, Copy)]
enum RenderMode {
	Cost,
	Tokens,
}

#[derive(Clone, Copy, Default)]
struct UsageTotals {
	tokens: u64,
	cost: f64,
}

#[derive(Default)]
struct CachedUsage {
	totals: UsageTotals,
	last_fetch: Option<Instant>,
}

static USAGE_CACHE: LazyLock<Mutex<HashMap<Metric, CachedUsage>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUsage {
	input_tokens: u64,
	output_tokens: u64,
	cache_creation_input_tokens: u64,
	cache_read_input_tokens: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMessage {
	model: Option<String>,
	usage: Option<RawUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLine {
	timestamp: Option<String>,
	message: Option<RawMessage>,
}

struct UsageEntry {
	timestamp: DateTime<Utc>,
	model: String,
	usage: RawUsage,
}

/// Parse one line of a Claude Code transcript (`~/.claude/projects/**/*.jsonl`).
/// Most lines aren't assistant-usage events (tool calls, meta, attachments, ...);
/// those return `None` rather than an error.
fn parse_usage_line(line: &str) -> Option<UsageEntry> {
	let raw: RawLine = serde_json::from_str(line).ok()?;
	let timestamp = raw.timestamp?.parse::<DateTime<Utc>>().ok()?;
	let message = raw.message?;
	let usage = message.usage?;
	Some(UsageEntry {
		timestamp,
		model: message.model.unwrap_or_default(),
		usage,
	})
}

/// Estimated $/MTok (input, output) per model. This is an approximation for a
/// personal usage gauge, not a billing reconciliation — actual invoices may
/// reflect tiering or promotional pricing this table doesn't know about.
fn model_pricing(model: &str) -> Option<(f64, f64)> {
	if model == "claude-fable-5" || model == "claude-mythos-5" {
		return Some((10.0, 50.0));
	}
	if model == "claude-opus-5"
		|| model == "claude-opus-4-8"
		|| model == "claude-opus-4-7"
		|| model == "claude-opus-4-6"
		|| model.starts_with("claude-opus-4-5")
	{
		return Some((5.0, 25.0));
	}
	if model == "claude-sonnet-5" || model == "claude-sonnet-4-6" || model.starts_with("claude-sonnet-4-5") {
		return Some((3.0, 15.0));
	}
	if model.starts_with("claude-haiku-4-5") {
		return Some((1.0, 5.0));
	}
	if model.starts_with("claude-opus-4-1") || model.starts_with("claude-opus-4-0") || model.starts_with("claude-opus-4-20") {
		return Some((15.0, 75.0));
	}
	if model.starts_with("claude-sonnet-4-0") || model.starts_with("claude-sonnet-4-20") {
		return Some((3.0, 15.0));
	}
	if model.starts_with("claude-3-5-sonnet") {
		return Some((3.0, 15.0));
	}
	if model.starts_with("claude-3-opus") {
		return Some((15.0, 75.0));
	}
	if model.starts_with("claude-3-5-haiku") {
		return Some((0.8, 4.0));
	}
	if model.starts_with("claude-3-haiku") {
		return Some((0.25, 1.25));
	}
	None
}

/// Cache write/read tokens are billed at multiples of the base input rate.
const CACHE_WRITE_MULTIPLIER: f64 = 1.25;
const CACHE_READ_MULTIPLIER: f64 = 0.1;

fn estimate_cost(model: &str, usage: &RawUsage) -> f64 {
	let Some((input_rate, output_rate)) = model_pricing(model) else {
		log::debug!("estimate_cost: unknown model '{}', counting tokens but not cost", model);
		return 0.0;
	};
	let per_tok = |tokens: u64, rate: f64| tokens as f64 / 1_000_000.0 * rate;
	per_tok(usage.input_tokens, input_rate)
		+ per_tok(usage.output_tokens, output_rate)
		+ per_tok(usage.cache_creation_input_tokens, input_rate * CACHE_WRITE_MULTIPLIER)
		+ per_tok(usage.cache_read_input_tokens, input_rate * CACHE_READ_MULTIPLIER)
}

fn claude_projects_dir() -> Option<PathBuf> {
	dirs::home_dir().map(|home| home.join(".claude").join("projects"))
}

fn accumulate_file(path: &Path, since: DateTime<Utc>, totals: &mut UsageTotals) {
	let Ok(file) = fs::File::open(path) else {
		return;
	};
	for line in BufReader::new(file).lines().map_while(Result::ok) {
		let Some(entry) = parse_usage_line(&line) else {
			continue;
		};
		if entry.timestamp < since {
			continue;
		}
		totals.tokens += entry.usage.input_tokens
			+ entry.usage.output_tokens
			+ entry.usage.cache_creation_input_tokens
			+ entry.usage.cache_read_input_tokens;
		totals.cost += estimate_cost(&entry.model, &entry.usage);
	}
}

/// Scan every session transcript under `~/.claude/projects` and total up usage
/// since `since`. Transcripts are append-only, so any file whose mtime is
/// older than `since` cannot contain rows inside the window and is skipped
/// without being opened.
fn scan_usage(since: DateTime<Utc>) -> Result<UsageTotals, String> {
	let root = claude_projects_dir().ok_or_else(|| "no home dir".to_string())?;
	let mut totals = UsageTotals::default();
	let Ok(project_dirs) = fs::read_dir(&root) else {
		// No Claude Code data yet on this machine — that's a valid (empty) state.
		return Ok(totals);
	};
	for project in project_dirs.flatten() {
		let project_path = project.path();
		if !project_path.is_dir() {
			continue;
		}
		let Ok(files) = fs::read_dir(&project_path) else {
			continue;
		};
		for file in files.flatten() {
			let file_path = file.path();
			if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
				continue;
			}
			let Ok(metadata) = file.metadata() else {
				continue;
			};
			let Ok(modified) = metadata.modified() else {
				continue;
			};
			let modified: DateTime<Utc> = modified.into();
			if modified < since {
				continue;
			}
			accumulate_file(&file_path, since, &mut totals);
		}
	}
	Ok(totals)
}

fn start_of_today() -> DateTime<Utc> {
	let midnight = Local::now().date_naive().and_hms_opt(0, 0, 0).expect("midnight is valid");
	Local.from_local_datetime(&midnight).single().expect("unambiguous local midnight").with_timezone(&Utc)
}

fn start_of_month() -> DateTime<Utc> {
	let first_of_month = Local::now()
		.date_naive()
		.with_day(1)
		.expect("day 1 is valid")
		.and_hms_opt(0, 0, 0)
		.expect("midnight is valid");
	Local
		.from_local_datetime(&first_of_month)
		.single()
		.expect("unambiguous local month start")
		.with_timezone(&Utc)
}

/// Return the cached total for `metric`, refetching from disk if it's stale
/// (or `force` is set, e.g. from a manual key press).
fn get_usage(metric: Metric, force: bool) -> Result<UsageTotals, String> {
	let now = Instant::now();
	{
		let cache = USAGE_CACHE.lock().unwrap();
		if let Some(cached) = cache.get(&metric) {
			let fresh = cached.last_fetch.map(|t| now.duration_since(t) < REFETCH_INTERVAL).unwrap_or(false);
			if fresh && !force {
				return Ok(cached.totals);
			}
		}
	}

	let since = match metric {
		Metric::Today => start_of_today(),
		Metric::Month => start_of_month(),
	};
	let totals = scan_usage(since)?;
	USAGE_CACHE.lock().unwrap().insert(
		metric,
		CachedUsage {
			totals,
			last_fetch: Some(now),
		},
	);
	Ok(totals)
}

fn format_currency(value: f64) -> String {
	format!("${:.2}", value)
}

fn format_tokens(tokens: u64) -> String {
	let n = tokens as f64;
	if tokens >= 1_000_000 {
		format!("{:.1}M", n / 1_000_000.0)
	} else if tokens >= 1_000 {
		format!("{:.1}K", n / 1_000.0)
	} else {
		tokens.to_string()
	}
}

fn render_svg(label: &str, amount: &str) -> String {
	format!(
		r##"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">
  <text x="72" y="62" text-anchor="middle" fill="#9a9a9a" font-family="sans-serif" font-size="12" font-weight="600">{label}</text>
  <text x="72" y="98" text-anchor="middle" fill="#2ecc71" font-family="sans-serif" font-size="26" font-weight="bold">{amount}</text>
</svg>"##,
		label = label,
		amount = amount,
	)
}

fn render_svg_error(message: &str) -> String {
	let short: String = message.chars().take(16).collect();
	format!(
		r##"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">
  <text x="72" y="76" text-anchor="middle" fill="#e74c3c" font-family="sans-serif" font-size="18" font-weight="bold">err</text>
  <text x="72" y="98" text-anchor="middle" fill="#9a9a9a" font-family="sans-serif" font-size="12">{short}</text>
</svg>"##,
		short = short,
	)
}

fn to_data_uri(svg: &str) -> String {
	let b64 = base64::engine::general_purpose::STANDARD.encode(svg.as_bytes());
	format!("data:image/svg+xml;base64,{}", b64)
}

async fn send_image(instance: &Instance, svg: &str, label: &str) {
	let uri = to_data_uri(svg);
	log::debug!("[{}] sending {label} image (uri len {})", instance.instance_id, uri.len());
	if let Err(e) = instance.set_image(Some(uri), None).await {
		log::error!("[{}] set_image failed: {}", instance.instance_id, e);
	}
}

async fn render_metric(instance: &Instance, metric: Metric, mode: RenderMode, label: &str, force: bool) {
	match get_usage(metric, force) {
		Ok(totals) => {
			let amount = match mode {
				RenderMode::Cost => format_currency(totals.cost),
				RenderMode::Tokens => format_tokens(totals.tokens),
			};
			send_image(instance, &render_svg(label, &amount), label).await;
		}
		Err(e) => {
			log::warn!("[{}] render_metric({label}): {}", instance.instance_id, e);
			send_image(instance, &render_svg_error(&e), label).await;
		}
	}
}

struct TodayAction;

#[async_trait]
impl Action for TodayAction {
	const UUID: ActionUuid = "com.achoobert.claudeusage.today";
	type Settings = EmptySettings;

	async fn will_appear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
		render_metric(instance, Metric::Today, RenderMode::Cost, "TODAY", false).await;
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
		render_metric(instance, Metric::Today, RenderMode::Cost, "TODAY", true).await;
		Ok(())
	}
}

struct MonthAction;

#[async_trait]
impl Action for MonthAction {
	const UUID: ActionUuid = "com.achoobert.claudeusage.month";
	type Settings = EmptySettings;

	async fn will_appear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
		render_metric(instance, Metric::Month, RenderMode::Cost, "MONTH", false).await;
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
		render_metric(instance, Metric::Month, RenderMode::Cost, "MONTH", true).await;
		Ok(())
	}
}

struct TokensAction;

#[async_trait]
impl Action for TokensAction {
	const UUID: ActionUuid = "com.achoobert.claudeusage.tokens";
	type Settings = EmptySettings;

	async fn will_appear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
		render_metric(instance, Metric::Today, RenderMode::Tokens, "TOKENS", false).await;
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
		render_metric(instance, Metric::Today, RenderMode::Tokens, "TOKENS", true).await;
		Ok(())
	}
}

/// Refresh + rebroadcast one action's metric to every currently visible
/// instance of it. Skipped entirely if no key is showing that action, and the
/// underlying scan only happens once per tick even if multiple keys share the
/// same metric (e.g. Today $ and Tokens both read `Metric::Today`), since the
/// first `render_metric` call repopulates the shared cache for the rest.
async fn tick_action(uuid: ActionUuid, metric: Metric, mode: RenderMode, label: &str) {
	let visible = visible_instances(uuid).await;
	for instance in visible {
		render_metric(&instance, metric, mode, label, false).await;
	}
}

async fn tick_instances() {
	tick_action(TodayAction::UUID, Metric::Today, RenderMode::Cost, "TODAY").await;
	tick_action(MonthAction::UUID, Metric::Month, RenderMode::Cost, "MONTH").await;
	tick_action(TokensAction::UUID, Metric::Today, RenderMode::Tokens, "TOKENS").await;
}

#[tokio::main]
async fn main() -> OpenActionResult<()> {
	{
		use simplelog::*;
		if let Err(error) = TermLogger::init(
			LevelFilter::Debug,
			Config::default(),
			TerminalMode::Stdout,
			ColorChoice::Never,
		) {
			eprintln!("Logger initialization failed: {}", error);
		}
	}

	register_action(TodayAction).await;
	register_action(MonthAction).await;
	register_action(TokensAction).await;

	let ticker = tokio::spawn(async {
		log::debug!("background ticker started (60s interval)");
		let mut interval = tokio::time::interval(TICK_INTERVAL);
		loop {
			interval.tick().await;
			tick_instances().await;
		}
	});

	let result = run(std::env::args().collect()).await;
	ticker.abort();
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn currency_formats_two_decimals() {
		assert_eq!(format_currency(74.75), "$74.75");
		assert_eq!(format_currency(100.5), "$100.50");
		assert_eq!(format_currency(0.0), "$0.00");
	}

	#[test]
	fn tokens_format_with_suffix() {
		assert_eq!(format_tokens(42), "42");
		assert_eq!(format_tokens(1_234), "1.2K");
		assert_eq!(format_tokens(2_500_000), "2.5M");
	}

	#[test]
	fn render_shows_label_and_amount() {
		let svg = render_svg("TODAY", "$74.75");
		assert!(svg.contains("$74.75"));
		assert!(svg.contains("TODAY"));
	}

	#[test]
	fn data_uri_is_svg() {
		let uri = to_data_uri(&render_svg("TODAY", "$0.00"));
		assert!(uri.starts_with("data:image/svg+xml;base64,"));
	}

	#[test]
	fn error_truncation_is_char_safe() {
		let svg = render_svg_error("this is a very long message that should be truncated!");
		assert!(svg.contains("this is a very l"));
	}

	#[test]
	fn pricing_known_current_model() {
		assert_eq!(model_pricing("claude-sonnet-5"), Some((3.0, 15.0)));
		assert_eq!(model_pricing("claude-opus-5"), Some((5.0, 25.0)));
	}

	#[test]
	fn pricing_matches_dated_legacy_ids() {
		assert_eq!(model_pricing("claude-3-5-sonnet-20241022"), Some((3.0, 15.0)));
	}

	#[test]
	fn pricing_unknown_model_is_none() {
		assert_eq!(model_pricing("some-future-model"), None);
	}

	#[test]
	fn estimate_cost_applies_cache_multipliers() {
		let usage = RawUsage {
			input_tokens: 1_000_000,
			output_tokens: 0,
			cache_creation_input_tokens: 1_000_000,
			cache_read_input_tokens: 1_000_000,
		};
		// sonnet-5: $3 input, cache write 1.25x = $3.75, cache read 0.1x = $0.30
		let cost = estimate_cost("claude-sonnet-5", &usage);
		assert!((cost - (3.0 + 3.75 + 0.30)).abs() < 1e-9);
	}

	#[test]
	fn estimate_cost_unknown_model_is_zero() {
		let usage = RawUsage {
			input_tokens: 1_000,
			output_tokens: 1_000,
			cache_creation_input_tokens: 0,
			cache_read_input_tokens: 0,
		};
		assert_eq!(estimate_cost("some-future-model", &usage), 0.0);
	}

	#[test]
	fn parses_sample_transcript_line() {
		let line = r#"{"timestamp":"2026-09-11T03:23:28.316Z","message":{"model":"claude-sonnet-5","usage":{"input_tokens":2,"cache_creation_input_tokens":2012,"cache_read_input_tokens":57396,"output_tokens":150}}}"#;
		let entry = parse_usage_line(line).expect("line should parse");
		assert_eq!(entry.model, "claude-sonnet-5");
		assert_eq!(entry.usage.output_tokens, 150);
		assert_eq!(entry.usage.cache_read_input_tokens, 57396);
	}

	#[test]
	fn non_usage_lines_return_none() {
		let line = r#"{"type":"system","subtype":"turn_duration","durationMs":80438}"#;
		assert!(parse_usage_line(line).is_none());
	}

	#[test]
	fn malformed_lines_return_none() {
		assert!(parse_usage_line("not json").is_none());
	}
}
