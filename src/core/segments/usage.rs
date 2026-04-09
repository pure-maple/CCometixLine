use super::{Segment, SegmentData};
use crate::config::{InputData, SegmentId};
use crate::utils::credentials;
use chrono::{DateTime, Datelike, Duration, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct ApiUsageResponse {
    five_hour: UsagePeriod,
    seven_day: UsagePeriod,
}

#[derive(Debug, Deserialize)]
struct UsagePeriod {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiUsageCache {
    five_hour_utilization: f64,
    seven_day_utilization: f64,
    #[serde(default)]
    five_hour_resets_at: Option<String>,
    resets_at: Option<String>,
    cached_at: String,
}

#[derive(Default)]
pub struct UsageSegment;

impl UsageSegment {
    pub fn new() -> Self {
        Self
    }

    pub(super) fn get_circle_icon(utilization: f64) -> String {
        let percent = (utilization * 100.0) as u8;
        match percent {
            0..=12 => "\u{f0a9e}".to_string(),  // circle_slice_1
            13..=25 => "\u{f0a9f}".to_string(), // circle_slice_2
            26..=37 => "\u{f0aa0}".to_string(), // circle_slice_3
            38..=50 => "\u{f0aa1}".to_string(), // circle_slice_4
            51..=62 => "\u{f0aa2}".to_string(), // circle_slice_5
            63..=75 => "\u{f0aa3}".to_string(), // circle_slice_6
            76..=87 => "\u{f0aa4}".to_string(), // circle_slice_7
            _ => "\u{f0aa5}".to_string(),       // circle_slice_8
        }
    }

    fn round_to_hour(time_str: &str) -> Option<DateTime<Local>> {
        let dt = DateTime::parse_from_rfc3339(time_str).ok()?;
        let mut local_dt = dt.with_timezone(&Local);
        if local_dt.minute() > 45 {
            local_dt += Duration::hours(1);
        }
        Some(local_dt)
    }

    pub(super) fn format_reset_hour(reset_time_str: Option<&str>) -> String {
        if let Some(time_str) = reset_time_str {
            if let Some(local_dt) = Self::round_to_hour(time_str) {
                return format!("@{}", local_dt.hour());
            }
        }
        "".to_string()
    }

    pub(super) fn format_reset_date_hour(reset_time_str: Option<&str>) -> String {
        if let Some(time_str) = reset_time_str {
            if let Some(local_dt) = Self::round_to_hour(time_str) {
                return format!(
                    "@{}-{} {}",
                    local_dt.month(),
                    local_dt.day(),
                    local_dt.hour()
                );
            }
        }
        "".to_string()
    }

    /// Calculate the expected (budget) utilization for a period based on elapsed time.
    /// Returns a percentage (0-100) representing how much of the budget should ideally
    /// be consumed by now if usage were evenly distributed across the window.
    pub(super) fn calc_budget_pace(reset_time_str: Option<&str>, period: Duration) -> Option<u8> {
        let time_str = reset_time_str?;
        let resets_at = DateTime::parse_from_rfc3339(time_str).ok()?;
        let resets_at_utc = resets_at.with_timezone(&Utc);
        let period_start = resets_at_utc - period;
        let now = Utc::now();

        if now < period_start || now > resets_at_utc {
            return None;
        }

        let elapsed = now.signed_duration_since(period_start);
        let pace = (elapsed.num_seconds() as f64 / period.num_seconds() as f64) * 100.0;
        Some(pace.round() as u8)
    }

    /// Estimate remaining time before hitting 100% at the current consumption rate.
    /// Returns a human-readable duration string like "~1.5d" or "~3h".
    pub(super) fn calc_time_to_limit(utilization: f64, reset_time_str: Option<&str>, period: Duration) -> Option<String> {
        if utilization <= 0.0 || utilization >= 100.0 {
            return None;
        }
        let time_str = reset_time_str?;
        let resets_at = DateTime::parse_from_rfc3339(time_str).ok()?;
        let resets_at_utc = resets_at.with_timezone(&Utc);
        let period_start = resets_at_utc - period;
        let now = Utc::now();

        if now <= period_start || now >= resets_at_utc {
            return None;
        }

        let elapsed_secs = now.signed_duration_since(period_start).num_seconds() as f64;
        // Current burn rate: utilization% consumed in elapsed_secs
        // Time to reach 100%: (100 / utilization) * elapsed_secs
        let total_secs_to_100 = (100.0 / utilization) * elapsed_secs;
        let remaining_secs = total_secs_to_100 - elapsed_secs;

        if remaining_secs <= 0.0 {
            return Some("~0".to_string());
        }

        let remaining_hours = remaining_secs / 3600.0;
        if remaining_hours >= 24.0 {
            let days = remaining_hours / 24.0;
            Some(format!("~{:.1}d", days))
        } else if remaining_hours >= 1.0 {
            Some(format!("~{:.0}h", remaining_hours))
        } else {
            let mins = remaining_secs / 60.0;
            Some(format!("~{:.0}m", mins))
        }
    }

    fn get_cache_path() -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        Some(
            home.join(".claude")
                .join("ccline")
                .join(".api_usage_cache.json"),
        )
    }

    fn load_cache(&self) -> Option<ApiUsageCache> {
        let cache_path = Self::get_cache_path()?;
        if !cache_path.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&cache_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_cache(&self, cache: &ApiUsageCache) {
        if let Some(cache_path) = Self::get_cache_path() {
            if let Some(parent) = cache_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(cache) {
                let _ = std::fs::write(&cache_path, json);
            }
        }
    }

    fn is_cache_valid(&self, cache: &ApiUsageCache, cache_duration: u64) -> bool {
        if let Ok(cached_at) = DateTime::parse_from_rfc3339(&cache.cached_at) {
            let now = Utc::now();
            let elapsed = now.signed_duration_since(cached_at.with_timezone(&Utc));
            elapsed.num_seconds() < cache_duration as i64
        } else {
            false
        }
    }

    fn get_claude_code_version() -> String {
        use std::process::Command;

        let output = Command::new("npm")
            .args(["view", "@anthropic-ai/claude-code", "version"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !version.is_empty() {
                    return format!("claude-code/{}", version);
                }
            }
            _ => {}
        }

        "claude-code".to_string()
    }

    fn get_proxy_from_settings() -> Option<String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        let settings_path = format!("{}/.claude/settings.json", home);

        let content = std::fs::read_to_string(&settings_path).ok()?;
        let settings: serde_json::Value = serde_json::from_str(&content).ok()?;

        // Try HTTPS_PROXY first, then HTTP_PROXY
        settings
            .get("env")?
            .get("HTTPS_PROXY")
            .or_else(|| settings.get("env")?.get("HTTP_PROXY"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn fetch_api_usage(
        &self,
        api_base_url: &str,
        token: &str,
        timeout_secs: u64,
    ) -> Option<ApiUsageResponse> {
        let url = format!("{}/api/oauth/usage", api_base_url);
        let user_agent = Self::get_claude_code_version();

        let agent = if let Some(proxy_url) = Self::get_proxy_from_settings() {
            if let Ok(proxy) = ureq::Proxy::new(&proxy_url) {
                ureq::Agent::config_builder()
                    .proxy(Some(proxy))
                    .build()
                    .new_agent()
            } else {
                ureq::Agent::new_with_defaults()
            }
        } else {
            ureq::Agent::new_with_defaults()
        };

        let response = agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", token))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", &user_agent)
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .build()
            .call()
            .ok()?;

        response.into_body().read_json().ok()
    }
}

impl Segment for UsageSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
        let token = credentials::get_oauth_token()?;

        // Load config from file to get segment options
        let config = crate::config::Config::load().ok()?;
        let segment_config = config.segments.iter().find(|s| s.id == SegmentId::Usage);

        let api_base_url = segment_config
            .and_then(|sc| sc.options.get("api_base_url"))
            .and_then(|v| v.as_str())
            .unwrap_or("https://api.anthropic.com");

        let cache_duration = segment_config
            .and_then(|sc| sc.options.get("cache_duration"))
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        let timeout = segment_config
            .and_then(|sc| sc.options.get("timeout"))
            .and_then(|v| v.as_u64())
            .unwrap_or(2);

        let cached_data = self.load_cache();
        let use_cached = cached_data
            .as_ref()
            .map(|cache| self.is_cache_valid(cache, cache_duration))
            .unwrap_or(false);

        let (five_hour_util, seven_day_util, five_hour_resets_at, seven_day_resets_at) =
            if use_cached {
                let cache = cached_data.unwrap();
                (
                    cache.five_hour_utilization,
                    cache.seven_day_utilization,
                    cache.five_hour_resets_at,
                    cache.resets_at,
                )
            } else {
                match self.fetch_api_usage(api_base_url, &token, timeout) {
                    Some(response) => {
                        let cache = ApiUsageCache {
                            five_hour_utilization: response.five_hour.utilization,
                            seven_day_utilization: response.seven_day.utilization,
                            five_hour_resets_at: response.five_hour.resets_at.clone(),
                            resets_at: response.seven_day.resets_at.clone(),
                            cached_at: Utc::now().to_rfc3339(),
                        };
                        self.save_cache(&cache);
                        (
                            response.five_hour.utilization,
                            response.seven_day.utilization,
                            response.five_hour.resets_at,
                            response.seven_day.resets_at,
                        )
                    }
                    None => {
                        if let Some(cache) = cached_data {
                            (
                                cache.five_hour_utilization,
                                cache.seven_day_utilization,
                                cache.five_hour_resets_at,
                                cache.resets_at,
                            )
                        } else {
                            return None;
                        }
                    }
                }
            };

        let max_util = five_hour_util.max(seven_day_util);
        let dynamic_icon = Self::get_circle_icon(max_util / 100.0);
        let five_hour_percent = five_hour_util.round() as u8;
        let seven_day_percent = seven_day_util.round() as u8;

        let five_hour_pace = Self::calc_budget_pace(five_hour_resets_at.as_deref(), Duration::hours(5));
        let seven_day_pace = Self::calc_budget_pace(seven_day_resets_at.as_deref(), Duration::days(7));
        let time_to_limit = Self::calc_time_to_limit(seven_day_util, seven_day_resets_at.as_deref(), Duration::days(7));

        // Combined format for legacy Usage segment
        let five_h = match five_hour_pace {
            Some(pace) => format!("5h {}%({}%)", five_hour_percent, pace),
            None => format!("5h {}%", five_hour_percent),
        };
        let mut seven_d = match seven_day_pace {
            Some(pace) => format!("7d {}%({}%)", seven_day_percent, pace),
            None => format!("7d {}%", seven_day_percent),
        };
        if let Some(ref ttl) = time_to_limit {
            seven_d.push_str(&format!(" {}", ttl));
        }

        let primary = format!("{} · {}", five_h, seven_d);

        let mut metadata = HashMap::new();
        metadata.insert("dynamic_icon".to_string(), dynamic_icon);
        metadata.insert("five_hour_utilization".to_string(), five_hour_util.to_string());
        metadata.insert("seven_day_utilization".to_string(), seven_day_util.to_string());

        Some(SegmentData {
            primary,
            secondary: String::new(),
            metadata,
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::Usage
    }
}

// ── Shared cache reader for split segments ──

struct UsageData {
    five_hour_util: f64,
    seven_day_util: f64,
    five_hour_resets_at: Option<String>,
    seven_day_resets_at: Option<String>,
}

fn load_usage_data() -> Option<UsageData> {
    let segment = UsageSegment::new();
    let token = credentials::get_oauth_token()?;

    let config = crate::config::Config::load().ok()?;
    // Read options from hourly_usage, weekly_usage, or legacy usage
    let segment_config = config.segments.iter().find(|s| {
        s.id == SegmentId::HourlyUsage || s.id == SegmentId::WeeklyUsage || s.id == SegmentId::Usage
    });

    let api_base_url = segment_config
        .and_then(|sc| sc.options.get("api_base_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("https://api.anthropic.com");
    let cache_duration = segment_config
        .and_then(|sc| sc.options.get("cache_duration"))
        .and_then(|v| v.as_u64())
        .unwrap_or(300);
    let timeout = segment_config
        .and_then(|sc| sc.options.get("timeout"))
        .and_then(|v| v.as_u64())
        .unwrap_or(2);

    let cached_data = segment.load_cache();
    let use_cached = cached_data.as_ref().map(|c| segment.is_cache_valid(c, cache_duration)).unwrap_or(false);

    let (fh, sd, fhr, sdr) = if use_cached {
        let c = cached_data.unwrap();
        (c.five_hour_utilization, c.seven_day_utilization, c.five_hour_resets_at, c.resets_at)
    } else {
        match segment.fetch_api_usage(api_base_url, &token, timeout) {
            Some(resp) => {
                let cache = ApiUsageCache {
                    five_hour_utilization: resp.five_hour.utilization,
                    seven_day_utilization: resp.seven_day.utilization,
                    five_hour_resets_at: resp.five_hour.resets_at.clone(),
                    resets_at: resp.seven_day.resets_at.clone(),
                    cached_at: Utc::now().to_rfc3339(),
                };
                segment.save_cache(&cache);
                (resp.five_hour.utilization, resp.seven_day.utilization, resp.five_hour.resets_at, resp.seven_day.resets_at)
            }
            None => {
                let c = cached_data?;
                (c.five_hour_utilization, c.seven_day_utilization, c.five_hour_resets_at, c.resets_at)
            }
        }
    };

    Some(UsageData { five_hour_util: fh, seven_day_util: sd, five_hour_resets_at: fhr, seven_day_resets_at: sdr })
}

// ── HourlyUsageSegment (5h) ──

#[derive(Default)]
pub struct HourlyUsageSegment;

impl HourlyUsageSegment {
    pub fn new() -> Self { Self }
}

impl Segment for HourlyUsageSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
        let data = load_usage_data()?;
        let percent = data.five_hour_util.round() as u8;
        let pace = UsageSegment::calc_budget_pace(data.five_hour_resets_at.as_deref(), Duration::hours(5));
        let reset = UsageSegment::format_reset_hour(data.five_hour_resets_at.as_deref());

        let primary = match pace {
            Some(p) => format!("{}%({}%)", percent, p),
            None => format!("{}%", percent),
        };
        // Secondary: reset time with clock icon
        let secondary = if !reset.is_empty() {
            format!("󰅐 {}", reset)
        } else {
            String::new()
        };

        let dynamic_icon = UsageSegment::get_circle_icon(data.five_hour_util / 100.0);
        let mut metadata = HashMap::new();
        metadata.insert("dynamic_icon".to_string(), dynamic_icon);
        metadata.insert("utilization".to_string(), data.five_hour_util.to_string());

        Some(SegmentData { primary, secondary, metadata })
    }

    fn id(&self) -> SegmentId { SegmentId::HourlyUsage }
}

// ── WeeklyUsageSegment (7d) ──

#[derive(Default)]
pub struct WeeklyUsageSegment;

impl WeeklyUsageSegment {
    pub fn new() -> Self { Self }
}

impl Segment for WeeklyUsageSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
        let data = load_usage_data()?;
        let percent = data.seven_day_util.round() as u8;
        let pace = UsageSegment::calc_budget_pace(data.seven_day_resets_at.as_deref(), Duration::days(7));
        let ttl = UsageSegment::calc_time_to_limit(data.seven_day_util, data.seven_day_resets_at.as_deref(), Duration::days(7));
        let reset = UsageSegment::format_reset_date_hour(data.seven_day_resets_at.as_deref());

        let mut primary = match pace {
            Some(p) => format!("{}%({}%)", percent, p),
            None => format!("{}%", percent),
        };
        if let Some(ref t) = ttl {
            primary.push_str(&format!(" {}", t));
        }

        // Secondary: reset time with clock icon
        let secondary = if !reset.is_empty() {
            format!("󰅐 {}", reset)
        } else {
            String::new()
        };

        let dynamic_icon = UsageSegment::get_circle_icon(data.seven_day_util / 100.0);
        let mut metadata = HashMap::new();
        metadata.insert("dynamic_icon".to_string(), dynamic_icon);
        metadata.insert("utilization".to_string(), data.seven_day_util.to_string());

        Some(SegmentData { primary, secondary, metadata })
    }

    fn id(&self) -> SegmentId { SegmentId::WeeklyUsage }
}
