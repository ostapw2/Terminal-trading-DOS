//! USDT-margined perpetual futures in the paper ledger: how an instrument is
//! named, what a position costs in margin, where it is liquidated, and what
//! funding moves every eight hours.
//!
//! A perpetual is its own instrument beside the spot coin: `BTCUSDT_PERP`
//! next to `BTCUSDT` (a ledger key; the exchange symbol is `BTCUSDT` on the
//! Futures host).  Its position may be short, it is held on margin
//! (`notional / leverage`), it is closed by force when the loss eats the
//! margin down to the maintenance level, and it pays or earns funding.  The
//! model is Binance USDT-M with isolated margin and the first tier of its
//! brackets; real brackets, cross margin and partial liquidations are not
//! modelled (the paper account says so where it shows them).

/// Ledger suffix of a perpetual.
pub const PERP_SUFFIX: &str = "_PERP";

/// Leverage bounds a position may be set to.
pub const MIN_LEVERAGE: f64 = 1.0;
pub const MAX_LEVERAGE: f64 = 125.0;
/// The default leverage of an automatic basket on perpetuals.
pub const DEFAULT_LEVERAGE: f64 = 3.0;

/// Maintenance margin, share of the notional (Binance tier 1 is 0.4 - 0.5 %).
pub const MAINT_RATE: f64 = 0.005;
/// What a liquidation costs on top of the lost margin: the clearance fee,
/// share of the notional.
pub const LIQ_FEE_RATE: f64 = 0.005;
/// Binance USDT-M, VIP 0, no BNB discount.
pub const FEE_MAKER_BPS: f64 = 2.0;
pub const FEE_TAKER_BPS: f64 = 5.0;
/// Funding is paid at 00:00, 08:00 and 16:00 UTC.
pub const FUNDING_EVERY_MS: i64 = 8 * 3_600_000;

/// `BTCUSDT` -> `BTCUSDT_PERP` (an instrument already named so stays).
pub fn perp(symbol: &str) -> String {
    if is_perp(symbol) {
        symbol.to_string()
    } else {
        format!("{symbol}{PERP_SUFFIX}")
    }
}

pub fn is_perp(symbol: &str) -> bool {
    symbol.ends_with(PERP_SUFFIX)
}

/// The exchange symbol of an instrument: `BTCUSDT_PERP` -> `BTCUSDT`.
pub fn exchange_symbol(symbol: &str) -> &str {
    symbol.strip_suffix(PERP_SUFFIX).unwrap_or(symbol)
}

pub fn clamp_leverage(l: f64) -> f64 {
    if l.is_finite() {
        l.clamp(MIN_LEVERAGE, MAX_LEVERAGE)
    } else {
        MIN_LEVERAGE
    }
}

/// Margin a position of `notional` dollars ties up at leverage `lev`.
pub fn initial_margin(notional: f64, lev: f64) -> f64 {
    notional.abs() / clamp_leverage(lev)
}

/// The price at which an isolated position entered at `entry` is closed by
/// force: the loss has eaten the margin down to the maintenance level.
/// `None` for a price that makes no sense; a long at 1x is "liquidated" only
/// near zero.
pub fn liquidation_price(entry: f64, long: bool, lev: f64) -> Option<f64> {
    if !(entry.is_finite() && entry > 0.0) {
        return None;
    }
    let lev = clamp_leverage(lev);
    let p = if long {
        entry * (1.0 - 1.0 / lev + MAINT_RATE)
    } else {
        entry * (1.0 + 1.0 / lev - MAINT_RATE)
    };
    (p > 0.0).then_some(p)
}

/// Has `price` reached the liquidation price of a position?
pub fn is_liquidated(entry: f64, long: bool, lev: f64, price: f64) -> bool {
    match liquidation_price(entry, long, lev) {
        Some(p) if long => price <= p,
        Some(p) => price >= p,
        None => false,
    }
}

/// How many funding times lie in `(from_ms, to_ms]`.
pub fn funding_times_between(from_ms: i64, to_ms: i64) -> i64 {
    if to_ms <= from_ms {
        return 0;
    }
    to_ms.div_euclid(FUNDING_EVERY_MS) - from_ms.div_euclid(FUNDING_EVERY_MS)
}

/// The cash a signed position (`qty` > 0 long) moves at one funding time:
/// with a positive rate longs pay shorts.
pub fn funding_payment(qty: f64, mark: f64, rate: f64) -> f64 {
    -qty * mark * rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(perp("BTCUSDT"), "BTCUSDT_PERP");
        assert_eq!(perp("BTCUSDT_PERP"), "BTCUSDT_PERP");
        assert!(is_perp("ETHUSDT_PERP") && !is_perp("ETHUSDT"));
        assert_eq!(exchange_symbol("ETHUSDT_PERP"), "ETHUSDT");
        assert_eq!(exchange_symbol("ETHUSDT"), "ETHUSDT");
    }

    #[test]
    fn liquidation_sits_closer_the_higher_the_leverage() {
        // 10x long at 100: the margin is 10, the maintenance 0.5: gone at 90.5.
        let p = liquidation_price(100.0, true, 10.0).unwrap();
        assert!((p - 90.5).abs() < 1e-9, "{p}");
        // 10x short: 109.5.
        let s = liquidation_price(100.0, false, 10.0).unwrap();
        assert!((s - 109.5).abs() < 1e-9, "{s}");
        // 3x long: about a third down.
        let p3 = liquidation_price(100.0, true, 3.0).unwrap();
        assert!(p3 < 68.0 && p3 > 66.0, "{p3}");
        // 1x long: only a fall of 99.5 % does it.
        assert!(liquidation_price(100.0, true, 1.0).unwrap() < 1.0);
        // ... but a 1x short can (the price would have to double).
        assert!(liquidation_price(100.0, false, 1.0).unwrap() > 199.0);
        assert!(is_liquidated(100.0, true, 10.0, 90.4));
        assert!(!is_liquidated(100.0, true, 10.0, 91.0));
        assert!(is_liquidated(100.0, false, 10.0, 109.6));
        assert!(!is_liquidated(100.0, false, 10.0, 105.0));
        assert!(liquidation_price(0.0, true, 5.0).is_none());
    }

    #[test]
    fn margin_and_leverage_are_clamped() {
        assert_eq!(initial_margin(1000.0, 4.0), 250.0);
        assert_eq!(initial_margin(-1000.0, 4.0), 250.0);
        assert_eq!(initial_margin(1000.0, 0.0), 1000.0);
        assert_eq!(initial_margin(1000.0, 1000.0), 8.0);
        assert_eq!(clamp_leverage(f64::NAN), 1.0);
    }

    #[test]
    fn funding_times_are_counted_and_longs_pay_a_positive_rate() {
        const H: i64 = 3_600_000;
        assert_eq!(funding_times_between(0, 8 * H), 1);
        assert_eq!(funding_times_between(1, 8 * H - 1), 0);
        assert_eq!(funding_times_between(7 * H, 17 * H), 2);
        assert_eq!(funding_times_between(5, 5), 0);
        assert_eq!(funding_times_between(10, 5), 0);
        // A $10,000 long at +0.01 % pays $1; a short earns it.
        assert!((funding_payment(1.0, 10_000.0, 0.0001) + 1.0).abs() < 1e-9);
        assert!((funding_payment(-1.0, 10_000.0, 0.0001) - 1.0).abs() < 1e-9);
    }
}
