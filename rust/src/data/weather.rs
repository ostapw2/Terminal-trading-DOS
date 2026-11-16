//! Weather as a series for formulas (`W:KYIV.TEMP`), from Open-Meteo: free,
//! no key.  A place name is looked up once (their geocoder), then hourly
//! readings come from the archive (it lags about five days) and the last
//! days from the forecast service's past hours.  Readings, not candles:
//! `formula::align` gives each candle the last one known at its open.
//!
//! Network only; `--demo` generates a plausible year instead.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::data::demo;
use crate::markets::Ohlc;

/// Variables: formula name, Open-Meteo hourly name, unit.
pub const VARS: &[(&str, &str, &str)] = &[
    ("TEMP", "temperature_2m", "C"),
    ("RAIN", "precipitation", "mm"),
    ("WIND", "wind_speed_10m", "km/h"),
    ("CLOUD", "cloud_cover", "%"),
    ("PRESS", "pressure_msl", "hPa"),
    ("HUM", "relative_humidity_2m", "%"),
    ("SUN", "sunshine_duration", "min/h"),
];

/// Longest history fetched (hourly readings: about 26 000).
pub const MAX_DAYS: i64 = 3 * 365;

pub fn var_name(v: &str) -> Option<&'static str> {
    VARS.iter()
        .find(|(n, _, _)| *n == v)
        .map(|(_, api, _)| *api)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(20))
        .user_agent("dos/0.1.0")
        .build()
}

fn places() -> &'static Mutex<HashMap<String, (f64, f64)>> {
    static P: OnceLock<Mutex<HashMap<String, (f64, f64)>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Latitude and longitude of a place name (the geocoder's best match).
pub fn locate(place: &str) -> Result<(f64, f64), String> {
    if let Some(&p) = places()
        .lock()
        .ok()
        .and_then(|m| m.get(place).copied())
        .as_ref()
    {
        return Ok(p);
    }
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&format=json",
        crate::data::yahoo::urlencode(&place.to_lowercase())
    );
    let v: serde_json::Value = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("geocoding {place}: {e}"))?
        .into_json()
        .map_err(|e| format!("geocoding {place}: {e}"))?;
    let r = &v["results"][0];
    let (lat, lon) = (r["latitude"].as_f64(), r["longitude"].as_f64());
    let (Some(lat), Some(lon)) = (lat, lon) else {
        return Err(format!("no place called {place} (try the English name)"));
    };
    if let Ok(mut m) = places().lock() {
        m.insert(place.to_string(), (lat, lon));
    }
    Ok((lat, lon))
}

/// `YYYY-MM-DDTHH:MM` (UTC) to Unix ms.
pub fn parse_hour(s: &str) -> Option<i64> {
    let (d, t) = s.split_once('T')?;
    let mut dp = d.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (dp.next()??, dp.next()??, dp.next()??);
    let mut tp = t.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi) = (tp.next()??, tp.next().flatten().unwrap_or(0));
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || !(0..24).contains(&h) {
        return None;
    }
    let days = crate::markets::days_from_civil(y, m as u32) as i64 + day - 1;
    Some(((days * 24 + h) * 60 + mi) * 60_000)
}

/// Hourly `(time, value)` rows of an Open-Meteo response.
fn rows(v: &serde_json::Value, api: &str) -> Vec<(i64, f64)> {
    let times = v["hourly"]["time"].as_array().cloned().unwrap_or_default();
    let vals = v["hourly"][api].as_array().cloned().unwrap_or_default();
    times
        .iter()
        .zip(vals.iter())
        .filter_map(|(t, x)| Some((parse_hour(t.as_str()?)?, x.as_f64()?)))
        .collect()
}

/// Hourly readings of `var` (see [`VARS`]) at `place` for the last `days`.
pub fn fetch(place: &str, var: &str, days: i64, now_ms: i64) -> Result<Vec<Ohlc>, String> {
    let api = var_name(var).ok_or_else(|| {
        format!(
            "weather: {var}? ({})",
            VARS.iter().map(|v| v.0).collect::<Vec<_>>().join(" ")
        )
    })?;
    let days = days.clamp(2, MAX_DAYS);
    if demo::is_on() {
        return Ok(demo_readings(place, var, days, now_ms));
    }
    let (lat, lon) = locate(place)?;
    let day = |ms: i64| crate::markets::unix_secs_to_iso_date(ms / 1000);
    let mut points: Vec<(i64, f64)> = Vec::new();
    // The archive up to about five days ago...
    let archive_end = now_ms - 5 * 86_400_000;
    let start = now_ms - days * 86_400_000;
    if archive_end > start {
        let url = format!(
            "https://archive-api.open-meteo.com/v1/archive?latitude={lat}&longitude={lon}&start_date={}&end_date={}&hourly={api}&timezone=UTC",
            day(start),
            day(archive_end)
        );
        let v: serde_json::Value = agent()
            .get(&url)
            .call()
            .map_err(|e| format!("weather archive: {e}"))?
            .into_json()
            .map_err(|e| format!("weather archive: {e}"))?;
        points.extend(rows(&v, api));
    }
    // ...and the last days from the forecast service (past hours only).
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&hourly={api}&past_days=7&forecast_days=1&timezone=UTC"
    );
    let v: serde_json::Value = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("weather: {e}"))?
        .into_json()
        .map_err(|e| format!("weather: {e}"))?;
    points.extend(rows(&v, api));
    Ok(to_readings(points, now_ms))
}

/// Sorted, one per hour (the newer source wins), nothing from the future.
fn to_readings(mut points: Vec<(i64, f64)>, now_ms: i64) -> Vec<Ohlc> {
    points.retain(|(t, v)| *t <= now_ms && v.is_finite());
    points.sort_by_key(|p| p.0);
    let mut out: Vec<Ohlc> = Vec::with_capacity(points.len());
    for (t, v) in points {
        let k = Ohlc {
            open: v,
            high: v,
            low: v,
            close: v,
            time_ms: t,
            ..Default::default()
        };
        match out.last_mut() {
            Some(last) if last.time_ms == t => *last = k,
            _ => out.push(k),
        }
    }
    out
}

/// A year of plausible weather: seasons, days and nights, some noise.
fn demo_readings(place: &str, var: &str, days: i64, now_ms: i64) -> Vec<Ohlc> {
    let seed = place
        .bytes()
        .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    let last = now_ms - now_ms.rem_euclid(3_600_000);
    let n = days * 24;
    let points = (0..n)
        .rev()
        .map(|back| {
            let t = last - back * 3_600_000;
            let day = t as f64 / 86_400_000.0;
            let season = (day / 365.25 * std::f64::consts::TAU).cos();
            let diurnal = ((day.fract() - 0.6) * std::f64::consts::TAU).cos();
            let noise = ((t / 3_600_000) as u64 ^ seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) as f64
                / u64::MAX as f64
                - 0.5;
            let v = match var {
                "TEMP" => 10.0 - 12.0 * season + 5.0 * diurnal + 2.0 * noise,
                "RAIN" => (noise * 6.0 - 2.0).max(0.0),
                "WIND" => 12.0 + 6.0 * noise + 3.0 * season,
                "CLOUD" => (55.0 + 80.0 * noise).clamp(0.0, 100.0),
                "PRESS" => 1013.0 + 8.0 * (day / 4.0).sin() + noise,
                "HUM" => (70.0 - 15.0 * diurnal + 20.0 * noise).clamp(0.0, 100.0),
                _ => (60.0 * diurnal).max(0.0),
            };
            (t, v)
        })
        .collect();
    to_readings(points, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_parse_as_utc_and_bad_ones_are_refused() {
        assert_eq!(parse_hour("1970-01-01T00:00"), Some(0));
        assert_eq!(parse_hour("2026-10-10T12:00"), Some(1_791_633_600_000));
        assert_eq!(parse_hour("2024-02-29T23:00"), Some(1_709_247_600_000));
        for bad in ["2026-13-01T00:00", "2026-10-10", "x", "2026-10-10T25:00"] {
            assert_eq!(parse_hour(bad), None, "{bad}");
        }
    }

    #[test]
    fn readings_are_hourly_sorted_and_never_from_the_future() {
        let now = 10 * 3_600_000;
        let r = to_readings(
            vec![
                (2 * 3_600_000, 1.0),
                (3_600_000, 0.5),
                (2 * 3_600_000, 1.5),
                (now + 1, 9.0),
                (0, f64::NAN),
            ],
            now,
        );
        assert_eq!(
            r.iter().map(|k| (k.time_ms, k.close)).collect::<Vec<_>>(),
            [(3_600_000, 0.5), (7_200_000, 1.5)]
        );
        let v: serde_json::Value = serde_json::json!({
            "hourly": {"time": ["2026-10-10T00:00", "2026-10-10T01:00"], "temperature_2m": [11.5, null]}
        });
        assert_eq!(rows(&v, "temperature_2m").len(), 1);
        let d = demo_readings("KYIV", "TEMP", 30, 1_791_633_600_000);
        assert_eq!(d.len(), 30 * 24);
        assert!(d.iter().all(|k| (-40.0..50.0).contains(&k.close)));
    }
}
