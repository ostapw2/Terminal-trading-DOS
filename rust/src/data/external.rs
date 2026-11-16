//! Outside data for formulas: any JSON API the user describes (`H:NAME`)
//! and macro series from FRED (`M:SERIES`).
//!
//! **Any API.** A source is a few lines in `sources.json` next to the
//! database (`DOS_SOURCES` overrides the path):
//!
//! ```json
//! [{"name": "FNG", "url": "https://api.alternative.me/fng/?limit=0",
//!   "items": "data", "time": "timestamp", "time_unit": "s",
//!   "value": "value", "lag_hours": 1}]
//! ```
//!
//! `items` is the dotted path to the array, `time` and `value` are fields of
//! each item (numbers or numeric strings); `time_unit` is `s`, `ms`, `date`
//! (`YYYY-MM-DD`) or `iso`.  `lag_hours` is how long after its time a value
//! becomes known (no look-ahead).  A key never goes in the file or a
//! formula: `"key_env": "MY_API_KEY"` puts that environment variable where
//! the URL says `{KEY}`, and errors never print the URL.  Only `https`.
//! Built in: `FNG`, the crypto Fear & Greed index (daily, since 2018).
//!
//! **FRED** (St. Louis Fed; free key in `FRED_API_KEY`): `M:DGS10` (10-year
//! yield), `M:CPIAUCSL` (CPI), `M:DTWEXBGS` (dollar index), ...  An
//! observation is dated by the period it measures, not by when it was
//! published (January's CPI comes out in mid-February), so each value is
//! moved to a conservative publication time by the series' frequency:
//! daily +1 day, weekly +7, monthly +45, quarterly +90, annual +365.
//!
//! Readings, not candles: `formula::align` gives each candle the last value
//! known at its open.  `--demo` generates data instead of the network.

use std::time::Duration;

use serde::Deserialize;

use crate::data::demo;
use crate::markets::Ohlc;

/// Largest response read (bytes).
const MAX_BODY: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct HttpSource {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub items: String,
    pub time: String,
    #[serde(default = "unit_s")]
    pub time_unit: String,
    pub value: String,
    #[serde(default)]
    pub key_env: Option<String>,
    #[serde(default)]
    pub lag_hours: f64,
}

fn unit_s() -> String {
    "s".into()
}

impl HttpSource {
    /// Refuse what could leak or surprise: names, https only, a sane lag.
    pub fn validate(&self) -> Result<(), String> {
        let ok_name = (1..=16).contains(&self.name.len())
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !ok_name {
            return Err(format!("source name '{}': 1-16 of A-Z 0-9 _", self.name));
        }
        if !self.url.starts_with("https://") {
            return Err(format!("source {}: the URL must be https://", self.name));
        }
        if !["s", "ms", "date", "iso"].contains(&self.time_unit.as_str()) {
            return Err(format!(
                "source {}: time_unit is s, ms, date or iso",
                self.name
            ));
        }
        if !(0.0..=24.0 * 400.0).contains(&self.lag_hours) {
            return Err(format!("source {}: lag_hours 0-9600", self.name));
        }
        if let Some(k) = &self.key_env {
            let ok = !k.is_empty()
                && k.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
            if !ok || !self.url.contains("{KEY}") {
                return Err(format!(
                    "source {}: key_env is an environment variable name, and the URL holds {{KEY}}",
                    self.name
                ));
            }
        }
        Ok(())
    }
}

/// Built-in sources (no key).
pub fn builtin() -> Vec<HttpSource> {
    vec![HttpSource {
        name: "FNG".into(),
        url: "https://api.alternative.me/fng/?limit=0".into(),
        items: "data".into(),
        time: "timestamp".into(),
        time_unit: "s".into(),
        value: "value".into(),
        key_env: None,
        lag_hours: 1.0,
    }]
}

/// Where the user's sources live.
pub fn sources_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("DOS_SOURCES").filter(|p| !p.is_empty()) {
        return Some(p.into());
    }
    crate::data::db::db_path()?
        .parent()
        .map(|d| d.join("sources.json"))
}

/// Built-in sources and the user's (a user source of the same name wins);
/// the second value lists the problems of the file, if any.
pub fn sources() -> (Vec<HttpSource>, Vec<String>) {
    let mut all = builtin();
    let mut problems = Vec::new();
    if let Some(path) = sources_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<Vec<HttpSource>>(&text) {
                Ok(list) => {
                    for s in list {
                        match s.validate() {
                            Ok(()) => {
                                all.retain(|b| b.name != s.name);
                                all.push(s);
                            }
                            Err(e) => problems.push(e),
                        }
                    }
                }
                Err(e) => problems.push(format!("{}: {e}", path.display())),
            }
        }
    }
    (all, problems)
}

/// `a.b.0.c` into a JSON value.
pub fn at_path<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.')
        .filter(|p| !p.is_empty())
        .try_fold(v, |cur, key| match key.parse::<usize>() {
            Ok(i) if cur.is_array() => cur.get(i),
            _ => cur.get(key),
        })
}

fn number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|x: &f64| x.is_finite())
}

/// A time field in Unix ms.
fn time_ms(v: &serde_json::Value, unit: &str) -> Option<i64> {
    match unit {
        "s" => number(v).map(|x| (x * 1000.0) as i64),
        "ms" => number(v).map(|x| x as i64),
        "date" => crate::data::weather::parse_hour(&format!("{}T00:00", v.as_str()?.get(..10)?)),
        _ => crate::data::weather::parse_hour(v.as_str()?.get(..16)?),
    }
}

/// Readings of a parsed response: one per item with a time and a value,
/// moved by the publication lag, oldest first, none from the future.
pub fn readings(
    src: &HttpSource,
    body: &serde_json::Value,
    now_ms: i64,
) -> Result<Vec<Ohlc>, String> {
    let items = at_path(body, &src.items)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("source {}: no array at '{}'", src.name, src.items))?;
    let lag = (src.lag_hours * 3_600_000.0) as i64;
    let mut out: Vec<Ohlc> = items
        .iter()
        .filter_map(|it| {
            let t = time_ms(at_path(it, &src.time)?, &src.time_unit)? + lag;
            let v = number(at_path(it, &src.value)?)?;
            Some(Ohlc {
                open: v,
                high: v,
                low: v,
                close: v,
                time_ms: t,
                ..Default::default()
            })
        })
        .filter(|k| k.time_ms > 0 && k.time_ms <= now_ms)
        .collect();
    out.sort_by_key(|k| k.time_ms);
    out.dedup_by_key(|k| k.time_ms);
    if out.is_empty() {
        return Err(format!(
            "source {}: no items with a '{}' and a numeric '{}'",
            src.name, src.time, src.value
        ));
    }
    Ok(out)
}

fn get(url: &str, what: &str) -> Result<serde_json::Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(20))
        .user_agent("dos/0.1.0")
        .build();
    // Errors name the source, never the URL (it may hold a key).
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("{what}: HTTP {code}"),
        _ => format!("{what}: no connection"),
    })?;
    use std::io::Read;
    let mut body = Vec::new();
    resp.into_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut body)
        .map_err(|_| format!("{what}: reading failed"))?;
    if body.len() as u64 > MAX_BODY {
        return Err(format!("{what}: response larger than 10 MB"));
    }
    serde_json::from_slice(&body).map_err(|_| format!("{what}: not JSON"))
}

/// Readings of the source called `name` (see [`sources`]).
pub fn fetch_http(name: &str, now_ms: i64) -> Result<Vec<Ohlc>, String> {
    let (all, problems) = sources();
    let Some(src) = all.into_iter().find(|s| s.name == name) else {
        let mut msg = format!("no source {name}: add it to sources.json (see :sources)");
        if let Some(p) = problems.first() {
            msg.push_str(&format!("; {p}"));
        }
        return Err(msg);
    };
    if demo::is_on() {
        return Ok(demo_series(name, 86_400_000, 2000, now_ms));
    }
    let url = match &src.key_env {
        Some(k) => {
            let key = std::env::var(k)
                .map_err(|_| format!("source {name}: set {k} in the environment"))?;
            src.url
                .replace("{KEY}", &crate::data::yahoo::urlencode(&key))
        }
        None => src.url.clone(),
    };
    readings(&src, &get(&url, &format!("source {name}"))?, now_ms)
}

// ─── FRED ───────────────────────────────────────────────────────────────

/// Days from an observation's date to when it is surely published, by the
/// series' frequency (FRED `frequency_short`).
pub fn publication_lag_days(freq: &str) -> i64 {
    match freq {
        "D" => 1,
        "W" | "BW" => 7,
        "M" => 45,
        "Q" => 90,
        "SA" => 180,
        _ => 365,
    }
}

/// A FRED series id: 1-30 of A-Z 0-9 _.
pub fn valid_series(s: &str) -> bool {
    (1..=30).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Readings of FRED series `id` over the last `days`, each at its
/// publication time (see [`publication_lag_days`]).
pub fn fetch_fred(id: &str, days: i64, now_ms: i64) -> Result<Vec<Ohlc>, String> {
    if !valid_series(id) {
        return Err(format!("FRED series '{id}'?"));
    }
    if demo::is_on() {
        return Ok(demo_series(id, 30 * 86_400_000, 120, now_ms));
    }
    let key = std::env::var("FRED_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .ok_or("FRED needs a free key: fred.stlouisfed.org, My Account, API Keys; put it in FRED_API_KEY")?;
    let key = crate::data::yahoo::urlencode(&key);
    let what = format!("FRED {id}");
    let meta = get(
        &format!(
            "https://api.stlouisfed.org/fred/series?series_id={id}&api_key={key}&file_type=json"
        ),
        &what,
    )?;
    let freq = at_path(&meta, "seriess.0.frequency_short")
        .and_then(|v| v.as_str())
        .unwrap_or("A")
        .to_string();
    let lag = publication_lag_days(&freq);
    let start = crate::markets::unix_secs_to_iso_date((now_ms - (days + lag) * 86_400_000) / 1000);
    let body = get(
        &format!(
            "https://api.stlouisfed.org/fred/series/observations?series_id={id}&api_key={key}&file_type=json&observation_start={start}"
        ),
        &what,
    )?;
    let src = HttpSource {
        name: id.into(),
        url: String::new(),
        items: "observations".into(),
        time: "date".into(),
        time_unit: "date".into(),
        value: "value".into(),
        key_env: None,
        lag_hours: (lag * 24) as f64,
    };
    // FRED writes a missing value as ".": skipped by `readings`.
    readings(&src, &body, now_ms)
}

/// Demo: a slow wave with some noise, `n` readings `step` apart.
fn demo_series(name: &str, step: i64, n: i64, now_ms: i64) -> Vec<Ohlc> {
    let seed = name
        .bytes()
        .fold(3u64, |h, b| h.wrapping_mul(131).wrapping_add(b as u64));
    let last = now_ms - now_ms.rem_euclid(step);
    (0..n)
        .rev()
        .map(|back| {
            let t = last - back * step;
            let x = back as f64;
            let noise = ((t / step) as u64 ^ seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) as f64
                / u64::MAX as f64;
            let v = 50.0 + 25.0 * (x / 40.0 + (seed % 5) as f64).sin() + 10.0 * (noise - 0.5);
            Ohlc {
                open: v,
                high: v,
                low: v,
                close: v,
                time_ms: t,
                ..Default::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_described_api_becomes_readings_with_its_lag() {
        let src = &builtin()[0];
        assert!(src.validate().is_ok());
        let body = json!({"data": [
            {"value": "64", "timestamp": "1791590400"},
            {"value": "59", "timestamp": "1791504000"},
            {"value": "n/a", "timestamp": "1791417600"},
            {"value": "70", "timestamp": "9999999999"}
        ]});
        let now = 1_791_600_000_000;
        let r = readings(src, &body, now).unwrap();
        assert_eq!(r.len(), 2, "no number, and the future, are dropped");
        assert_eq!(r[0].close, 59.0);
        assert_eq!(
            r[1].time_ms,
            1_791_590_400_000 + 3_600_000,
            "the publication lag"
        );
        assert!(readings(src, &json!({"other": []}), now)
            .unwrap_err()
            .contains("no array"));
    }

    #[test]
    fn paths_dates_and_fred_style_values() {
        let v = json!({"a": {"b": [{"c": 1}, {"c": 2}]}});
        assert_eq!(at_path(&v, "a.b.1.c"), Some(&json!(2)));
        assert_eq!(at_path(&v, "a.x"), None);
        let fred = HttpSource {
            name: "DGS10".into(),
            url: String::new(),
            items: "observations".into(),
            time: "date".into(),
            time_unit: "date".into(),
            value: "value".into(),
            key_env: None,
            lag_hours: 24.0 * publication_lag_days("M") as f64,
        };
        let body = json!({"observations": [
            {"date": "2026-01-01", "value": "3.1"},
            {"date": "2026-02-01", "value": "."}
        ]});
        let r = readings(&fred, &body, 1_791_633_600_000).unwrap();
        assert_eq!(r.len(), 1, "'.' is FRED's missing value");
        // January's value is known 45 days later, not on January 1.
        assert_eq!(
            r[0].time_ms,
            crate::data::weather::parse_hour("2026-02-15T00:00").unwrap()
        );
        assert!(valid_series("CPIAUCSL") && !valid_series("cpi") && !valid_series("A&B"));
    }

    #[test]
    fn sources_that_could_leak_or_surprise_are_refused() {
        let base = builtin()[0].clone();
        let bad = [
            HttpSource {
                url: "http://x.y/".into(),
                ..base.clone()
            },
            HttpSource {
                name: "bad name".into(),
                ..base.clone()
            },
            HttpSource {
                time_unit: "weeks".into(),
                ..base.clone()
            },
            HttpSource {
                key_env: Some("MY_KEY".into()),
                ..base.clone()
            },
            HttpSource {
                key_env: Some("lower".into()),
                url: "https://a/?k={KEY}".into(),
                ..base.clone()
            },
        ];
        for b in bad {
            assert!(b.validate().is_err(), "{b:?}");
        }
        let ok = HttpSource {
            key_env: Some("MY_KEY".into()),
            url: "https://a/?k={KEY}".into(),
            ..base
        };
        assert!(ok.validate().is_ok());
    }
}
