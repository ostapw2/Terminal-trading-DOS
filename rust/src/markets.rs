//! Market symbol definitions and data structures.
//! No synthetic data — symbols start with empty OHLC and get populated
//! from Binance (crypto) or Yahoo (stocks/forex/commodities) at startup.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketType {
    UsStocks,
    Crypto,
    EuStocks,
    Forex,
    Commodities,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timeframe {
    S15,
    M1,
    M5,
    H1,
    D1,
}

impl Timeframe {
    pub fn duration_secs(&self) -> u64 {
        match self {
            Timeframe::S15 => 15,
            Timeframe::M1 => 60,
            Timeframe::M5 => 300,
            Timeframe::H1 => 3600,
            Timeframe::D1 => 86400,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Timeframe::S15 => "15s",
            Timeframe::M1 => "1m",
            Timeframe::M5 => "5m",
            Timeframe::H1 => "1h",
            Timeframe::D1 => "1d",
        }
    }

    /// Yahoo Finance `interval` query param (Yahoo has no 15s — fall back to 1m).
    pub fn yahoo_interval(&self) -> &'static str {
        match self {
            Self::S15 => "1m",
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::H1 => "1h",
            Self::D1 => "1d",
        }
    }
    /// Yahoo `range` to pair with the interval.  Sub-day intervals are
    /// limited to recent history.
    pub fn yahoo_range(&self) -> &'static str {
        match self {
            Self::S15 | Self::M1 => "5d",
            Self::M5 => "1mo",
            Self::H1 => "3mo",
            Self::D1 => "3mo",
        }
    }
    /// Binance kline interval.  Binance does not offer 15s either — fallback
    /// is 1m.  Kept as an explicit pair so future per-exchange tuning is easy.
    pub fn binance_interval(&self) -> &'static str {
        match self {
            Self::S15 => "1m",
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::H1 => "1h",
            Self::D1 => "1d",
        }
    }
    /// Seconds remaining until the next candle on this timeframe closes,
    /// based on UTC wall clock.
    pub fn secs_to_close(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bucket = self.duration_secs();
        if bucket == 0 {
            return 0;
        }
        let remainder = now % bucket;
        bucket - remainder
    }
}

impl MarketType {
    pub fn name(&self) -> &'static str {
        match self {
            Self::UsStocks => "US Stocks",
            Self::Crypto => "Crypto",
            Self::EuStocks => "EU Stocks",
            Self::Forex => "Forex",
            Self::Commodities => "Commodities",
        }
    }

    /// Convert the display ticker into the symbol Yahoo Finance expects.
    /// Crypto needs `-USD`, forex `=X`, commodities `=F`.  EU stocks have
    /// per-exchange suffixes (`.AS`, `.DE`, `.PA`) which we don't track yet
    /// — for now those go bare and Yahoo may 404.
    pub fn yahoo_ticker(&self, ticker: &str) -> String {
        match self {
            Self::Crypto => format!("{}-USD", ticker),
            Self::Forex => format!("{}=X", ticker),
            Self::Commodities => format!("{}=F", ticker),
            Self::UsStocks | Self::EuStocks => ticker.to_string(),
        }
    }
}

#[derive(Clone, Default)]
pub struct Ohlc {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub date: String,
    /// Unix epoch milliseconds at the candle's open.  0 = unknown
    /// (pre-time_ms cache); session-time filter then no-ops.
    pub time_ms: i64,
    /// Sentinel candle inserted between sessions when the session-time
    /// filter is active.  Renderers must skip these (no body, no wick,
    /// no volume bar) so the column appears as a visible gap.
    pub is_gap: bool,
}

/// Inclusive `[start_min, end_min)` minute-of-day window (0..1440).  When
/// `start_min == end_min` (or the unset default 0..1440) the filter is
/// "off" and every candle passes.  When `end_min < start_min` it's
/// treated as an overnight session (e.g. 22:00..04:00 → minutes
/// `>= 22*60 || < 4*60`).  Candles whose `time_ms == 0` (pre-time_ms
/// cache) bypass the filter entirely so legacy caches keep rendering.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionWindow {
    pub start_min: u16,
    pub end_min: u16,
}

impl SessionWindow {
    pub const ALL: SessionWindow = SessionWindow {
        start_min: 0,
        end_min: 1440,
    };

    pub fn is_off(self) -> bool {
        self.start_min == self.end_min || (self.start_min == 0 && self.end_min >= 1440)
    }

    pub fn contains_minute(self, m: u16) -> bool {
        let m = m.min(1439);
        if self.is_off() {
            return true;
        }
        if self.end_min > self.start_min {
            m >= self.start_min && m < self.end_min
        } else {
            m >= self.start_min || m < self.end_min
        }
    }

    pub fn label(self) -> String {
        if self.is_off() {
            return "All hours".into();
        }
        format!(
            "{:02}:{:02}-{:02}:{:02}",
            self.start_min / 60,
            self.start_min % 60,
            self.end_min / 60,
            self.end_min % 60
        )
    }
}

/// Apply user-selected filters to a raw OHLC series:
///   * Date range  — inclusive `date_from <= o.date <= date_to` (string compare on YYYY-MM-DD).
///   * Session     — minute-of-day window (intraday only; ignored when `time_ms == 0`).
///
/// When the session filter is active, a single sentinel "gap" candle is
/// inserted between consecutive sessions (i.e. whenever the calendar day
/// changes between two surviving candles).  Renderers detect `is_gap`
/// and skip the column, producing a visible blank slot.
pub fn apply_filters(
    data: &[Ohlc],
    date_from: Option<&str>,
    date_to: Option<&str>,
    session: SessionWindow,
    time_from_ms: i64,
) -> Vec<Ohlc> {
    let session_active = !session.is_off();
    let mut out: Vec<Ohlc> = Vec::with_capacity(data.len());
    let mut last_day: Option<String> = None;
    for o in data {
        if time_from_ms > 0 && o.time_ms > 0 && o.time_ms < time_from_ms {
            continue;
        }
        if let Some(from) = date_from {
            if o.date.as_str() < from {
                continue;
            }
        }
        if let Some(to) = date_to {
            if o.date.as_str() > to {
                continue;
            }
        }
        if session_active && o.time_ms != 0 {
            // Minute-of-day in UTC.  Good enough for v1 — most session
            // boundaries are quoted in exchange-local time anyway and
            // the user picks the window to match what they see.
            let minute = (((o.time_ms / 60_000).rem_euclid(1440)) as u16).min(1439);
            if !session.contains_minute(minute) {
                continue;
            }
        }
        if session_active {
            // Day-boundary detection — push a gap whenever the
            // surviving series jumps to a new calendar day.  Skip the
            // very first surviving candle and any time `is_gap` would
            // be redundant with the previous one.
            if let Some(prev_day) = &last_day {
                if prev_day != &o.date && !out.last().map(|c| c.is_gap).unwrap_or(false) {
                    out.push(Ohlc {
                        is_gap: true,
                        ..Ohlc::default()
                    });
                }
            }
            last_day = Some(o.date.clone());
        }
        out.push(o.clone());
    }
    // A trailing gap looks weird — drop it.
    while out.last().map(|c| c.is_gap).unwrap_or(false) {
        out.pop();
    }
    out
}

/// Insert a sentinel `is_gap` candle between every two consecutive
/// candles whose `date` strings differ, but ONLY when the input looks
/// like an intraday timeframe (≥2 candles per unique date on average).
/// On daily TFs every candle has its own date, so we'd inject a gap on
/// every column = visual noise, so we skip.
///
/// This is what produces the visible day-boundary dividers in
/// `render_session_dividers` even when no other filter is active —
/// the gap column reserves its own slot, no candle gets clobbered.
pub fn insert_day_gaps(data: &[Ohlc]) -> Vec<Ohlc> {
    if data.is_empty() {
        return Vec::new();
    }
    // Heuristic: on daily TF, unique dates ≈ candle count → ratio ≈ 1.
    // On intraday TF the ratio is candles per day (24 for 1h, 12 for 2h,
    // 6 for 4h, etc.).  Threshold of 2 lets 12h still get gaps and
    // safely excludes 1d.
    let mut unique = std::collections::HashSet::<&str>::new();
    let mut real = 0usize;
    for o in data.iter() {
        if o.is_gap || o.date.is_empty() {
            continue;
        }
        unique.insert(o.date.as_str());
        real += 1;
    }
    if unique.is_empty() || real / unique.len().max(1) < 2 {
        return data.to_vec();
    }

    let mut out: Vec<Ohlc> = Vec::with_capacity(data.len() + unique.len());
    let mut last_day: Option<String> = None;
    for o in data.iter() {
        if !o.is_gap {
            if let Some(prev) = &last_day {
                if prev != &o.date && !out.last().map(|c| c.is_gap).unwrap_or(false) {
                    out.push(Ohlc {
                        is_gap: true,
                        ..Ohlc::default()
                    });
                }
            }
            last_day = Some(o.date.clone());
        }
        out.push(o.clone());
    }
    out
}

#[derive(Clone)]
pub struct Symbol {
    pub ticker: &'static str,
    pub name: &'static str,
    pub data: Vec<Ohlc>,
}

/// Return the symbol list for `market` with empty OHLC data.
/// Real data is fetched from Binance (crypto) or Yahoo (others) at startup.
pub fn market_symbols(market: MarketType) -> Vec<Symbol> {
    match market {
        MarketType::UsStocks => vec![
            Symbol {
                ticker: "AAPL",
                name: "Apple Inc.",
                data: vec![],
            },
            Symbol {
                ticker: "GOOGL",
                name: "Alphabet Inc.",
                data: vec![],
            },
            Symbol {
                ticker: "MSFT",
                name: "Microsoft Corp.",
                data: vec![],
            },
            Symbol {
                ticker: "NVDA",
                name: "NVIDIA Corp.",
                data: vec![],
            },
            Symbol {
                ticker: "AMZN",
                name: "Amazon.com Inc.",
                data: vec![],
            },
            Symbol {
                ticker: "META",
                name: "Meta Platforms",
                data: vec![],
            },
            Symbol {
                ticker: "TSLA",
                name: "Tesla Inc.",
                data: vec![],
            },
            Symbol {
                ticker: "NFLX",
                name: "Netflix Inc.",
                data: vec![],
            },
            Symbol {
                ticker: "AMD",
                name: "Advanced Micro Dev.",
                data: vec![],
            },
            Symbol {
                ticker: "INTC",
                name: "Intel Corporation",
                data: vec![],
            },
        ],
        MarketType::Crypto => vec![
            Symbol {
                ticker: "BTC",
                name: "Bitcoin",
                data: vec![],
            },
            Symbol {
                ticker: "ETH",
                name: "Ethereum",
                data: vec![],
            },
            Symbol {
                ticker: "SOL",
                name: "Solana",
                data: vec![],
            },
            Symbol {
                ticker: "BNB",
                name: "BNB",
                data: vec![],
            },
            Symbol {
                ticker: "XRP",
                name: "Ripple",
                data: vec![],
            },
            Symbol {
                ticker: "ADA",
                name: "Cardano",
                data: vec![],
            },
            Symbol {
                ticker: "DOGE",
                name: "Dogecoin",
                data: vec![],
            },
            Symbol {
                ticker: "AVAX",
                name: "Avalanche",
                data: vec![],
            },
            Symbol {
                ticker: "DOT",
                name: "Polkadot",
                data: vec![],
            },
            Symbol {
                ticker: "LINK",
                name: "Chainlink",
                data: vec![],
            },
        ],
        MarketType::EuStocks => vec![
            Symbol {
                ticker: "ASML",
                name: "ASML Holding",
                data: vec![],
            },
            Symbol {
                ticker: "SAP",
                name: "SAP SE",
                data: vec![],
            },
            Symbol {
                ticker: "NESN",
                name: "Nestle",
                data: vec![],
            },
            Symbol {
                ticker: "NOVO",
                name: "Novo Nordisk",
                data: vec![],
            },
            Symbol {
                ticker: "LVMH",
                name: "LVMH",
                data: vec![],
            },
            Symbol {
                ticker: "OR",
                name: "L'Oreal",
                data: vec![],
            },
            Symbol {
                ticker: "SAN",
                name: "Sanofi",
                data: vec![],
            },
            Symbol {
                ticker: "DGE",
                name: "Diageo",
                data: vec![],
            },
            Symbol {
                ticker: "SHEL",
                name: "Shell",
                data: vec![],
            },
            Symbol {
                ticker: "ULVR",
                name: "Unilever",
                data: vec![],
            },
        ],
        MarketType::Forex => vec![
            Symbol {
                ticker: "EURUSD",
                name: "Euro / US Dollar",
                data: vec![],
            },
            Symbol {
                ticker: "GBPUSD",
                name: "Pound / US Dollar",
                data: vec![],
            },
            Symbol {
                ticker: "USDJPY",
                name: "US Dollar / Yen",
                data: vec![],
            },
            Symbol {
                ticker: "USDCHF",
                name: "US Dollar / Franc",
                data: vec![],
            },
            Symbol {
                ticker: "AUDUSD",
                name: "Aussie / US Dollar",
                data: vec![],
            },
            Symbol {
                ticker: "USDCAD",
                name: "US Dollar / Loonie",
                data: vec![],
            },
            Symbol {
                ticker: "NZDUSD",
                name: "Kiwi / US Dollar",
                data: vec![],
            },
            Symbol {
                ticker: "EURGBP",
                name: "Euro / Pound",
                data: vec![],
            },
            Symbol {
                ticker: "EURJPY",
                name: "Euro / Yen",
                data: vec![],
            },
            Symbol {
                ticker: "GBPJPY",
                name: "Pound / Yen",
                data: vec![],
            },
        ],
        MarketType::Commodities => vec![
            Symbol {
                ticker: "GC",
                name: "Gold (oz)",
                data: vec![],
            },
            Symbol {
                ticker: "SI",
                name: "Silver (oz)",
                data: vec![],
            },
            Symbol {
                ticker: "CL",
                name: "Crude Oil (bbl)",
                data: vec![],
            },
            Symbol {
                ticker: "NG",
                name: "Nat Gas (mmBtu)",
                data: vec![],
            },
            Symbol {
                ticker: "HG",
                name: "Copper (lb)",
                data: vec![],
            },
            Symbol {
                ticker: "ZC",
                name: "Corn (bu)",
                data: vec![],
            },
            Symbol {
                ticker: "ZW",
                name: "Wheat (bu)",
                data: vec![],
            },
            Symbol {
                ticker: "SB",
                name: "Sugar (lb)",
                data: vec![],
            },
            Symbol {
                ticker: "CC",
                name: "Cocoa (mt)",
                data: vec![],
            },
            Symbol {
                ticker: "KC",
                name: "Coffee (lb)",
                data: vec![],
            },
        ],
    }
}

pub fn fmt_volume(n: f64) -> String {
    if n >= 1e9 {
        format!("{:.2}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.2}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.2}K", n / 1e3)
    } else if n.fract() == 0.0 {
        format!("{}", n as u64)
    } else {
        // Coin-denominated volume (0.4 BTC): keep the fraction.
        format!("{:.2}", n)
    }
}

/// Decompose a Unix epoch **second** into `(year, month, day)` in UTC.
pub fn unix_secs_to_ymd(secs: i64) -> (i64, u32, u32) {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Convert a Unix epoch **second** to a UTC date string `YYYY-MM-DD`.
pub fn unix_secs_to_iso_date(secs: i64) -> String {
    let (y, m, d) = unix_secs_to_ymd(secs);
    format!("{:04}-{:02}-{:02}", y, m, d)
}
