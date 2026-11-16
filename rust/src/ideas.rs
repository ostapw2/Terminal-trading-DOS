//! Ideas: what the Compare page found, as rows to look at and act on.
//!
//! The scanner's findings (pairs, pumps, dumps, stretched coins, falling
//! knives), ready synthetic charts and linked pairs from the heat map all
//! become an [`Idea`]: a row with a tag, what to chart, and — once the chart's
//! candles are in — a [`Plan`]: a verdict, numbered steps and the levels
//! (entry, target, stop) drawn on the chart.
//!
//! A plan never promises: where the coin's own history can be checked (a
//! stretch back to its mean, a gap closing, a knife's next day), the steps
//! say how often it happened and how many times that is out of; where it
//! cannot (a live pump), the verdict is "watch", not "act".  The judge
//! (Compare mode 3) is one key away for the real test ([`Idea::test`]).

use crate::live_ideas as live;
use crate::markets::Ohlc;
use crate::movers::Mover;
use crate::scanner::{Knife, PairIdea, Scan, Stretch};
use crate::widgets::fmt::fmt_value;

/// What kind of row it is: the tag in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pair,
    Pump,
    Dump,
    /// Stretched down: far below its own normal.
    Dip,
    /// Stretched up.
    High,
    Knife,
    /// A ready synthetic chart.
    Chart,
    /// Two symbols from the heat map.
    Link,
    /// A rule the search tried ("when X, is Y higher h later?").
    Rule,
    /// Live: among the biggest moves of the last minutes, up / down.
    Gain,
    Loss,
    /// Live: a coin behind the one it has moved with this hour.
    Lag,
    /// Live: two coins moving as one (or opposite) this hour.
    Live,
    /// Live: a group of coins moving as one this hour.
    Group,
    /// Live: a linked pair whose ratio is stretched this hour.
    Spread,
    /// Options: put-call parity with an edge (a conversion / reversal).
    Parity,
    /// Options: a box spread with an edge.
    Box,
    /// Live: a synthetic instrument (coins that moved as one, traded as one).
    Syn,
    /// Live: one synthetic against another, ratio stretched.
    SynPair,
}

use crate::i18n::pick;

impl Kind {
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Pair => pick("PAIR", "ПАРА"),
            Kind::Pump => pick("PUMP", "ПАМП"),
            Kind::Dump => pick("DUMP", "ДАМП"),
            Kind::Dip => pick("DIP", "ЯМА"),
            Kind::High => pick("HIGH", "ПІК"),
            Kind::Knife => pick("KNIFE", "НІЖ"),
            Kind::Chart => pick("CHART", "ГРАФ"),
            Kind::Link => pick("LINK", "ЗВЯЗ"),
            Kind::Rule => pick("RULE", "ПРАВ"),
            Kind::Gain => pick("GAIN", "РІСТ"),
            Kind::Loss => pick("LOSS", "ПАД"),
            Kind::Lag => pick("LAG", "ВІДС"),
            Kind::Live => pick("LIVE", "ЖИВЕ"),
            Kind::Group => pick("GROUP", "ГРУПА"),
            Kind::Spread => pick("SPREAD", "РОЗРИВ"),
            Kind::Parity => pick("PARITY", "ПАРИТ"),
            Kind::Box => pick("BOX", "БОКС"),
            Kind::Syn => pick("SYN", "СИНТ"),
            Kind::SynPair => pick("SYNPAIR", "СИНПАР"),
        }
    }
}

/// A ready synthetic chart: a formula and how to read it.
#[derive(Debug, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub formula: &'static str,
    /// Timeframe it reads best on.
    pub tf: &'static str,
    /// What the line is, in one sentence.
    pub about: &'static str,
    /// What it means when the line is high / low against its usual.
    pub high: &'static str,
    pub low: &'static str,
    /// The same in Ukrainian: name, about, high, low.
    pub uk: [&'static str; 4],
}

impl Preset {
    pub fn title(&self) -> &'static str {
        crate::i18n::pick(self.name, self.uk[0])
    }
    pub fn about(&self) -> &'static str {
        crate::i18n::pick(self.about, self.uk[1])
    }
    pub fn high(&self) -> &'static str {
        crate::i18n::pick(self.high, self.uk[2])
    }
    pub fn low(&self) -> &'static str {
        crate::i18n::pick(self.low, self.uk[3])
    }
}

/// The gallery of Compare mode 7.  Eight sources at most per formula.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "BTC against ETH",
        formula: "=BTC/ETH",
        tf: "4h",
        about: "How many ETH one BTC buys. Rising: money prefers BTC; falling: ETH leads.",
        high: "BTC is unusually strong against ETH",
        low: "ETH is unusually strong against BTC",
        uk: ["BTC до ETH", "Скільки ETH коштує один BTC. Росте: гроші йдуть у BTC; падає: веде ETH.", "BTC незвично сильний проти ETH", "ETH незвично сильний проти BTC"],
    },
    Preset {
        name: "Altcoins vs BTC",
        formula: "=(IDX(SOL)+IDX(BNB)+IDX(XRP)+IDX(ADA)+IDX(AVAX)+IDX(LINK)+IDX(DOGE))/7-IDX(BTC)",
        tf: "4h",
        about: "Seven large altcoins, each from 100, minus BTC from 100. Above zero and rising: an altcoin season.",
        high: "Altcoins beat BTC more than usual",
        low: "BTC beats the altcoins more than usual",
        uk: ["Альткоїни проти BTC", "Сім великих альткоїнів (кожен від 100) мінус BTC від 100. Вище нуля й росте: сезон альтів.", "Альткоїни обганяють BTC більше звичайного", "BTC обганяє альткоїни більше звичайного"],
    },
    Preset {
        name: "Meme coins index",
        formula: "=(IDX(DOGE)+IDX(SHIB)+IDX(PEPE)+IDX(WIF)+IDX(BONK)+IDX(FLOKI))/6",
        tf: "1h",
        about: "Six meme coins, each from 100, averaged: the market's appetite for risk.",
        high: "Appetite for risk is unusually high",
        low: "Meme coins are unusually weak: risk is off",
        uk: ["Індекс мемкоїнів", "Шість мемкоїнів (кожен від 100), середнє: апетит ринку до ризику.", "Апетит до ризику незвично високий", "Мемкоїни незвично слабкі: ризик вимкнено"],
    },
    Preset {
        name: "Futures premium, BTC",
        formula: "=(F:BTCUSDT/BTC-1)*100",
        tf: "1h",
        about: "How much more BTC costs on Futures than on Spot, %. High: longs pay up (crowded); below zero: fear.",
        high: "Futures buyers pay an unusual premium: crowded longs",
        low: "Futures trade unusually cheap: fear or crowded shorts",
        uk: ["Преміум ф'ючерса BTC", "На скільки % BTC дорожчий на ф'ючерсах, ніж на споті. Високо: лонги переплачують; нижче нуля: страх.", "Покупці ф'ючерса платять незвичну премію: переповнені лонги", "Ф'ючерс незвично дешевий: страх або переповнені шорти"],
    },
    Preset {
        name: "BTC/ETH gap in sd",
        formula: "=PAIRZ(BTC,ETH,100)",
        tf: "1h",
        about: "The BTC/ETH ratio against its last 100 candles, in standard deviations. Beyond +-2: unusually far apart.",
        high: "BTC is far above its usual ratio to ETH",
        low: "BTC is far below its usual ratio to ETH",
        uk: ["Розрив BTC/ETH у sd", "Співвідношення BTC/ETH проти останніх 100 свічок, у стандартних відхиленнях. Понад +-2: незвично далеко.", "BTC далеко вище звичайного співвідношення до ETH", "BTC далеко нижче звичайного співвідношення до ETH"],
    },
    Preset {
        name: "BTC volume surge",
        formula: "=VOLZ(BTC,50)",
        tf: "1h",
        about: "BTC's volume against its last 50 candles, in standard deviations. Spikes mark news and liquidations.",
        high: "Volume is unusually heavy",
        low: "Volume is unusually light",
        uk: ["Сплеск обсягу BTC", "Обсяг BTC проти останніх 50 свічок, у sd. Піки: новини й ліквідації.", "Обсяг незвично великий", "Обсяг незвично малий"],
    },
    Preset {
        name: "BTC-ETH link",
        formula: "=RCORR(BTC,ETH,50)",
        tf: "1h",
        about: "Correlation of BTC's and ETH's candle moves over 50 candles (-1..+1). A drop: the two decouple.",
        high: "BTC and ETH move together more than usual",
        low: "BTC and ETH have decoupled more than usual",
        uk: ["Зв'язок BTC-ETH", "Кореляція рухів BTC і ETH за 50 свічок (-1..+1). Падіння: вони розходяться.", "BTC і ETH рухаються разом більше звичайного", "BTC і ETH розійшлися більше звичайного"],
    },
    Preset {
        name: "SOL beyond BTC",
        formula: "=RESID(SOL,BTC,100)",
        tf: "1h",
        about: "SOL's move each candle that BTC's move does not explain, %: SOL's own news.",
        high: "SOL moves up on its own, more than usual",
        low: "SOL moves down on its own, more than usual",
        uk: ["SOL без BTC", "Рух SOL за свічку, якого не пояснює BTC, %: власні новини SOL.", "SOL росте сам по собі більше звичайного", "SOL падає сам по собі більше звичайного"],
    },
    Preset {
        name: "ETH distance from its mean",
        formula: "=DEV(ETH,50)",
        tf: "1h",
        about: "ETH against its 50-candle average, %.",
        high: "ETH is unusually far above its average",
        low: "ETH is unusually far below its average",
        uk: ["ETH від свого середнього", "ETH проти свого середнього за 50 свічок, %.", "ETH незвично далеко вище середнього", "ETH незвично далеко нижче середнього"],
    },
    Preset {
        name: "BTC against the S&P 500",
        formula: "=BTC/Y:SPY",
        tf: "1d",
        about: "BTC priced in shares of the S&P 500 fund (Yahoo, daily). Rising: crypto beats stocks.",
        high: "BTC beats stocks more than usual",
        low: "Stocks beat BTC more than usual",
        uk: ["BTC проти S&P 500", "BTC у частках фонду S&P 500 (Yahoo, дні). Росте: крипта обганяє акції.", "BTC обганяє акції більше звичайного", "Акції обганяють BTC більше звичайного"],
    },
];

/// The thing a row stands for.
#[derive(Clone, Debug, PartialEq)]
pub enum What {
    Pair(PairIdea),
    Stretch(Stretch),
    Knife(Knife),
    Mover(Mover),
    Chart(usize),
    Link {
        a: String,
        b: String,
        r: f64,
    },
    Rule(Rule),
    /// Live rows (`crate::live_ideas`): today's picture from the stream.
    Move(live::Move),
    Laggard(live::Laggard),
    LiveLink(live::Link),
    Group(live::Group),
    Spread(live::Spread),
    /// Options rows (`crate::options`): bid / ask of one fetch.
    Parity(crate::options::Parity),
    Box(crate::options::BoxSpread),
    Synthetic(live::Synthetic),
    SynPair(live::SynPair),
}

/// One side of a judged rule: events, how many rose, the usual share.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Odds {
    pub events: usize,
    pub up: usize,
    pub base_hit: f64,
}

impl Odds {
    fn hit(&self) -> f64 {
        self.up as f64 / self.events.max(1) as f64
    }
    fn words(&self) -> String {
        pick(
            format!(
                "higher {} of {} times ({:.0}%) vs {:.0}% usually",
                self.up,
                self.events,
                self.hit() * 100.0,
                self.base_hit * 100.0
            ),
            format!(
                "вище {} з {} разів ({:.0}%) проти {:.0}% звичайно",
                self.up,
                self.events,
                self.hit() * 100.0,
                self.base_hit * 100.0
            ),
        )
    }
}

/// A search hit, as far as a plan needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub cond: String,
    pub target: String,
    pub h: usize,
    /// Judged: older part (where it was picked) and newer part (unseen).
    pub judged: Option<(Odds, Odds)>,
    pub survives: bool,
    pub reason: String,
    /// Screened only: the older part's events.
    pub older_events: usize,
}

impl Rule {
    pub fn from_hit(h: &crate::search::Hit) -> Rule {
        let odds = |s: &crate::judge::Side| Odds {
            events: s.events,
            up: s.up,
            base_hit: s.base_hit,
        };
        Rule {
            cond: h.q.cond.clone(),
            target: h.q.target.clone(),
            h: h.q.h,
            judged: h.verdict.as_ref().map(|v| (odds(&v.older), odds(&v.newer))),
            survives: h.verdict.as_ref().is_some_and(|v| v.survives),
            reason: h
                .verdict
                .as_ref()
                .map_or(String::new(), |v| v.reason.clone()),
            older_events: h.older_events,
        }
    }

    /// The condition split at its comparison: the line it watches and the
    /// threshold ("Z(DOGE,50)", ">", 2.5); `None` for other shapes.
    pub fn split(&self) -> Option<(String, &'static str, f64)> {
        for op in [">=", "<=", ">", "<"] {
            if let Some((l, r)) = self.cond.split_once(op) {
                let v: f64 = r.trim().parse().ok()?;
                let op: &'static str = match op {
                    ">=" => ">=",
                    "<=" => "<=",
                    ">" => ">",
                    _ => "<",
                };
                return Some((l.trim().to_string(), op, v));
            }
        }
        None
    }
}

/// One row of the Compare list.
#[derive(Clone, Debug, PartialEq)]
pub struct Idea {
    pub what: What,
    /// The timeframe its finding is on ("1h", "15m").
    pub tf: String,
}

fn short(s: &str) -> &str {
    s.strip_suffix("USDT").unwrap_or(s)
}

impl Idea {
    pub fn kind(&self) -> Kind {
        match &self.what {
            What::Pair(_) => Kind::Pair,
            What::Stretch(s) if s.z < 0.0 => Kind::Dip,
            What::Stretch(_) => Kind::High,
            What::Knife(_) => Kind::Knife,
            What::Mover(m) if m.up => Kind::Pump,
            What::Mover(_) => Kind::Dump,
            What::Chart(_) => Kind::Chart,
            What::Link { .. } => Kind::Link,
            What::Rule(_) => Kind::Rule,
            What::Move(m) if m.pct >= 0.0 => Kind::Gain,
            What::Move(_) => Kind::Loss,
            What::Laggard(_) => Kind::Lag,
            What::LiveLink(_) => Kind::Live,
            What::Group(_) => Kind::Group,
            What::Spread(_) => Kind::Spread,
            What::Parity(_) => Kind::Parity,
            What::Box(_) => Kind::Box,
            What::Synthetic(_) => Kind::Syn,
            What::SynPair(_) => Kind::SynPair,
        }
    }

    /// The row's name: "buy SOL / sell AVAX", "BNB", "BTC against ETH".
    pub fn title(&self) -> String {
        match &self.what {
            What::Pair(p) => pick(
                format!("buy {} / sell {}", short(&p.long), short(&p.short)),
                format!("купи {} / продай {}", short(&p.long), short(&p.short)),
            ),
            What::Stretch(s) => short(&s.symbol).to_string(),
            What::Knife(k) => short(&k.symbol).to_string(),
            What::Mover(m) => short(&m.symbol).to_string(),
            What::Chart(i) => PRESETS.get(*i).map_or("?", |p| p.title()).to_string(),
            What::Link { a, b, .. } => format!("{} / {}", short(a), short(b)),
            What::Rule(r) => format!("{} -> {} +{}", r.cond, r.target, r.h).replace("USDT", ""),
            What::Move(m) => short(&m.symbol).to_string(),
            What::Laggard(l) => pick(
                format!("{} behind {}", short(&l.follower), short(&l.leader)),
                format!("{} відстає від {}", short(&l.follower), short(&l.leader)),
            ),
            What::LiveLink(l) => format!("{} / {}", short(&l.a), short(&l.b)),
            What::Group(g) => {
                let names: Vec<&str> = g.members.iter().take(4).map(|m| short(m)).collect();
                if g.members.len() > 4 {
                    format!("{} +{}", names.join(" "), g.members.len() - 4)
                } else {
                    names.join(" ")
                }
            }
            What::Spread(sp) => {
                let (l, sh) = sp.long_short();
                pick(
                    format!("buy {} / sell {}", short(l), short(sh)),
                    format!("купи {} / продай {}", short(l), short(sh)),
                )
            }
            What::Parity(p) => p.words(),
            What::Box(b) => b.words(),
            What::Synthetic(sy) => pick(
                format!("{} ({} coins)", sy.name, sy.members.len()),
                format!("{} ({} монет)", sy.name, sy.members.len()),
            ),
            What::SynPair(p) => {
                let ((long, _), (sh, _)) = p.long_short();
                pick(
                    format!("buy {long} / sell {sh}"),
                    format!("купи {long} / продай {sh}"),
                )
            }
        }
    }

    /// The row's numbers, short.
    pub fn facts(&self) -> String {
        match &self.what {
            What::Pair(p) => pick(
                format!("gap {:+.1} sd, half-life {:.0}", p.z, p.half_life),
                format!("розрив {:+.1} sd, піврозпад {:.0}", p.z, p.half_life),
            ),
            What::Stretch(s) => format!("{:+.1} sd, {:+.1}%", s.z, s.change_pct),
            What::Knife(k) => pick(
                format!("{:+.0}% day, vol x{:.1}", k.day_pct, k.volume_x),
                format!("{:+.0}% за добу, обсяг x{:.1}", k.day_pct, k.volume_x),
            ),
            What::Mover(m) => pick(
                format!(
                    "{:+.1}% in {}m, vol x{:.1}",
                    m.change_pct, m.window_min, m.volume_x
                ),
                format!(
                    "{:+.1}% за {} хв, обсяг x{:.1}",
                    m.change_pct, m.window_min, m.volume_x
                ),
            ),
            What::Chart(i) => PRESETS.get(*i).map_or("", |p| p.tf).to_string(),
            What::Link { r, .. } => format!("r {r:+.2}"),
            What::Rule(r) => match r.judged {
                Some((_, n)) if n.events > 0 => {
                    format!("{:.0}% vs {:.0}%", n.hit() * 100.0, n.base_hit * 100.0)
                }
                _ => pick("screened", "відібрано").to_string(),
            },
            What::Move(m) => pick(
                format!(
                    "{:+.1}% in {}m, vol x{:.1}",
                    m.pct, m.window_min, m.volume_x
                ),
                format!(
                    "{:+.1}% за {} хв, обсяг x{:.1}",
                    m.pct, m.window_min, m.volume_x
                ),
            ),
            What::Laggard(l) => pick(
                format!(
                    "{:+.1}% vs {:+.1}% in {}m, r {:.2}",
                    l.follow_pct, l.lead_pct, l.window_min, l.r
                ),
                format!(
                    "{:+.1}% проти {:+.1}% за {} хв, r {:.2}",
                    l.follow_pct, l.lead_pct, l.window_min, l.r
                ),
            ),
            What::LiveLink(l) => pick(
                format!("r {:+.2} over {}m", l.r, l.minutes),
                format!("r {:+.2} за {} хв", l.r, l.minutes),
            ),
            What::Group(g) => match g.z_hedge {
                Some(z) => pick(
                    format!(
                        "{} coins, r {:.2}, {:+.1}%, {z:+.1} sd vs BTC",
                        g.members.len(),
                        g.r_mean,
                        g.pct
                    ),
                    format!(
                        "{} монет, r {:.2}, {:+.1}%, {z:+.1} sd проти BTC",
                        g.members.len(),
                        g.r_mean,
                        g.pct
                    ),
                ),
                None => pick(
                    format!(
                        "{} coins, r {:.2}, {:+.1}%",
                        g.members.len(),
                        g.r_mean,
                        g.pct
                    ),
                    format!(
                        "{} монет, r {:.2}, {:+.1}%",
                        g.members.len(),
                        g.r_mean,
                        g.pct
                    ),
                ),
            },
            What::Spread(sp) => pick(
                format!("{:+.1} sd, r {:.2} over {}m", sp.z, sp.r, sp.minutes),
                format!("{:+.1} sd, r {:.2} за {} хв", sp.z, sp.r, sp.minutes),
            ),
            What::Parity(p) => format!(
                "{:+.0} bps net, {:.1}%/y, {:.0}d",
                p.net_bps(),
                p.annual_pct(),
                p.days
            ),
            What::Box(b) => format!(
                "{:+.0} bps net, {:.1}%/y, {:.0}d",
                b.net_bps(),
                b.annual_pct(),
                b.days
            ),
            What::Synthetic(sy) => match sy.z_hedge {
                Some(z) => format!("r >= {:.2}, {:+.1}%, {z:+.1} sd vs BTC", sy.tier_r, sy.pct),
                None => format!("r >= {:.2}, {:+.1}%", sy.tier_r, sy.pct),
            },
            What::SynPair(p) => pick(
                format!("{:+.1} sd, r {:.2} over {}m", p.z, p.r, p.minutes),
                format!("{:+.1} sd, r {:.2} за {} хв", p.z, p.r, p.minutes),
            ),
        }
    }

    /// The main symbol (Binance form), when there is one.
    pub fn symbol(&self) -> Option<&str> {
        match &self.what {
            What::Stretch(s) => Some(&s.symbol),
            What::Knife(k) => Some(&k.symbol),
            What::Mover(m) => Some(&m.symbol),
            What::Move(m) => Some(&m.symbol),
            _ => None,
        }
    }

    /// What its chart shows: a formula (`=...`), and on which timeframe.
    /// A pair's chart is its gap in sd, which the scan already holds
    /// ([`pair_candles`]): its formula is what opens on the big chart.
    pub fn chart(&self) -> (String, String) {
        match &self.what {
            What::Pair(p) => (
                format!(
                    "=Z(LN({})-{:.3}*LN({}),200)",
                    short(&p.a),
                    p.beta,
                    short(&p.b)
                ),
                self.tf.clone(),
            ),
            What::Stretch(s) => (format!("={}", short(&s.symbol)), self.tf.clone()),
            What::Knife(k) => (format!("={}", short(&k.symbol)), self.tf.clone()),
            What::Mover(m) => (format!("={}", short(&m.symbol)), "5m".into()),
            What::Chart(i) => PRESETS
                .get(*i)
                .map_or((String::new(), self.tf.clone()), |p| {
                    (p.formula.to_string(), p.tf.to_string())
                }),
            What::Link { a, b, .. } => (format!("={}/{}", short(a), short(b)), self.tf.clone()),
            What::Rule(r) => {
                let line = r.split().map_or(r.target.clone(), |(l, _, _)| l);
                (format!("={line}"), self.tf.clone())
            }
            What::Move(m) => (format!("={}", short(&m.symbol)), self.tf.clone()),
            What::Laggard(l) => (
                format!("={}/{}", short(&l.follower), short(&l.leader)),
                self.tf.clone(),
            ),
            What::LiveLink(l) => (format!("={}/{}", short(&l.a), short(&l.b)), self.tf.clone()),
            What::Group(g) => (group_formula(&g.members), self.tf.clone()),
            What::Spread(sp) => (
                format!("={}/{}", short(&sp.a), short(&sp.b)),
                self.tf.clone(),
            ),
            What::Parity(p) => (format!("={}", short(&p.underlying)), self.tf.clone()),
            What::Box(b) => (format!("={}", short(&b.underlying)), self.tf.clone()),
            What::Synthetic(sy) => (group_formula(&sy.members), self.tf.clone()),
            What::SynPair(p) => (
                syn_pair_formula(&p.a_members, &p.b_members),
                self.tf.clone(),
            ),
        }
    }

    /// A judge question that tests the idea: (condition, target, candles).
    /// `None` when there is nothing sensible to test.
    pub fn test(&self) -> Option<(String, String, usize)> {
        match &self.what {
            What::Pair(p) => {
                let (a, b) = (short(&p.a), short(&p.b));
                let gap = format!("LN({a})-{:.3}*LN({b})", p.beta);
                let thr = p.z.abs().max(2.0);
                let h = p.history.horizon.max(4);
                Some(if p.z > 0.0 {
                    (
                        format!("Z({gap},200) > {thr:.1}"),
                        format!("{:.3}*LN({b})-LN({a})", p.beta),
                        h,
                    )
                } else {
                    (format!("Z({gap},200) < -{thr:.1}"), gap, h)
                })
            }
            What::Stretch(s) => {
                let sym = short(&s.symbol);
                let thr = s.z.abs().max(2.0);
                Some(if s.z < 0.0 {
                    (format!("Z({sym},50) < -{thr:.1}"), sym.to_string(), 24)
                } else {
                    (format!("Z({sym},50) > {thr:.1}"), sym.to_string(), 24)
                })
            }
            What::Knife(k) => {
                let sym = short(&k.symbol);
                Some((format!("CHG({sym},24) < -8"), sym.to_string(), 24))
            }
            What::Rule(r) => Some((r.cond.clone(), r.target.clone(), r.h)),
            What::Laggard(l) => {
                // When the leader is far above the follower (their ratio
                // stretched), does the follower catch up?  Down moves: the
                // other way round.
                let (lead, fol) = (short(&l.leader), short(&l.follower));
                let up = l.lead_pct > 0.0;
                let ratio = format!("{lead}/{fol}");
                let cond = if up {
                    format!("Z({ratio},50) > 2")
                } else {
                    format!("Z({ratio},50) < -2")
                };
                let target = if up {
                    fol.to_string()
                } else {
                    lead.to_string()
                };
                Some((cond, target, 15))
            }
            What::Move(m) => {
                let sym = short(&m.symbol);
                let thr = (m.pct.abs() / 2.0).clamp(1.0, 5.0);
                let w = m.window_min as usize;
                Some(if m.pct >= 0.0 {
                    (format!("CHG({sym},{w}) > {thr:.1}"), sym.to_string(), w)
                } else {
                    (format!("CHG({sym},{w}) < -{thr:.1}"), sym.to_string(), w)
                })
            }
            What::Spread(sp) => {
                // When the ratio is this stretched, does the cheap one gain
                // on the rich one over the next hour?
                let (a, b) = (short(&sp.a), short(&sp.b));
                let thr = sp.z.abs().max(2.0);
                let ratio = format!("{a}/{b}");
                Some(if sp.z > 0.0 {
                    (format!("Z({ratio},60) > {thr:.1}"), format!("{b}/{a}"), 60)
                } else {
                    (format!("Z({ratio},60) < -{thr:.1}"), ratio, 60)
                })
            }
            _ => None,
        }
    }
}

/// The group as one line: the mean of its members rebased to 100 (a
/// formula takes eight sources at most, so the first eight).
pub fn group_formula(members: &[String]) -> String {
    let parts: Vec<String> = members
        .iter()
        .take(crate::formula::MAX_SOURCES)
        .map(|m| format!("IDX({})", short(m)))
        .collect();
    format!("=({})/{}", parts.join("+"), parts.len().max(1))
}

/// A synthetic against another as one line: the mean of the first four of
/// each (a formula takes eight sources at most), A over B.
pub fn syn_pair_formula(a: &[String], b: &[String]) -> String {
    let half = crate::formula::MAX_SOURCES / 2;
    let side = |m: &[String]| {
        let parts: Vec<String> = m
            .iter()
            .take(half)
            .map(|x| format!("IDX({})", short(x)))
            .collect();
        format!("(({})/{})", parts.join("+"), parts.len().max(1))
    };
    format!("={}/{}", side(a), side(b))
}

/// The live picture as rows: laggards first (a coin that may still move),
/// then groups, the biggest moves up and down, then the links.
pub fn from_live(l: &live::Live) -> Vec<Idea> {
    let row = |what| Idea {
        what,
        tf: "1m".to_string(),
    };
    let mut out: Vec<Idea> = l
        .laggards
        .iter()
        .cloned()
        .map(What::Laggard)
        .map(row)
        .collect();
    out.extend(l.groups.iter().cloned().map(What::Group).map(row));
    out.extend(l.gainers.iter().cloned().map(What::Move).map(row));
    out.extend(l.losers.iter().cloned().map(What::Move).map(row));
    out.extend(
        l.spreads
            .iter()
            .take(live::TOP)
            .cloned()
            .map(What::Spread)
            .map(row),
    );
    out.extend(
        l.syn_pairs
            .iter()
            .take(live::TOP)
            .cloned()
            .map(What::SynPair)
            .map(row),
    );
    out.extend(
        l.synthetics
            .iter()
            .take(live::TOP)
            .cloned()
            .map(What::Synthetic)
            .map(row),
    );
    out.extend(l.links.iter().cloned().map(What::LiveLink).map(row));
    out
}

/// The options picture as rows: parities, then boxes (the best first).
pub fn from_options(o: &crate::options::Options) -> Vec<Idea> {
    let row = |what| Idea {
        what,
        tf: "1h".to_string(),
    };
    let mut out: Vec<Idea> = o
        .parity
        .iter()
        .cloned()
        .map(What::Parity)
        .map(row)
        .collect();
    out.extend(o.boxes.iter().cloned().map(What::Box).map(row));
    out
}

/// Every finding of a scan as rows: pairs first (they carry a plan with
/// history), then the live movers, then stretched coins, then knives.
pub fn from_scan(scan: &Scan) -> Vec<Idea> {
    let tf = if scan.tf.is_empty() {
        "1h".to_string()
    } else {
        scan.tf.clone()
    };
    let row = |what| Idea {
        what,
        tf: tf.clone(),
    };
    let mut out: Vec<Idea> = scan
        .pairs
        .iter()
        .cloned()
        .map(What::Pair)
        .map(row)
        .collect();
    out.extend(scan.pumps.iter().cloned().map(What::Mover).map(row));
    out.extend(scan.dumps.iter().cloned().map(What::Mover).map(row));
    out.extend(scan.down.iter().cloned().map(What::Stretch).map(row));
    out.extend(scan.up.iter().cloned().map(What::Stretch).map(row));
    out.extend(scan.knives.iter().cloned().map(What::Knife).map(row));
    out
}

/// Every pair of a correlation matrix as rows, the strongest links (either
/// way) first; at most `MAX_LINKS`.
pub fn from_matrix(m: &crate::compare::Matrix, tf: &str) -> Vec<Idea> {
    let n = m.names.len();
    let mut v: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            if let Some(r) = m.r.get(i).and_then(|row| row.get(j)).copied().flatten() {
                v.push((r, i, j));
            }
        }
    }
    v.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
    v.into_iter()
        .take(MAX_LINKS)
        .map(|(r, i, j)| Idea {
            what: What::Link {
                a: m.names[i].clone(),
                b: m.names[j].clone(),
                r,
            },
            tf: tf.to_string(),
        })
        .collect()
}

/// A search report as rows, in its order (survivors first).
pub fn from_search(r: &crate::search::Report, tf: &str) -> Vec<Idea> {
    r.hits
        .iter()
        .map(|h| Idea {
            what: What::Rule(Rule::from_hit(h)),
            tf: tf.to_string(),
        })
        .collect()
}

/// Most links listed from a heat map.
pub const MAX_LINKS: usize = 45;

/// The gallery as rows.
pub fn presets() -> Vec<Idea> {
    (0..PRESETS.len())
        .map(|i| Idea {
            what: What::Chart(i),
            tf: PRESETS[i].tf.to_string(),
        })
        .collect()
}

/// A pair's gap path as line candles (no times: the chart has no dates).
pub fn pair_candles(p: &PairIdea) -> Vec<Ohlc> {
    p.z_path
        .iter()
        .map(|&z| Ohlc {
            open: z,
            high: z,
            low: z,
            close: z,
            volume: 0.0,
            date: String::new(),
            time_ms: 0,
            is_gap: false,
        })
        .collect()
}

/// How strongly a plan says to act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The idea has history on its side.
    Act,
    /// Look, but there is no evidence either way yet.
    Watch,
    /// History or the setup says no.
    Avoid,
    /// A reading, not a trade.
    Info,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Act => pick("ACT", "ДІЙ"),
            Verdict::Watch => pick("WATCH", "СТЕЖ"),
            Verdict::Avoid => pick("AVOID", "НЕ ЧІПАЙ"),
            Verdict::Info => pick("READING", "ПОКАЗАННЯ"),
        }
    }
}

/// What a level on the chart is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelKind {
    Entry,
    Target,
    Stop,
    /// A band of the usual range.
    Band,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub value: f64,
    pub kind: LevelKind,
    pub label: String,
}

impl Level {
    fn new(value: f64, kind: LevelKind, label: impl Into<String>) -> Level {
        Level {
            value,
            kind,
            label: label.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub verdict: Verdict,
    /// One line: what this is and what to do.
    pub headline: String,
    pub steps: Vec<String>,
    pub levels: Vec<Level>,
}

fn mean_sd(x: &[f64]) -> (f64, f64) {
    let n = x.len().max(1) as f64;
    let m = x.iter().sum::<f64>() / n;
    let v = x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    (m, v.sqrt())
}

/// Rolling z of each close against the `n` closes before it (`None` until
/// there are `n`).
fn rolling_z(c: &[f64], n: usize) -> Vec<Option<f64>> {
    (0..c.len())
        .map(|t| {
            if t < n {
                return None;
            }
            let (m, sd) = mean_sd(&c[t - n..t]);
            (sd > 0.0).then(|| (c[t] - m) / sd)
        })
        .collect()
}

/// Earlier stretches like now: every time the rolling z crossed out past
/// `thr` on the side of `side` (-1 below, +1 above), did the price get back
/// to its mean (z back to 0) within `horizon` candles?  Episodes without a
/// full horizon (the current one) are not counted.  Returns (times, back).
pub fn stretch_history(c: &[f64], n: usize, thr: f64, side: f64, horizon: usize) -> (usize, usize) {
    let z = rolling_z(c, n);
    let out_at = |t: usize| z[t].is_some_and(|v| v * side >= thr);
    let (mut times, mut back) = (0, 0);
    let mut t = 1;
    while t < z.len() {
        if !(out_at(t) && !out_at(t - 1)) {
            t += 1;
            continue;
        }
        if t + horizon >= z.len() {
            break;
        }
        times += 1;
        match (1..=horizon).find(|k| z[t + k].is_some_and(|v| v * side <= 0.0)) {
            Some(k) => {
                back += 1;
                t += k;
            }
            None => t += horizon,
        }
    }
    (times, back)
}

/// Earlier falls of `drop_pct` % or more over `window` candles: each fresh
/// one, the change over the next `window` candles.  Returns those changes, %.
pub fn knife_history(c: &[f64], window: usize, drop_pct: f64) -> Vec<f64> {
    let chg = |t: usize| (c[t] / c[t - window] - 1.0) * 100.0;
    let mut out = Vec::new();
    let mut t = window + 1;
    while t + window < c.len() {
        if c[t - window] > 0.0 && chg(t) <= -drop_pct && chg(t - 1) > -drop_pct {
            out.push((c[t + window] / c[t] - 1.0) * 100.0);
            t += window;
        } else {
            t += 1;
        }
    }
    out
}

fn pct(from: f64, to: f64) -> String {
    if from == 0.0 {
        return String::new();
    }
    format!("{:+.1}%", (to / from - 1.0) * 100.0)
}

/// "7 of 9 times (78%)".
fn rate(k: usize, n: usize) -> String {
    let share = k as f64 / n.max(1) as f64 * 100.0;
    pick(
        format!(
            "{k} of {n} time{} ({share:.0}%)",
            if n == 1 { "" } else { "s" }
        ),
        format!("{k} з {n} разів ({share:.0}%)"),
    )
}

/// Fewer earlier cases than this is not evidence.
pub const MIN_CASES: usize = 5;

/// The plan for `idea` from its chart's candles (a pair uses
/// [`pair_candles`]); `usd`: the order size per leg / per trade.
pub fn plan(idea: &Idea, candles: &[Ohlc], usd: f64) -> Plan {
    let closes: Vec<f64> = candles
        .iter()
        .filter(|c| !c.is_gap && c.close.is_finite())
        .map(|c| c.close)
        .collect();
    match &idea.what {
        What::Pair(p) => pair_plan(p, usd),
        What::Stretch(s) => stretch_plan(s, &closes, &idea.tf, usd),
        What::Knife(k) => knife_plan(k, candles, &closes, &idea.tf),
        What::Mover(m) => mover_plan(m, &closes),
        What::Chart(i) => reading_plan(&PRESETS[(*i).min(PRESETS.len() - 1)], &closes),
        What::Link { a, b, r } => link_plan(short(a), short(b), *r, &closes),
        What::Rule(r) => rule_plan(r, &closes),
        What::Move(m) => move_plan(m, &closes),
        What::Laggard(l) => laggard_plan(l, &closes),
        What::LiveLink(l) => {
            let mut p = link_plan(short(&l.a), short(&l.b), l.r, &closes);
            p.steps.insert(
                0,
                pick(
                    format!(
                        "Today's link only ({} min), not history. {} {:+.1}%, {} {:+.1}%.",
                        l.minutes,
                        short(&l.a),
                        l.pct_a,
                        short(&l.b),
                        l.pct_b
                    ),
                    format!(
                        "Лише зв'язок дня ({} хв), не історія. {} {:+.1}%, {} {:+.1}%.",
                        l.minutes,
                        short(&l.a),
                        l.pct_a,
                        short(&l.b),
                        l.pct_b
                    ),
                ),
            );
            p
        }
        What::Group(g) => group_plan(g, &closes),
        What::Spread(sp) => spread_plan(sp, &closes),
        What::Parity(p) => parity_plan(p),
        What::Box(b) => box_plan(b),
        What::Synthetic(sy) => synthetic_plan(sy, &closes),
        What::SynPair(p) => syn_pair_plan(p, &closes),
    }
}

/// A synthetic instrument: what it is made of and how it is traded.
fn synthetic_plan(sy: &live::Synthetic, c: &[f64]) -> Plan {
    let names: Vec<&str> = sy.members.iter().map(|m| short(m)).collect();
    let mut steps = vec![
        pick(
            format!(
                "{}: {} coins that moved as one over {} min (every pair r >= {:.2}, mean {:.2}): {}.",
                sy.name,
                sy.members.len(),
                sy.minutes,
                sy.tier_r,
                sy.r_mean,
                names.join(", ")
            ),
            format!(
                "{}: {} монет рухались як одна за {} хв (кожна пара r >= {:.2}, середнє {:.2}): {}.",
                sy.name,
                sy.members.len(),
                sy.minutes,
                sy.tier_r,
                sy.r_mean,
                names.join(", ")
            ),
        ),
        pick(
            format!(
                "One instrument, not {} coins: {:+.1}% over the hour. b buys it as one basket, equal dollars a coin. Rebuilt every pass: members can change.",
                sy.members.len(),
                sy.pct
            ),
            format!(
                "Один інструмент, а не {} монет: {:+.1}% за годину. b купує його кошиком, порівну на монету. Перебудовується щоразу: склад може змінитись.",
                sy.members.len(),
                sy.pct
            ),
        ),
    ];
    if let Some(z) = sy.z_hedge {
        let (en, uk) = if z > 1.5 {
            ("ran ahead of the market", "випередила ринок")
        } else if z < -1.5 {
            ("lags the market", "відстає від ринку")
        } else {
            ("in step with the market", "іде в ногу з ринком")
        };
        steps.push(pick(
            format!("Against BTC: {z:+.1} sd from its hour's mean, {en}."),
            format!("Проти BTC: {z:+.1} sd від середнього за годину, {uk}."),
        ));
    }
    let mut levels = Vec::new();
    if c.len() >= 30 {
        let (mean, sd) = mean_sd(&c[c.len().saturating_sub(100)..]);
        levels.push(Level::new(
            mean,
            LevelKind::Target,
            pick("usual", "звичайно"),
        ));
        levels.push(Level::new(mean + 2.0 * sd, LevelKind::Band, "+2 sd"));
        levels.push(Level::new(mean - 2.0 * sd, LevelKind::Band, "-2 sd"));
    }
    steps.push(
        pick(
            "Chart: the mean of its coins, each rebased to 100 (first eight).",
            "Графік: середнє її монет, кожна від 100 (перші вісім).",
        )
        .to_string(),
    );
    Plan {
        verdict: Verdict::Info,
        headline: pick(
            format!(
                "{}: {} coins as one, {:+.1}% this hour",
                sy.name,
                sy.members.len(),
                sy.pct
            ),
            format!(
                "{}: {} монет як одна, {:+.1}% за годину",
                sy.name,
                sy.members.len(),
                sy.pct
            ),
        ),
        steps,
        levels,
    }
}

/// One synthetic against another: the stat-arb pair of portfolios.
fn syn_pair_plan(p: &live::SynPair, c: &[f64]) -> Plan {
    let ((long, lm), (sh, sm)) = p.long_short();
    let mut steps = vec![
        pick(
            format!(
                "{} ({} coins) and {} ({} coins) moved as one over {} min (r {:.2}); their ratio is {:+.1} sd from the hour's mean (sd {:.2}%). {} {:+.1}%, {} {:+.1}%.",
                p.a, p.a_members.len(), p.b, p.b_members.len(), p.minutes, p.r, p.z, p.sd_pct, p.a, p.pct_a, p.b, p.pct_b
            ),
            format!(
                "{} ({} монет) і {} ({} монет) рухались разом {} хв (r {:.2}); їхнє співвідношення {:+.1} sd від середнього за годину (sd {:.2}%). {} {:+.1}%, {} {:+.1}%.",
                p.a, p.a_members.len(), p.b, p.b_members.len(), p.minutes, p.r, p.z, p.sd_pct, p.a, p.pct_a, p.b, p.pct_b
            ),
        ),
        pick(
            format!(
                "Trade: buy every coin of {long} ({}), sell every coin of {sh} ({}), equal dollars: {} legs, limits in and out as one basket. Pays when the ratio returns. b: the basket; A: the rule for every such pair.",
                lm.len(), sm.len(), lm.len() + sm.len()
            ),
            format!(
                "Угода: купи всі монети {long} ({}), продай усі монети {sh} ({}), порівну доларів: {} ніг, ліміти на вході й виході одним кошиком. Платить, коли співвідношення повернеться. b: кошик; A: правило для кожної такої пари.",
                lm.len(), sm.len(), lm.len() + sm.len()
            ),
        ),
        pick(
            "An hour of minutes is today's picture, not evidence.",
            "Година хвилин це картина дня, а не доказ.",
        )
        .to_string(),
    ];
    let mut levels = Vec::new();
    if c.len() >= 30 {
        let (mean, sd) = mean_sd(&c[c.len().saturating_sub(100)..]);
        let last = *c.last().expect("checked");
        let z = if sd > 0.0 { (last - mean) / sd } else { 0.0 };
        steps.push(pick(
            format!(
                "Chart: {} over {} (first four coins of each): now {z:+.1} sd from 100 candles.",
                p.a, p.b
            ),
            format!(
                "Графік: {} до {} (перші чотири монети кожної): зараз {z:+.1} sd від 100 свічок.",
                p.a, p.b
            ),
        ));
        levels.push(Level::new(
            mean,
            LevelKind::Target,
            pick("usual ratio", "звичайне співвідношення"),
        ));
        levels.push(Level::new(mean + 2.0 * sd, LevelKind::Band, "+2 sd"));
        levels.push(Level::new(mean - 2.0 * sd, LevelKind::Band, "-2 sd"));
    }
    Plan {
        verdict: Verdict::Watch,
        headline: pick(
            format!(
                "{} / {}: ratio {:+.1} sd; buy {long}, sell {sh}",
                p.a, p.b, p.z
            ),
            format!(
                "{} / {}: співвідношення {:+.1} sd; купи {long}, продай {sh}",
                p.a, p.b, p.z
            ),
        ),
        steps,
        levels,
    }
}

/// A stretched ratio among this hour's linked coins: the stat-arb pair.
fn spread_plan(sp: &live::Spread, c: &[f64]) -> Plan {
    let (a, b) = (short(&sp.a), short(&sp.b));
    let (long, sh) = sp.long_short();
    let (long, sh) = (short(long), short(sh));
    let mut steps = vec![
        pick(
            format!(
                "{a} and {b} moved as one for {} min (r {:.2}); their ratio is {:+.1} sd from the hour's mean (sd {:.2}%). {a} {:+.1}%, {b} {:+.1}%.",
                sp.minutes, sp.r, sp.z, sp.sd_pct, sp.pct_a, sp.pct_b
            ),
            format!(
                "{a} і {b} рухались разом {} хв (r {:.2}); їхнє співвідношення {:+.1} sd від середнього за годину (sd {:.2}%). {a} {:+.1}%, {b} {:+.1}%.",
                sp.minutes, sp.r, sp.z, sp.sd_pct, sp.pct_a, sp.pct_b
            ),
        ),
        pick(
            format!("Trade: buy {long}, sell {sh}, equal dollars; pays when the ratio returns. b: the basket; A: the rule for every such pair."),
            format!("Угода: купи {long}, продай {sh}, порівну доларів; платить, коли співвідношення повернеться. b: кошик; A: правило для кожної такої пари."),
        ),
        pick(
            "An hour of minutes is today's picture, not evidence: e asks the judge if such a stretch closed before.",
            "Година хвилин це картина дня, а не доказ: e питає суддю, чи такий розрив закривався раніше.",
        )
        .to_string(),
    ];
    let mut levels = Vec::new();
    if c.len() >= 30 {
        let (mean, sd) = mean_sd(&c[c.len().saturating_sub(100)..]);
        let last = *c.last().expect("checked");
        let z = if sd > 0.0 { (last - mean) / sd } else { 0.0 };
        steps.push(pick(
            format!("Chart: {a} priced in {b}: now {z:+.1} sd from 100 candles."),
            format!("Графік: {a} у {b}: зараз {z:+.1} sd від 100 свічок."),
        ));
        levels.push(Level::new(
            mean,
            LevelKind::Target,
            pick("usual ratio", "звичайне співвідношення"),
        ));
        levels.push(Level::new(mean + 2.0 * sd, LevelKind::Band, "+2 sd"));
        levels.push(Level::new(mean - 2.0 * sd, LevelKind::Band, "-2 sd"));
    }
    Plan {
        verdict: Verdict::Watch,
        headline: pick(
            format!("{a} / {b}: ratio {:+.1} sd; buy {long}, sell {sh}", sp.z),
            format!(
                "{a} / {b}: співвідношення {:+.1} sd; купи {long}, продай {sh}",
                sp.z
            ),
        ),
        steps,
        levels,
    }
}

/// Put-call parity: the three legs, the edge, what it needs for real.
fn parity_plan(p: &crate::options::Parity) -> Plan {
    use crate::options::Side;
    let u = short(&p.underlying);
    let ((spot_en, put_en, call_en), (spot_uk, put_uk, call_uk)) = match p.side {
        Side::Conversion => (("buy", "buy", "sell"), ("купи", "купи", "продай")),
        Side::Reversal => (("sell", "sell", "buy"), ("продай", "продай", "купи")),
    };
    let steps = vec![
        pick(
            format!(
                "At strike {} the options lock a forward of {:.2} vs a fair {:.2} (spot {:.2} at {:.1}% for {:.0} days): {:+.0} bps gross, {:+.0} net of {:.0} bps fees.",
                p.strike, p.implied_forward, p.forward, p.spot, p.rate * 100.0, p.days, p.gross_bps, p.net_bps(), p.fees_bps
            ),
            format!(
                "На страйку {} опціони фіксують форвард {:.2} проти справедливого {:.2} (спот {:.2} під {:.1}% на {:.0} дн.): {:+.0} б.п. брутто, {:+.0} після комісій {:.0} б.п.",
                p.strike, p.implied_forward, p.forward, p.spot, p.rate * 100.0, p.days, p.gross_bps, p.net_bps(), p.fees_bps
            ),
        ),
        pick(
            format!(
                "Three legs per coin: {spot_en} {u} spot, {put_en} the {} put, {call_en} the {} call; a contract is {} coin(s). Held to expiry ({}) it pays the edge whatever the price does.",
                p.strike, p.strike, p.unit, crate::options::expiry_words(p.expiry_ms)
            ),
            format!(
                "Три ноги на монету: {spot_uk} {u} спот, {put_uk} put {}, {call_uk} call {}; контракт це {} монет. До експірації ({}) платить різницю, що б не робила ціна.",
                p.strike, p.strike, p.unit, crate::options::expiry_words(p.expiry_ms)
            ),
        ),
        pick(
            format!(
                "{:.1}% a year. For real: at least 0.01 contract (about ${:.0}), margin on the short legs, and the quote is one moment's.",
                p.annual_pct(),
                p.spot * p.unit / 100.0
            ),
            format!(
                "{:.1}% річних. По-справжньому: мінімум 0.01 контракту (близько ${:.0}), маржа на коротких ногах, а котирування лише на мить.",
                p.annual_pct(),
                p.spot * p.unit / 100.0
            ),
        ),
        pick(
            "Paper only: options never trade live here.",
            "Лише папір: опціони тут ніколи не торгуються наживо.",
        )
        .to_string(),
    ];
    Plan {
        verdict: if p.net_bps() >= 10.0 {
            Verdict::Act
        } else {
            Verdict::Watch
        },
        headline: pick(
            format!(
                "{}: {:+.0} bps net to {}, {:.1}% a year",
                p.words(),
                p.net_bps(),
                crate::options::expiry_words(p.expiry_ms),
                p.annual_pct()
            ),
            format!(
                "{}: {:+.0} б.п. чистими до {}, {:.1}% річних",
                p.words(),
                p.net_bps(),
                crate::options::expiry_words(p.expiry_ms),
                p.annual_pct()
            ),
        ),
        steps,
        levels: vec![Level::new(
            p.strike,
            LevelKind::Target,
            pick("strike", "страйк"),
        )],
    }
}

/// A box spread: four options, a known payout, an implied loan.
fn box_plan(b: &crate::options::BoxSpread) -> Plan {
    let u = short(&b.underlying);
    let (buy_en, sell_en) = if b.buy {
        ("buy", "sell")
    } else {
        ("sell", "buy")
    };
    let (buy_uk, sell_uk) = if b.buy {
        ("купи", "продай")
    } else {
        ("продай", "купи")
    };
    let steps = vec![
        pick(
            format!(
                "The box pays {:.2} a coin at expiry ({}) whatever {u} does. {} it {} {:.2}: {:+.0} bps gross, {:+.0} net of {:.0} bps fees.",
                b.payout(),
                crate::options::expiry_words(b.expiry_ms),
                if b.buy { "Buying" } else { "Selling" },
                if b.buy { "costs" } else { "brings" },
                b.cost,
                b.gross_bps,
                b.net_bps(),
                b.fees_bps
            ),
            format!(
                "Бокс платить {:.2} за монету на експірації ({}), що б не робив {u}. {} {:.2}: {:+.0} б.п. брутто, {:+.0} після комісій {:.0} б.п.",
                b.payout(),
                crate::options::expiry_words(b.expiry_ms),
                if b.buy { "Купівля коштує" } else { "Продаж приносить" },
                b.cost,
                b.gross_bps,
                b.net_bps(),
                b.fees_bps
            ),
        ),
        pick(
            format!(
                "Four legs: {buy_en} the {:.0} call, {sell_en} the {:.0} call, {sell_en} the {:.0} put, {buy_en} the {:.0} put. No spot, no price risk; the risk is a vanished quote and margin.",
                b.low, b.high, b.low, b.high
            ),
            format!(
                "Чотири ноги: {buy_uk} call {:.0}, {sell_uk} call {:.0}, {sell_uk} put {:.0}, {buy_uk} put {:.0}. Без споту, без цінового ризику; ризик це зниклі котирування й маржа.",
                b.low, b.high, b.low, b.high
            ),
        ),
        pick(
            format!(
                "{:.1}% a year over {:.0} days. For real: at least 0.01 contract, four fills at once. Paper only.",
                b.annual_pct(),
                b.days
            ),
            format!(
                "{:.1}% річних за {:.0} дн. По-справжньому: мінімум 0.01 контракту, чотири виконання разом. Лише папір.",
                b.annual_pct(),
                b.days
            ),
        ),
    ];
    Plan {
        verdict: if b.net_bps() >= 10.0 {
            Verdict::Act
        } else {
            Verdict::Watch
        },
        headline: pick(
            format!(
                "{}: {:+.0} bps net, {:.1}% a year",
                b.words(),
                b.net_bps(),
                b.annual_pct()
            ),
            format!(
                "{}: {:+.0} б.п. чистими, {:.1}% річних",
                b.words(),
                b.net_bps(),
                b.annual_pct()
            ),
        ),
        steps,
        levels: vec![
            Level::new(b.low, LevelKind::Band, pick("low strike", "нижній страйк")),
            Level::new(
                b.high,
                LevelKind::Band,
                pick("high strike", "верхній страйк"),
            ),
        ],
    }
}

/// A live mover: like a pump, with the window it was ranked on.
fn move_plan(m: &live::Move, c: &[f64]) -> Plan {
    let as_mover = Mover {
        symbol: m.symbol.clone(),
        up: m.pct >= 0.0,
        change_pct: m.pct,
        window_min: m.window_min,
        volume_x: m.volume_x,
        price: m.price,
        first_ms: 0,
        last_ms: 0,
    };
    let mut p = mover_plan(&as_mover, c);
    let win = |w: Option<f64>, min: u32| {
        w.map_or(String::new(), |v| {
            format!(" {v:+.1}% / {min}{}", pick("m", "хв"))
        })
    };
    let (w5, w15, w60) = (win(m.pct_5, 5), win(m.pct_15, 15), win(m.pct_60, 60));
    let mut line = pick(
        format!(
            "Among the biggest moves of the last {} min:{w5}{w15}{w60}.",
            m.window_min
        ),
        format!(
            "Серед найбільших рухів за {} хв:{w5}{w15}{w60}.",
            m.window_min
        ),
    );
    line = line.replace("min:.", "min.").replace("хв:.", "хв.");
    p.steps.insert(0, line);
    p
}

/// A coin behind its leader: the candidate for a catch-up or a pair.
fn laggard_plan(l: &live::Laggard, c: &[f64]) -> Plan {
    let (lead, fol) = (short(&l.leader), short(&l.follower));
    let up = l.lead_pct > 0.0;
    let gap = l.gap_pct();
    let mut steps = vec![
        pick(
            format!(
                "{fol} moved with {lead} over {} min (r {:.2}). In the last {} min {lead} made {:+.1}%, {fol} only {:+.1}%.",
                l.minutes, l.r, l.window_min, l.lead_pct, l.follow_pct
            ),
            format!(
                "{fol} рухався разом з {lead} {} хв (r {:.2}). За останні {} хв {lead} зробив {:+.1}%, {fol} лише {:+.1}%.",
                l.minutes, l.r, l.window_min, l.lead_pct, l.follow_pct
            ),
        ),
        if up {
            pick(
                format!("Two ways: {fol} catches up (buy {fol}), or the gap closes (buy {fol}, sell {lead}). The pair is hedged."),
                format!("Два шляхи: {fol} наздожене (купи {fol}) або розрив закриється (купи {fol}, продай {lead}). Пара захищена."),
            )
        } else {
            pick(
                format!("Two ways: {fol} follows down (sell {fol}), or the gap closes (sell {fol}, buy {lead}). The pair is hedged."),
                format!("Два шляхи: {fol} піде вниз (продай {fol}) або розрив закриється (продай {fol}, купи {lead}). Пара захищена."),
            )
        },
        pick(
            "Minutes of link are today's picture, not evidence: e asks the judge if such a stretch closed before.",
            "Хвилини зв'язку це картина дня, а не доказ: e питає суддю, чи такий розрив закривався раніше.",
        )
        .to_string(),
    ];
    let mut levels = Vec::new();
    if c.len() >= 30 {
        let (mean, sd) = mean_sd(&c[c.len().saturating_sub(100)..]);
        let last = *c.last().expect("checked");
        let z = if sd > 0.0 { (last - mean) / sd } else { 0.0 };
        steps.push(pick(
            format!("Chart: {fol} priced in {lead}: now {z:+.1} sd from 100 minutes."),
            format!("Графік: {fol} у {lead}: зараз {z:+.1} sd від 100 хвилин."),
        ));
        levels.push(Level::new(
            mean,
            LevelKind::Target,
            pick("usual ratio", "звичайне співвідношення"),
        ));
        levels.push(Level::new(mean + 2.0 * sd, LevelKind::Band, "+2 sd"));
        levels.push(Level::new(mean - 2.0 * sd, LevelKind::Band, "-2 sd"));
    }
    Plan {
        verdict: Verdict::Watch,
        headline: pick(
            format!(
                "{fol} is {gap:.1} points behind {lead}'s {:+.1}%: watch the catch-up",
                l.lead_pct
            ),
            format!(
                "{fol} відстає на {gap:.1} п. від {lead} ({:+.1}%): стеж за наздоганянням",
                l.lead_pct
            ),
        ),
        steps,
        levels,
    }
}

/// A group moving as one: one bet, and a basket candidate.
fn group_plan(g: &live::Group, c: &[f64]) -> Plan {
    let names: Vec<&str> = g.members.iter().map(|m| short(m)).collect();
    let mut steps = vec![
        pick(
            format!(
                "{} coins moved as one over {} min (mean r {:.2}): {}.",
                g.members.len(),
                g.minutes,
                g.r_mean,
                names.join(", ")
            ),
            format!(
                "{} монет рухались як одна {} хв (середнє r {:.2}): {}.",
                g.members.len(),
                g.minutes,
                g.r_mean,
                names.join(", ")
            ),
        ),
        pick(
            format!(
                "Together {:+.1}%. Holding several is one bet: size the group as one position.",
                g.pct
            ),
            format!(
                "Разом {:+.1}%. Кілька з них це одна ставка: рахуй групу як одну позицію.",
                g.pct
            ),
        ),
        pick(
            "Chart: the group's mean, each coin rebased to 100 (first eight). It trades as a basket: one entry, one exit.",
            "Графік: середнє групи, кожна монета від 100 (перші вісім). Торгується кошиком: один вхід, один вихід.",
        )
        .to_string(),
    ];
    let mut levels = Vec::new();
    if c.len() >= 30 {
        let (mean, sd) = mean_sd(&c[c.len().saturating_sub(100)..]);
        let last = *c.last().expect("checked");
        let z = if sd > 0.0 { (last - mean) / sd } else { 0.0 };
        steps.push(pick(
            format!("The group line is {z:+.1} sd from 100 minutes."),
            format!("Лінія групи: {z:+.1} sd від 100 хвилин."),
        ));
        levels.push(Level::new(
            mean,
            LevelKind::Target,
            pick("usual", "звичайно"),
        ));
        levels.push(Level::new(mean + 2.0 * sd, LevelKind::Band, "+2 sd"));
        levels.push(Level::new(mean - 2.0 * sd, LevelKind::Band, "-2 sd"));
    }
    Plan {
        verdict: Verdict::Info,
        headline: pick(
            format!(
                "{} coins move as one this hour ({:+.1}%, r {:.2})",
                g.members.len(),
                g.pct,
                g.r_mean
            ),
            format!(
                "{} монет рухаються як одна цю годину ({:+.1}%, r {:.2})",
                g.members.len(),
                g.pct,
                g.r_mean
            ),
        ),
        steps,
        levels,
    }
}

fn rule_plan(r: &Rule, c: &[f64]) -> Plan {
    let split = r.split();
    let mut levels = Vec::new();
    let mut now = String::new();
    if let Some((line, op, thr)) = &split {
        levels.push(Level::new(
            *thr,
            LevelKind::Entry,
            pick(format!("event: {op} {thr}"), format!("подія: {op} {thr}")),
        ));
        if let Some(last) = c.last() {
            let on = match *op {
                ">" => *last > *thr,
                ">=" => *last >= *thr,
                "<" => *last < *thr,
                _ => *last <= *thr,
            };
            now = pick(
                format!(
                    "Now {line} is {}: {}.",
                    fmt_value(*last),
                    if on {
                        "the event is on"
                    } else {
                        "the event is off"
                    }
                ),
                format!(
                    "Зараз {line} = {}: {}.",
                    fmt_value(*last),
                    if on {
                        "подія триває"
                    } else {
                        "події немає"
                    }
                ),
            );
        }
    }
    let target = short(&r.target);
    let ask = match &split {
        Some((line, op, thr)) => pick(
            format!(
                "The line is {}; the event: it goes {op} {thr} (yellow level). Was {target} higher {} candle(s) later?",
                line.replace("USDT", ""),
                r.h
            ),
            format!(
                "Лінія {}; подія: вона {op} {thr} (жовтий рівень). Чи {target} було вище через {} св.?",
                line.replace("USDT", ""),
                r.h
            ),
        ),
        None => pick(
            format!(
                "After {} turns true, was {target} higher {} candle(s) later?",
                r.cond.replace("USDT", ""),
                r.h
            ),
            format!(
                "Після того як {} стає істинним, чи {target} було вище через {} св.?",
                r.cond.replace("USDT", ""),
                r.h
            ),
        ),
    };
    let (verdict, headline, mut steps) = match r.judged {
        Some((old, new)) => {
            let verdict = if r.survives {
                Verdict::Act
            } else if new.events < 20 {
                Verdict::Watch
            } else {
                Verdict::Avoid
            };
            let headline = match verdict {
                Verdict::Act => pick(
                    format!(
                        "Holds: after it, {} rose {:.0}% of the time vs {:.0}% usually",
                        target,
                        new.hit() * 100.0,
                        new.base_hit * 100.0
                    ),
                    format!(
                        "Тримається: після цього {} росла в {:.0}% випадків проти {:.0}% звичайно",
                        target,
                        new.hit() * 100.0,
                        new.base_hit * 100.0
                    ),
                ),
                Verdict::Watch => pick(
                    "Too few events in the newer part to tell".to_string(),
                    "Замало подій у новій частині, щоб судити".to_string(),
                ),
                _ => pick(
                    "No edge: chance does as well, don't trade it".to_string(),
                    "Переваги немає: випадок не гірший, не торгуй".to_string(),
                ),
            };
            (
                verdict,
                headline,
                vec![
                    ask,
                    pick(
                        format!("Older 70% (where it was picked): {}.", old.words()),
                        format!("Старі 70% (де знайдено): {}.", old.words()),
                    ),
                    pick(
                        format!("Newer 30% (unseen by the search): {}.", new.words()),
                        format!("Нові 30% (пошук не бачив): {}.", new.words()),
                    ),
                ],
            )
        }
        None => (
            Verdict::Watch,
            pick(
                "Screened only: not judged".to_string(),
                "Лише відібрано: не перевірено".to_string(),
            ),
            vec![
                ask,
                pick(
                    format!("{} events in the older part.", r.older_events),
                    format!("{} подій у старій частині.", r.older_events),
                ),
            ],
        ),
    };
    if !r.reason.is_empty() {
        let mut c = r.reason.chars();
        let first = c.next().map(|f| f.to_uppercase().collect::<String>());
        steps.push(format!("{}{}", first.unwrap_or_default(), c.as_str()).replace("USDT", ""));
    }
    if !now.is_empty() {
        steps.push(now);
    }
    steps.push(
        pick(
            "e: full test with chart; Enter: the line on the big chart.",
            "e: повна перевірка з графіком; Enter: лінія на великому графіку.",
        )
        .to_string(),
    );
    Plan {
        verdict,
        headline,
        steps,
        levels,
    }
}

fn pair_plan(p: &PairIdea, usd: f64) -> Plan {
    let (long, shortl) = (short(&p.long), short(&p.short));
    let h = &p.history;
    let side = p.z.signum();
    let verdict = if h.times >= MIN_CASES && h.closed * 10 >= h.times * 6 {
        Verdict::Act
    } else if h.times >= MIN_CASES && h.closed * 10 < h.times * 4 {
        Verdict::Avoid
    } else {
        Verdict::Watch
    };
    let evidence = if h.times == 0 {
        pick(
            "the gap was never this wide: no history".to_string(),
            "розрив ніколи не був таким: історії немає".to_string(),
        )
    } else {
        let tail = if h.closed > 0 {
            pick(
                format!(", usually in {:.0}", h.median_candles),
                format!(", зазвичай за {:.0}", h.median_candles),
            )
        } else {
            String::new()
        };
        pick(
            format!(
                "earlier gaps this wide closed {} within {} candles{tail}",
                rate(h.closed, h.times),
                h.horizon
            ),
            format!(
                "такі розриви раніше закривалися {} за {} св.{tail}",
                rate(h.closed, h.times),
                h.horizon
            ),
        )
    };
    let headline = match verdict {
        Verdict::Act => pick(
            format!("Buy {long}, sell {shortl}: the gap tends to close"),
            format!("Купи {long}, продай {shortl}: розрив зазвичай закривається"),
        ),
        Verdict::Avoid => pick(
            format!("{long} / {shortl}: gaps here rarely close, leave it"),
            format!("{long} / {shortl}: розриви тут рідко закриваються, залиш"),
        ),
        _ => pick(
            format!("Buy {long}, sell {shortl} only small: little history"),
            format!("Купи {long}, продай {shortl} лише трохи: мало історії"),
        ),
    };
    Plan {
        verdict,
        headline,
        steps: vec![
            pick(
                format!(
                    "They usually move together (r {:.2}); the gap is {:+.1} sd from normal.",
                    p.r, p.z
                ),
                format!(
                    "Зазвичай рухаються разом (r {:.2}); розрив {:+.1} sd від норми.",
                    p.r, p.z
                ),
            ),
            pick(
                format!("Enter: buy ${usd:.0} of {long}, sell ${usd:.0} of {shortl} at once (t sends both)."),
                format!("Вхід: купи {long} на ${usd:.0}, продай {shortl} на ${usd:.0} разом (t шле обидві)."),
            ),
            pick(
                format!("Exit when the gap is near 0 (white line); {evidence}."),
                format!("Вихід, коли розрив біля 0 (біла лінія); {evidence}."),
            ),
            pick(
                format!(
                    "Stop: past {:+.1} sd the tie has likely broken; close both.",
                    3.5 * side
                ),
                format!(
                    "Стоп: за {:+.1} sd зв'язок, схоже, порвався; закрий обидві.",
                    3.5 * side
                ),
            ),
        ],
        levels: vec![
            Level::new(2.0 * side, LevelKind::Entry, pick("entry zone", "зона входу")),
            Level::new(0.0, LevelKind::Target, pick("exit: gap closed", "вихід: розрив закрито")),
            Level::new(3.5 * side, LevelKind::Stop, pick("stop: tie broken", "стоп: зв'язок порвано")),
            Level::new(-2.0 * side, LevelKind::Band, pick("other side", "інший бік")),
        ],
    }
}

fn stretch_plan(s: &Stretch, c: &[f64], tf: &str, usd: f64) -> Plan {
    let sym = short(&s.symbol);
    let down = s.z < 0.0;
    let side = if down { -1.0 } else { 1.0 };
    if c.len() < 60 {
        return waiting(sym);
    }
    let window = &c[c.len() - 50..];
    let (mean, sd) = mean_sd(window);
    let last = *c.last().expect("checked");
    let z = if sd > 0.0 { (last - mean) / sd } else { 0.0 };
    let thr = z.abs().max(2.0);
    let (times, back) = stretch_history(c, 50, thr, side, 24);
    let stop = if (z * side) < 3.5 {
        mean + 3.5 * sd * side
    } else {
        last + sd * side
    };
    let verdict = if times >= MIN_CASES && back * 10 >= times * 6 {
        Verdict::Act
    } else if times >= MIN_CASES && back * 10 < times * 4 {
        Verdict::Avoid
    } else {
        Verdict::Watch
    };
    let evidence = if times == 0 {
        pick(
            format!("never this far in {} candles: no history", c.len()),
            format!("ніколи так далеко за {} св.: історії немає", c.len()),
        )
    } else {
        pick(
            format!(
                "{sym} got back to its average within 24 candles {} in the last {} {tf} candles",
                rate(back, times),
                c.len()
            ),
            format!(
                "{sym} поверталась до середнього за 24 св. {} за останні {} св. {tf}",
                rate(back, times),
                c.len()
            ),
        )
    };
    let (headline, enter) = match (verdict, down) {
        (Verdict::Act, true) => (
            pick(
                format!("Buy the dip in {sym}: it usually returns to its average"),
                format!("Купуй просідання {sym}: вона зазвичай повертається до середнього"),
            ),
            pick(
                format!("Enter: buy ${usd:.0} near {} (the chart's BUY).", fmt_value(last)),
                format!("Вхід: купи на ${usd:.0} біля {} (BUY на графіку).", fmt_value(last)),
            ),
        ),
        (Verdict::Act, false) => (
            pick(
                format!("{sym} is overextended: it usually falls back"),
                format!("{sym} перетягнута вгору: зазвичай відкочується"),
            ),
            pick(
                format!("Enter: take profit if you hold it; a short (SELL ${usd:.0}) needs Futures."),
                format!("Вхід: зафіксуй прибуток, якщо тримаєш; шорт (SELL ${usd:.0}) потребує ф'ючерсів."),
            ),
        ),
        (Verdict::Avoid, true) => (
            pick(
                format!("Don't buy {sym}'s dip: it tends to keep falling"),
                format!("Не купуй просідання {sym}: вона зазвичай падає далі"),
            ),
            pick(
                "Do not enter: this coin's stretches usually continued.".to_string(),
                "Не входь: розтягнення цієї монети зазвичай тривали.".to_string(),
            ),
        ),
        (Verdict::Avoid, false) => (
            pick(
                format!("Don't fade {sym}: strength tends to continue"),
                format!("Не грай проти {sym}: сила зазвичай триває"),
            ),
            pick(
                "Do not short it: this coin's stretches usually continued.".to_string(),
                "Не шортуй: розтягнення цієї монети зазвичай тривали.".to_string(),
            ),
        ),
        (_, true) => (
            pick(
                format!("{sym} is far below its average: watch, history is thin"),
                format!("{sym} далеко нижче середнього: стеж, історії мало"),
            ),
            pick(
                "If you enter, enter small: too little history.".to_string(),
                "Якщо входиш, то трохи: історії замало.".to_string(),
            ),
        ),
        (_, false) => (
            pick(
                format!("{sym} is far above its average: watch, history is thin"),
                format!("{sym} далеко вище середнього: стеж, історії мало"),
            ),
            pick(
                "If you act, act small: too little history.".to_string(),
                "Якщо дієш, то трохи: історії замало.".to_string(),
            ),
        ),
    };
    Plan {
        verdict,
        headline,
        steps: vec![
            pick(
                format!(
                    "{sym} is {:+.1} sd from its 50-candle average ({}).",
                    z,
                    pct(mean, last)
                ),
                format!(
                    "{sym}: {:+.1} sd від середнього за 50 св. ({}).",
                    z,
                    pct(mean, last)
                ),
            ),
            enter,
            pick(
                format!(
                    "Target: the average {} ({}); {evidence}.",
                    fmt_value(mean),
                    pct(last, mean)
                ),
                format!(
                    "Ціль: середнє {} ({}); {evidence}.",
                    fmt_value(mean),
                    pct(last, mean)
                ),
            ),
            pick(
                format!(
                    "Stop: {} ({}), where a stretch has become a new trend.",
                    fmt_value(stop),
                    pct(last, stop)
                ),
                format!(
                    "Стоп: {} ({}), де розтягнення стало новим трендом.",
                    fmt_value(stop),
                    pct(last, stop)
                ),
            ),
        ],
        levels: vec![
            Level::new(last, LevelKind::Entry, pick("now", "зараз")),
            Level::new(
                mean,
                LevelKind::Target,
                pick("target: average", "ціль: середнє"),
            ),
            Level::new(stop, LevelKind::Stop, pick("stop", "стоп")),
            Level::new(mean + 2.0 * sd * side, LevelKind::Band, "2 sd"),
        ],
    }
}

fn knife_plan(k: &Knife, candles: &[Ohlc], c: &[f64], tf: &str) -> Plan {
    let sym = short(&k.symbol);
    if c.len() < 60 {
        return waiting(sym);
    }
    let recent: Vec<&Ohlc> = candles
        .iter()
        .filter(|c| !c.is_gap)
        .rev()
        .take(24)
        .collect();
    let low = recent.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let trigger = recent.first().map_or(0.0, |c| c.high);
    let mut after = knife_history(c, 24, 8.0);
    let evidence = if after.is_empty() {
        pick(
            "no earlier fall like this in view".to_string(),
            "раніше такого падіння не було".to_string(),
        )
    } else {
        after.sort_by(f64::total_cmp);
        let up = after.iter().filter(|v| **v > 0.0).count();
        pick(
            format!(
                "after such falls it was higher a day later {} (middle case {:+.1}%)",
                rate(up, after.len()),
                after[after.len() / 2]
            ),
            format!(
                "після таких падінь за добу було вище {} (типовий випадок {:+.1}%)",
                rate(up, after.len()),
                after[after.len() / 2]
            ),
        )
    };
    Plan {
        verdict: Verdict::Avoid,
        headline: pick(
            format!("Don't catch {sym}: wait until it stops falling"),
            format!("Не лови {sym}: зачекай, поки перестане падати"),
        ),
        steps: vec![
            pick(
                format!(
                    "{sym} fell {:+.0}% in a day on x{:.1} volume and is still near the low.",
                    k.day_pct, k.volume_x
                ),
                format!(
                    "{sym} впала на {:+.0}% за добу з обсягом x{:.1} і ще біля мінімуму.",
                    k.day_pct, k.volume_x
                ),
            ),
            pick(
                format!(
                    "Wait: a {tf} candle closing above {} (the last high) is the first sign it stopped.",
                    fmt_value(trigger)
                ),
                format!(
                    "Чекай: закриття свічки {tf} вище {} (останній максимум) перша ознака зупинки.",
                    fmt_value(trigger)
                ),
            ),
            pick(
                format!(
                    "Only then: buy small, stop under the low {} ({}).",
                    fmt_value(low),
                    pct(trigger, low)
                ),
                format!(
                    "Лише тоді: купи трохи, стоп під мінімумом {} ({}).",
                    fmt_value(low),
                    pct(trigger, low)
                ),
            ),
            pick(
                format!("History: {evidence}."),
                format!("Історія: {evidence}."),
            ),
        ],
        levels: vec![
            Level::new(
                trigger,
                LevelKind::Entry,
                pick("close above: it stopped", "закриття вище: зупинилась"),
            ),
            Level::new(low, LevelKind::Stop, pick("the low", "мінімум")),
        ],
    }
}

fn mover_plan(m: &Mover, c: &[f64]) -> Plan {
    let sym = short(&m.symbol);
    let now = c.last().copied().unwrap_or(m.price);
    let start = m.price / (1.0 + m.change_pct / 100.0);
    let half = start + (m.price - start) / 2.0;
    let (what, chase, fade) = if m.up {
        (
            pick("jumped", "виросла"),
            pick("Buying now chases it", "Купівля зараз це погоня"),
            pick("a fall back under", "падіння назад нижче"),
        )
    } else {
        (
            pick("dropped", "впала"),
            pick("Selling now chases it", "Продаж зараз це погоня"),
            pick("a rise back over", "зростання назад вище"),
        )
    };
    Plan {
        verdict: Verdict::Watch,
        headline: pick(
            format!(
                "{sym} {what} {:+.1}% in {} min: watch, don't chase",
                m.change_pct, m.window_min
            ),
            format!(
                "{sym} {what} на {:+.1}% за {} хв: стеж, не доганяй",
                m.change_pct, m.window_min
            ),
        ),
        steps: vec![
            pick(
                format!(
                    "On x{:.1} the usual dollars: news, a listing or liquidations.",
                    m.volume_x
                ),
                format!(
                    "На x{:.1} звичайних доларів: новини, лістинг або ліквідації.",
                    m.volume_x
                ),
            ),
            pick(
                format!("{chase}: pumps are not checked against history, so this is no signal."),
                format!("{chase}: пампи не перевіряються історією, тож це не сигнал."),
            ),
            pick(
                format!(
                    "Half the move is at {}: {fade} it means it is fading.",
                    fmt_value(half)
                ),
                format!(
                    "Половина руху на {}: {fade} означає, що рух слабшає.",
                    fmt_value(half)
                ),
            ),
            pick(
                format!(
                    "If you trade it: small, stop beyond the start ({}, {}).",
                    fmt_value(start),
                    pct(now, start)
                ),
                format!(
                    "Якщо торгуєш: трохи, стоп за початком руху ({}, {}).",
                    fmt_value(start),
                    pct(now, start)
                ),
            ),
        ],
        levels: vec![
            Level::new(start, LevelKind::Stop, pick("move started", "початок руху")),
            Level::new(
                half,
                LevelKind::Band,
                pick("half the move", "половина руху"),
            ),
        ],
    }
}

fn reading_plan(p: &Preset, c: &[f64]) -> Plan {
    if c.len() < 30 {
        return Plan {
            verdict: Verdict::Info,
            headline: p.about().to_string(),
            steps: vec![pick("Loading its candles...", "Завантажую свічки...").into()],
            levels: Vec::new(),
        };
    }
    let w = &c[c.len().saturating_sub(100)..];
    let (mean, sd) = mean_sd(w);
    let last = *c.last().expect("checked");
    let z = if sd > 0.0 { (last - mean) / sd } else { 0.0 };
    let reading = if z >= 2.0 {
        format!("{} ({z:+.1} sd).", p.high())
    } else if z <= -2.0 {
        format!("{} ({z:+.1} sd).", p.low())
    } else if z >= 1.0 {
        pick(
            format!("Leaning high ({z:+.1} sd), still usual: {}.", p.high()),
            format!("Схиляється вгору ({z:+.1} sd), ще звичайно: {}.", p.high()),
        )
    } else if z <= -1.0 {
        pick(
            format!("Leaning low ({z:+.1} sd), still usual: {}.", p.low()),
            format!("Схиляється вниз ({z:+.1} sd), ще звичайно: {}.", p.low()),
        )
    } else {
        pick(
            format!("Near its usual level ({z:+.1} sd): nothing unusual."),
            format!("Біля звичайного рівня ({z:+.1} sd): нічого незвичного."),
        )
    };
    Plan {
        verdict: Verdict::Info,
        headline: reading.trim_end_matches('.').to_string(),
        steps: vec![
            p.about().to_string(),
            pick(
                format!(
                    "Now {}; the last {} candles average {} (white line); dotted: 2 sd.",
                    fmt_value(last),
                    w.len(),
                    fmt_value(mean)
                ),
                format!(
                    "Зараз {}; середнє за {} св. {} (біла лінія); пунктир: 2 sd.",
                    fmt_value(last),
                    w.len(),
                    fmt_value(mean)
                ),
            ),
            pick(
                "Beyond the dotted lines is unusual. Enter: the big chart, where the formula can be changed (t).",
                "За пунктиром незвично. Enter: великий графік, де формулу можна змінити (t).",
            )
            .to_string(),
        ],
        levels: vec![
            Level::new(mean, LevelKind::Target, pick("usual", "звичайно")),
            Level::new(mean + 2.0 * sd, LevelKind::Band, "+2 sd"),
            Level::new(mean - 2.0 * sd, LevelKind::Band, "-2 sd"),
        ],
    }
}

fn link_plan(a: &str, b: &str, r: f64, c: &[f64]) -> Plan {
    let (verdict, headline, advice) = if r >= 0.6 {
        (
            Verdict::Info,
            pick(
                format!("{a} and {b} move as one (r {r:+.2})"),
                format!("{a} і {b} рухаються як одна (r {r:+.2})"),
            ),
            pick(
                "Holding both is one bet: size them as one. If the ratio strays far, it is a pair trade (Scanner).",
                "Тримати обидві це одна ставка: рахуй як одну. Якщо співвідношення сильно відхилилось, це парна угода (Сканер).",
            )
            .to_string(),
        )
    } else if r <= -0.3 {
        (
            Verdict::Info,
            pick(
                format!("{a} and {b} move opposite (r {r:+.2})"),
                format!("{a} і {b} рухаються навпаки (r {r:+.2})"),
            ),
            pick(
                "One hedges the other: holding both lowers the swings.",
                "Одна хеджує іншу: обидві разом зменшують коливання.",
            )
            .to_string(),
        )
    } else if r.abs() < 0.2 {
        (
            Verdict::Info,
            pick(
                format!("{a} and {b} are hardly linked (r {r:+.2})"),
                format!("{a} і {b} майже не пов'язані (r {r:+.2})"),
            ),
            pick(
                "They diversify each other: separate bets.",
                "Вони диверсифікують одна одну: окремі ставки.",
            )
            .to_string(),
        )
    } else {
        (
            Verdict::Info,
            pick(
                format!("{a} and {b} are loosely linked (r {r:+.2})"),
                format!("{a} і {b} слабо пов'язані (r {r:+.2})"),
            ),
            pick(
                "Partly the same bet: part of the risk counts twice.",
                "Частково та сама ставка: частина ризику враховується двічі.",
            )
            .to_string(),
        )
    };
    let mut levels = Vec::new();
    let mut steps = vec![
        advice,
        pick(
            format!("Chart: {a} priced in {b}; rising means {a} is stronger."),
            format!("Графік: {a} у {b}; росте, коли {a} сильніша."),
        ),
    ];
    if c.len() >= 30 {
        let (mean, sd) = mean_sd(&c[c.len().saturating_sub(100)..]);
        let last = *c.last().expect("checked");
        let z = if sd > 0.0 { (last - mean) / sd } else { 0.0 };
        steps.push(pick(
            format!("The ratio is {z:+.1} sd from 100 candles."),
            format!("Співвідношення {z:+.1} sd від 100 свічок."),
        ));
        levels.push(Level::new(
            mean,
            LevelKind::Target,
            pick("usual ratio", "звичайне співвідношення"),
        ));
        levels.push(Level::new(mean + 2.0 * sd, LevelKind::Band, "+2 sd"));
        levels.push(Level::new(mean - 2.0 * sd, LevelKind::Band, "-2 sd"));
    }
    Plan {
        verdict,
        headline,
        steps,
        levels,
    }
}

fn waiting(sym: &str) -> Plan {
    Plan {
        verdict: Verdict::Watch,
        headline: pick(
            format!("{sym}: loading its candles..."),
            format!("{sym}: завантажую свічки..."),
        ),
        steps: Vec::new(),
        levels: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: &[f64]) -> Vec<Ohlc> {
        v.iter()
            .map(|&x| Ohlc {
                open: x,
                high: x,
                low: x,
                close: x,
                volume: 1.0,
                date: String::new(),
                time_ms: 0,
                is_gap: false,
            })
            .collect()
    }

    /// A wave around 100 that dips to `depth` every 40 candles, then returns.
    fn dips(depth: f64, back: bool) -> Vec<f64> {
        let mut v = Vec::new();
        let mut level = 100.0;
        for i in 0..600 {
            let phase = i % 40;
            let wiggle = ((i * 7919) % 13) as f64 / 13.0 - 0.5;
            let x = if phase == 30 {
                level - depth
            } else if phase > 30 && !back {
                level -= 0.4;
                level - depth
            } else {
                level + wiggle
            };
            v.push(x);
        }
        v
    }

    #[test]
    fn stretches_that_return_are_counted_and_the_open_one_is_not() {
        let c = dips(8.0, true);
        let (times, back) = stretch_history(&c, 30, 2.0, -1.0, 24);
        assert!(times >= 10, "{times}");
        assert_eq!(back, times, "every dip came back");
        let (t2, b2) = stretch_history(&c[..c.len() - 5], 30, 2.0, -1.0, 24);
        assert!(t2 <= times && b2 <= t2);
    }

    #[test]
    fn a_dip_that_keeps_returning_is_an_act_and_one_that_keeps_falling_is_not() {
        let s = Stretch {
            symbol: "SOLUSDT".into(),
            z: -3.0,
            change_pct: -6.0,
        };
        let idea = Idea {
            what: What::Stretch(s.clone()),
            tf: "1h".into(),
        };
        let mut c = dips(8.0, true);
        c.push(92.0);
        let p = plan(&idea, &line(&c), 25.0);
        assert_eq!(p.verdict, Verdict::Act, "{p:?}");
        assert!(p.levels.iter().any(|l| l.kind == LevelKind::Target));
        let stop = p.levels.iter().find(|l| l.kind == LevelKind::Stop).unwrap();
        assert!(stop.value < 92.0, "a dip's stop is below the price");
        let mut c = dips(8.0, false);
        c.push(c.last().unwrap() - 2.0);
        let p = plan(&idea, &line(&c), 25.0);
        assert_ne!(p.verdict, Verdict::Act, "{p:?}");
        // Too few candles: no verdict yet.
        assert_eq!(plan(&idea, &line(&c[..10]), 25.0).verdict, Verdict::Watch);
    }

    #[test]
    fn a_knife_is_never_an_act_and_its_history_is_counted() {
        let mut c: Vec<f64> = (0..300).map(|i| 100.0 + (i % 5) as f64).collect();
        for (i, v) in c.iter_mut().enumerate().skip(100).take(24) {
            *v = 100.0 - (i - 99) as f64; // a 24% fall
        }
        let h = knife_history(&c, 24, 8.0);
        assert_eq!(h.len(), 1, "{h:?}");
        let k = Knife {
            symbol: "XUSDT".into(),
            day_pct: -12.0,
            recent_pct: -3.0,
            volume_x: 3.0,
        };
        let idea = Idea {
            what: What::Knife(k),
            tf: "1h".into(),
        };
        assert_eq!(plan(&idea, &line(&c), 25.0).verdict, Verdict::Avoid);
    }

    #[test]
    fn a_pair_plan_follows_its_gap_history() {
        let mut p = PairIdea {
            a: "SOLUSDT".into(),
            b: "AVAXUSDT".into(),
            r: 0.8,
            beta: 1.1,
            z: 2.4,
            half_life: 6.0,
            t_older: -4.0,
            t_newer: -3.5,
            long: "AVAXUSDT".into(),
            short: "SOLUSDT".into(),
            z_path: vec![0.0, 1.0, 2.4],
            history: Default::default(),
        };
        p.history.times = 9;
        p.history.closed = 7;
        p.history.horizon = 12;
        let idea = Idea {
            what: What::Pair(p.clone()),
            tf: "1h".into(),
        };
        let plan1 = plan(&idea, &pair_candles(&p), 25.0);
        assert_eq!(plan1.verdict, Verdict::Act);
        assert!(plan1.headline.contains("Buy AVAX"), "{}", plan1.headline);
        let stop = plan1
            .levels
            .iter()
            .find(|l| l.kind == LevelKind::Stop)
            .unwrap();
        assert_eq!(stop.value, 3.5, "the stop is beyond the gap, on its side");
        p.history.closed = 2;
        let idea = Idea {
            what: What::Pair(p),
            tf: "1h".into(),
        };
        assert_eq!(plan(&idea, &[], 25.0).verdict, Verdict::Avoid);
        // The test the judge gets: the gap narrowing after it was this wide.
        let (cond, target, h) = idea.test().unwrap();
        assert!(
            cond.starts_with("Z(LN(SOL)-1.100*LN(AVAX),200) > 2.4"),
            "{cond}"
        );
        assert_eq!(target, "1.100*LN(AVAX)-LN(SOL)");
        assert_eq!(h, 12);
    }

    #[test]
    fn every_preset_parses_and_reads() {
        let t = crate::formula::builtin_templates();
        for p in PRESETS {
            let f = crate::formula::parse_with(p.formula, &t)
                .unwrap_or_else(|e| panic!("{}: {e}", p.name));
            assert!(f.sources().len() <= 8, "{}", p.name);
            assert!(crate::markets::Timeframe::ALL
                .iter()
                .any(|x| x.name() == p.tf));
        }
        let idea = presets().remove(0);
        let mut c: Vec<f64> = (0..200).map(|i| 20.0 + (i % 7) as f64 * 0.01).collect();
        c.push(25.0);
        let p = plan(&idea, &line(&c), 0.0);
        assert_eq!(p.verdict, Verdict::Info);
        assert!(
            p.headline.starts_with("BTC is unusually strong"),
            "{}",
            p.headline
        );
    }

    #[test]
    fn a_rule_is_charted_by_its_line_with_the_threshold() {
        let rule = Rule {
            cond: "Z(DOGE,50) > 2.5".into(),
            target: "DOGE".into(),
            h: 4,
            judged: Some((
                Odds {
                    events: 29,
                    up: 20,
                    base_hit: 0.5,
                },
                Odds {
                    events: 20,
                    up: 10,
                    base_hit: 0.5,
                },
            )),
            survives: false,
            reason: String::new(),
            older_events: 29,
        };
        assert_eq!(rule.split(), Some(("Z(DOGE,50)".to_string(), ">", 2.5)));
        let idea = Idea {
            what: What::Rule(rule),
            tf: "1h".into(),
        };
        assert_eq!(idea.chart().0, "=Z(DOGE,50)");
        let p = plan(&idea, &line(&[1.0, 3.0]), 0.0);
        assert_eq!(p.verdict, Verdict::Avoid);
        assert_eq!(p.levels[0].value, 2.5);
        assert!(
            p.steps.iter().any(|s| s.contains("the event is on")),
            "{p:?}"
        );
        assert_eq!(idea.facts(), "50% vs 50%");
    }

    #[test]
    fn a_scan_becomes_rows_in_order() {
        let scan = Scan {
            down: vec![Stretch {
                symbol: "BNBUSDT".into(),
                z: -2.6,
                change_pct: -3.0,
            }],
            tf: "15m".into(),
            ..Scan::default()
        };
        let rows = from_scan(&scan);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind(), Kind::Dip);
        assert_eq!(rows[0].title(), "BNB");
        assert_eq!(rows[0].chart(), ("=BNB".to_string(), "15m".to_string()));
    }
}
