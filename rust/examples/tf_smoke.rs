//! Test that all timeframes actually return data.
use dos::data::yahoo;
use dos::markets::Timeframe;

fn main() {
    for tf in [
        Timeframe::S15,
        Timeframe::M1,
        Timeframe::M5,
        Timeframe::H1,
        Timeframe::D1,
    ] {
        print!(
            "Yahoo AAPL {:>4} (range={}, interval={}): ",
            tf.name(),
            tf.yahoo_range(),
            tf.yahoo_interval()
        );
        match yahoo::fetch("AAPL", tf.yahoo_range(), tf.yahoo_interval()) {
            Ok(d) => println!(
                "OK {} candles, last close {}",
                d.len(),
                d.last().map(|o| o.close).unwrap_or(0.0)
            ),
            Err(e) => println!("ERR {}", e),
        }
    }
}
