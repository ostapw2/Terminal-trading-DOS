//! Formula charts: a candle series computed from other series.
//!
//! A chart whose ticker starts with `=` is a formula over symbols, numbers,
//! operators and functions, e.g. `=BTC/ETH`, `=IDX(SOL)-IDX(BTC)`,
//! `=F:BTCUSDT-BTCUSDT` (perp basis), `=Y:SPY*0.01`, `=Z(BTC/ETH,50)`.
//!
//! - **Symbols:** a Binance Spot symbol (`BTC` means `BTCUSDT`), `F:SYM` for
//!   Binance Futures, `Y:TICKER` for Yahoo (`Y:"BTC-USD"` when the ticker
//!   has a minus sign).  At most [`MAX_SOURCES`].
//! - **Futures market data** (Binance, readings rather than candles):
//!   `FR:SYM` funding rate (% per 8 h, about a year back), `OI:SYM` open
//!   interest, `LS:SYM` long/short accounts ratio, `TK:SYM` taker buy/sell
//!   ratio (these three: the last 30 days).  Each candle takes the last
//!   reading known at its open: no look-ahead.
//! - **Weather** (Open-Meteo, hourly, about three years back): `W:KYIV.TEMP`,
//!   `W:"NEW YORK".RAIN`; variables in `crate::data::weather::VARS`.
//! - **Macro and any API** (`crate::data::external`): `M:DGS10` (FRED, at
//!   publication time), `H:FNG` (a source described in `sources.json`).
//! - **Operators:** `+ - * /`, `^` (power), unary minus, parentheses.
//! - **Fields:** a bare symbol is its open / high / low / close in the
//!   candle's open / high / low / close (see below); `OPEN(x) HIGH(x) LOW(x)
//!   CLOSE(x) VOL(x)` pin one field.
//! - **Math:** `LOG LN EXP ABS SQRT`, `MIN(a,b) MAX(a,b)`.
//! - **Over time** (`n` a whole number, 1-1000): `SMA EMA STD SUM LAG(x,n)`,
//!   `CHG(x,n)` (% change over n candles), `Z(x,n)` (z-score over n),
//!   `IDX(x)` (rebased to 100 at the first candle), `RANK(x,n)` (percentile
//!   of the last value in its window, 0-100).
//! - **Comparison of two series over n:** `CORR(x,y,n)` (Pearson), `COV`,
//!   `BETA(x,y,n)` (x on y).  Correlate changes, not prices:
//!   `CORR(CHG(BTC,1),CHG(ETH,1),50)` (template `RCORR(BTC,ETH,50)`).
//! - **Conditions:** `> < >= <=` give 1 or 0; `IF(cond, a, b)`.
//! - **Templates:** `NAME(a,b,n) = body` with parameters; built in:
//!   [`BUILTIN_TEMPLATES`]; the user's are kept by the app.
//!
//! How candles are built: the formula runs four times, on every symbol's
//! open, high, low and close; the results are the candle's open and close,
//! and its high / low are the extremes of the four.  Exact for open and
//! close; high and low are an estimate (symbols do not peak at the same
//! instant), as on other platforms.  Candles are matched by their open time
//! on the chart's timeframe; only times every symbol has are kept.  A value
//! that is not a number (division by zero, log of a negative) becomes a gap.
//! Pure: candles in, candles out.

use std::collections::HashMap;

use crate::markets::{Ohlc, Timeframe};

pub const MAX_LEN: usize = 300;
pub const MAX_SOURCES: usize = 8;
const MAX_DEPTH: usize = 32;
const MAX_WINDOW: f64 = 1000.0;

/// Where a symbol's candles come from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Spot(String),
    Futures(String),
    Yahoo(String),
    /// Binance Futures funding rate, % per 8 h (`FR:`).
    Funding(String),
    /// Binance Futures open interest, in coins (`OI:`; last 30 days).
    OpenInterest(String),
    /// Long / short ratio of accounts (`LS:`; last 30 days).
    LongShort(String),
    /// Taker buy / sell volume ratio (`TK:`; last 30 days).
    Taker(String),
    /// Hourly weather at a place (`W:KYIV.TEMP`; Open-Meteo): place, variable.
    Weather(String, String),
    /// A FRED macro series at its publication time (`M:DGS10`).
    Macro(String),
    /// A JSON API described in `sources.json` (`H:FNG`).
    Http(String),
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Spot(s) => s.clone(),
            Source::Futures(s) => format!("F:{s}"),
            Source::Yahoo(s) => format!("Y:{s}"),
            Source::Funding(s) => format!("FR:{s}"),
            Source::OpenInterest(s) => format!("OI:{s}"),
            Source::LongShort(s) => format!("LS:{s}"),
            Source::Taker(s) => format!("TK:{s}"),
            Source::Weather(p, v) if p.contains(' ') => format!("W:\"{p}\".{v}"),
            Source::Weather(p, v) => format!("W:{p}.{v}"),
            Source::Macro(s) => format!("M:{s}"),
            Source::Http(s) => format!("H:{s}"),
        }
    }

    /// A series of readings at its own times (funding every 8 h, ...), not
    /// candles: on a chart each candle takes the last reading known at its
    /// open, and the series never narrows the timeline.
    pub fn is_step(&self) -> bool {
        matches!(
            self,
            Source::Funding(_)
                | Source::OpenInterest(_)
                | Source::LongShort(_)
                | Source::Taker(_)
                | Source::Weather(..)
                | Source::Macro(_)
                | Source::Http(_)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// The pass's own field (open in the open pass, ...).
    Auto,
    Open,
    High,
    Low,
    Close,
    Volume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Gt,
    Lt,
    Ge,
    Le,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Func {
    Log,
    Ln,
    Exp,
    Abs,
    Sqrt,
    Min,
    Max,
    Sma,
    Ema,
    Std,
    Sum,
    Lag,
    Chg,
    Z,
    Idx,
    Rank,
    Corr,
    Cov,
    Beta,
    If,
}

impl Func {
    fn parse(name: &str) -> Option<Func> {
        Some(match name {
            "LOG" => Func::Log,
            "LN" => Func::Ln,
            "EXP" => Func::Exp,
            "ABS" => Func::Abs,
            "SQRT" => Func::Sqrt,
            "MIN" => Func::Min,
            "MAX" => Func::Max,
            "SMA" => Func::Sma,
            "EMA" => Func::Ema,
            "STD" => Func::Std,
            "SUM" => Func::Sum,
            "LAG" => Func::Lag,
            "CHG" => Func::Chg,
            "Z" => Func::Z,
            "IDX" => Func::Idx,
            "RANK" => Func::Rank,
            "CORR" => Func::Corr,
            "COV" => Func::Cov,
            "BETA" => Func::Beta,
            "IF" => Func::If,
            _ => return None,
        })
    }

    /// Number of arguments; for the windowed ones the last is `n`.
    fn arity(self) -> usize {
        match self {
            Func::Log | Func::Ln | Func::Exp | Func::Abs | Func::Sqrt | Func::Idx => 1,
            Func::Corr | Func::Cov | Func::Beta | Func::If => 3,
            _ => 2,
        }
    }

    fn windowed(self) -> bool {
        !matches!(
            self,
            Func::Log
                | Func::Ln
                | Func::Exp
                | Func::Abs
                | Func::Sqrt
                | Func::Idx
                | Func::Min
                | Func::Max
                | Func::If
        )
    }
}

fn field_fn(name: &str) -> Option<Field> {
    Some(match name {
        "OPEN" => Field::Open,
        "HIGH" => Field::High,
        "LOW" => Field::Low,
        "CLOSE" => Field::Close,
        "VOL" | "VOLUME" => Field::Volume,
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Num(f64),
    Sym(Source, Field),
    Neg(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
    /// A template's parameter (only inside a template's body).
    Param(usize),
    /// `OPEN(p)` ... on a parameter: resolved when the template is used.
    FieldOf(Field, Box<Expr>),
}

/// A formula with parameters: `PAIRZ(A,B,N) = Z(A/B,N)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub name: String,
    pub params: Vec<String>,
    /// The definition as written (`NAME(a,b) = body`).
    pub text: String,
    body: Expr,
}

/// Templates by name.
pub type Templates = HashMap<String, Template>;

/// Built-in templates, each a definition as a user would type it.
pub const BUILTIN_TEMPLATES: &[&str] = &[
    // How far the ratio of two is from its own normal, in standard deviations.
    "PAIRZ(A,B,N) = Z(A/B,N)",
    // Relative strength: both rebased to 100, the difference.
    "RS(A,B) = IDX(A)-IDX(B)",
    // Correlation and beta of candle-to-candle changes.
    "RCORR(A,B,N) = CORR(CHG(A,1),CHG(B,1),N)",
    "RBETA(A,B,N) = BETA(CHG(A,1),CHG(B,1),N)",
    // A's change that B (scaled by beta) does not explain.
    "RESID(A,B,N) = CHG(A,1)-RBETA(A,B,N)*CHG(B,1)",
    // Unusual volume.
    "VOLZ(A,N) = Z(VOL(A),N)",
    // Distance from the moving average, %.
    "DEV(A,N) = (A/SMA(A,N)-1)*100",
];

fn valid_template_name(s: &str) -> bool {
    (1..=16).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && Func::parse(s).is_none()
        && field_fn(s).is_none()
}

/// Parse a definition `NAME(p1,p2,...) = body`; `known` templates may be
/// used in the body.
pub fn define(def: &str, known: &Templates) -> Result<Template, String> {
    let (head, body) = def
        .split_once('=')
        .ok_or("a template is NAME(a,b) = formula")?;
    let head = head.trim().to_ascii_uppercase();
    let (name, rest) = head
        .split_once('(')
        .ok_or("a template is NAME(a,b) = formula")?;
    let name = name.trim().to_string();
    if !valid_template_name(&name) {
        return Err(format!(
            "template name '{name}': 1-16 of A-Z 0-9 _, starting with a letter, not a function"
        ));
    }
    let params: Vec<String> = rest
        .strip_suffix(')')
        .ok_or("missing ')' after the parameters")?
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if params.is_empty() || params.len() > 8 {
        return Err("a template has 1-8 parameters".into());
    }
    if params
        .iter()
        .any(|p| !valid_template_name(p) || p.len() > 8)
    {
        return Err("parameter names: short words of A-Z 0-9 _".into());
    }
    let body_text = body.trim();
    if body_text.is_empty() || body_text.chars().count() > MAX_LEN {
        return Err(format!("a template body is 1-{MAX_LEN} characters"));
    }
    let mut p = Parser {
        toks: tokenize(body_text)?,
        pos: 0,
        depth: 0,
        params: params.clone(),
        templates: known,
    };
    let body = p.expr()?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected {:?}", p.toks[p.pos]));
    }
    Ok(Template {
        text: format!("{name}({}) = {body_text}", params.join(",")),
        name,
        params,
        body,
    })
}

/// The built-in templates, parsed.
pub fn builtin_templates() -> Templates {
    let mut t = Templates::new();
    for def in BUILTIN_TEMPLATES {
        let tpl = define(def, &t).expect("built-in templates parse");
        t.insert(tpl.name.clone(), tpl);
    }
    t
}

/// `e` with the parameters replaced by `args`.
fn instantiate(e: &Expr, args: &[Expr]) -> Result<Expr, String> {
    Ok(match e {
        Expr::Param(i) => args[*i].clone(),
        Expr::FieldOf(f, inner) => match instantiate(inner, args)? {
            Expr::Sym(src, Field::Auto) => Expr::Sym(src, *f),
            _ => return Err("OPEN/HIGH/LOW/CLOSE/VOL take a symbol".into()),
        },
        Expr::Num(_) | Expr::Sym(..) => e.clone(),
        Expr::Neg(a) => Expr::Neg(Box::new(instantiate(a, args)?)),
        Expr::Bin(op, a, b) => Expr::Bin(
            *op,
            Box::new(instantiate(a, args)?),
            Box::new(instantiate(b, args)?),
        ),
        Expr::Call(f, a) => Expr::Call(
            *f,
            a.iter()
                .map(|x| instantiate(x, args))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// After templates are filled in: windows are whole numbers 1-1000.
fn check(e: &Expr) -> Result<(), String> {
    match e {
        Expr::Num(_) | Expr::Sym(..) => Ok(()),
        Expr::Param(_) | Expr::FieldOf(..) => Err("a parameter outside its template".into()),
        Expr::Neg(a) => check(a),
        Expr::Bin(_, a, b) => check(a).and(check(b)),
        Expr::Call(f, args) => {
            if f.windowed() {
                match args.last() {
                    Some(Expr::Num(n)) if (1.0..=MAX_WINDOW).contains(n) && n.fract() == 0.0 => {}
                    _ => return Err("the window n of a function is a whole number 1-1000".into()),
                }
            }
            args.iter().try_for_each(check)
        }
    }
}

/// A parsed formula.
#[derive(Clone, Debug, PartialEq)]
pub struct Formula {
    expr: Expr,
    /// The text as typed, without the leading `=`.
    pub text: String,
}

// ─── Parsing ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Sym(Source),
    Op(char),
    Cmp(Op),
    LParen,
    RParen,
    Comma,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let word = |i: &mut usize| {
        let start = *i;
        while *i < c.len() && (c[*i].is_ascii_alphanumeric() || c[*i] == '_') {
            *i += 1;
        }
        c[start..*i].iter().collect::<String>().to_ascii_uppercase()
    };
    while i < c.len() {
        let ch = c[i];
        match ch {
            ' ' | '\t' => i += 1,
            '+' | '-' | '*' | '/' | '^' => {
                out.push(Tok::Op(ch));
                i += 1;
            }
            '>' | '<' => {
                let eq = c.get(i + 1) == Some(&'=');
                out.push(Tok::Cmp(match (ch, eq) {
                    ('>', false) => Op::Gt,
                    ('<', false) => Op::Lt,
                    ('>', true) => Op::Ge,
                    _ => Op::Le,
                }));
                i += 1 + usize::from(eq);
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '0'..='9' | '.' => {
                let start = i;
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                    i += 1;
                }
                // An exponent: 1e18, 2.5E-3.
                if i < c.len() && matches!(c[i], 'e' | 'E') {
                    let mut j = i + 1;
                    if j < c.len() && matches!(c[j], '+' | '-') {
                        j += 1;
                    }
                    if j < c.len() && c[j].is_ascii_digit() {
                        while j < c.len() && c[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let t: String = c[start..i].iter().collect();
                let v: f64 = t.parse().map_err(|_| format!("bad number '{t}'"))?;
                out.push(Tok::Num(v));
            }
            _ if ch.is_ascii_alphabetic() => {
                let w = word(&mut i);
                if w == "W" && i < c.len() && c[i] == ':' {
                    // W:PLACE.VAR or W:"PLACE WITH SPACES".VAR
                    i += 1;
                    let place = if i < c.len() && c[i] == '"' {
                        let start = i + 1;
                        let end = c[start..]
                            .iter()
                            .position(|&q| q == '"')
                            .map(|p| start + p)
                            .ok_or("unclosed quote in a W:\"...\" place")?;
                        i = end + 1;
                        c[start..end]
                            .iter()
                            .collect::<String>()
                            .to_ascii_uppercase()
                    } else {
                        word(&mut i)
                    };
                    if i >= c.len() || c[i] != '.' {
                        return Err("weather is W:PLACE.VAR, e.g. W:KYIV.TEMP".into());
                    }
                    i += 1;
                    let var = word(&mut i);
                    let ok_place = !place.is_empty()
                        && place.chars().count() <= 40
                        && place
                            .chars()
                            .all(|ch| ch.is_alphanumeric() || matches!(ch, ' ' | '-' | '\''));
                    if !ok_place {
                        return Err(format!("bad place '{place}' after W:"));
                    }
                    if crate::data::weather::var_name(&var).is_none() {
                        let vars: Vec<&str> =
                            crate::data::weather::VARS.iter().map(|v| v.0).collect();
                        return Err(format!("weather variable {var}? ({})", vars.join(" ")));
                    }
                    out.push(Tok::Sym(Source::Weather(place, var)));
                    continue;
                }
                if i < c.len()
                    && c[i] == ':'
                    && matches!(
                        w.as_str(),
                        "F" | "Y" | "FR" | "OI" | "LS" | "TK" | "M" | "H"
                    )
                {
                    i += 1;
                    let name = if w == "Y" && i < c.len() && c[i] == '"' {
                        // A quoted Yahoo ticker may hold anything printable.
                        let start = i + 1;
                        let end = c[start..]
                            .iter()
                            .position(|&q| q == '"')
                            .map(|p| start + p)
                            .ok_or("unclosed quote in a Y:\"...\" ticker")?;
                        i = end + 1;
                        c[start..end]
                            .iter()
                            .collect::<String>()
                            .to_ascii_uppercase()
                    } else if w == "Y" {
                        let start = i;
                        while i < c.len()
                            && (c[i].is_ascii_alphanumeric()
                                || matches!(c[i], '^' | '=' | '.' | '_'))
                        {
                            i += 1;
                        }
                        c[start..i].iter().collect::<String>().to_ascii_uppercase()
                    } else {
                        word(&mut i)
                    };
                    let ok = !name.is_empty()
                        && name.len() <= 24
                        && name.chars().all(|ch| ch.is_ascii_graphic() && ch != '"');
                    if !ok {
                        return Err(format!("bad ticker after {w}:"));
                    }
                    out.push(Tok::Sym(match w.as_str() {
                        "Y" => Source::Yahoo(name),
                        "F" => Source::Futures(spot_symbol(&name)),
                        "FR" => Source::Funding(spot_symbol(&name)),
                        "OI" => Source::OpenInterest(spot_symbol(&name)),
                        "LS" => Source::LongShort(spot_symbol(&name)),
                        "M" => Source::Macro(name),
                        "H" => Source::Http(name),
                        _ => Source::Taker(spot_symbol(&name)),
                    }));
                } else {
                    out.push(Tok::Ident(w));
                }
            }
            _ => return Err(format!("unexpected '{ch}'")),
        }
    }
    Ok(out)
}

struct Parser<'t> {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
    /// Parameter names while parsing a template's body.
    params: Vec<String>,
    templates: &'t Templates,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, t: Tok, what: &str) -> Result<(), String> {
        if self.next() == Some(t) {
            Ok(())
        } else {
            Err(format!("expected {what}"))
        }
    }

    /// A comparison of two sums (at most one), then `+ -`, `* /`, unary
    /// minus, `^` (right to left).
    fn expr(&mut self) -> Result<Expr, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("formula is nested too deep".into());
        }
        let lhs = self.sum()?;
        let e = if let Some(Tok::Cmp(op)) = self.peek().cloned() {
            self.pos += 1;
            Expr::Bin(op, Box::new(lhs), Box::new(self.sum()?))
        } else {
            lhs
        };
        self.depth -= 1;
        Ok(e)
    }

    fn sum(&mut self) -> Result<Expr, String> {
        let mut lhs = self.term()?;
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.term()?;
            let op = if c == '+' { Op::Add } else { Op::Sub };
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        while let Some(Tok::Op(c @ ('*' | '/'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.unary()?;
            let op = if c == '*' { Op::Mul } else { Op::Div };
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Tok::Op('-')) {
            self.pos += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.power()
    }

    fn power(&mut self) -> Result<Expr, String> {
        let base = self.atom()?;
        if self.peek() == Some(&Tok::Op('^')) {
            self.pos += 1;
            let exp = self.unary()?;
            return Ok(Expr::Bin(Op::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(v)) => Ok(Expr::Num(v)),
            Some(Tok::Sym(src)) => Ok(Expr::Sym(src, Field::Auto)),
            Some(Tok::LParen) => {
                let e = self.expr()?;
                self.expect(Tok::RParen, "')'")?;
                Ok(e)
            }
            Some(Tok::Ident(name)) => {
                if self.peek() != Some(&Tok::LParen) {
                    if let Some(i) = self.params.iter().position(|p| *p == name) {
                        return Ok(Expr::Param(i));
                    }
                    return Ok(Expr::Sym(Source::Spot(spot_symbol(&name)), Field::Auto));
                }
                self.pos += 1;
                let mut args = vec![self.expr()?];
                while self.peek() == Some(&Tok::Comma) {
                    self.pos += 1;
                    args.push(self.expr()?);
                }
                self.expect(Tok::RParen, "')'")?;
                if let Some(field) = field_fn(&name) {
                    return match args.as_slice() {
                        [Expr::Sym(src, Field::Auto)] => Ok(Expr::Sym(src.clone(), field)),
                        [p @ Expr::Param(_)] => Ok(Expr::FieldOf(field, Box::new(p.clone()))),
                        _ => Err(format!("{name}(...) takes one symbol, e.g. {name}(BTC)")),
                    };
                }
                if let Some(t) = self.templates.get(&name) {
                    if args.len() != t.params.len() {
                        return Err(format!(
                            "{name} takes {} argument(s): {}",
                            t.params.len(),
                            t.text
                        ));
                    }
                    return instantiate(&t.body, &args);
                }
                let f = Func::parse(&name).ok_or_else(|| format!("unknown function {name}"))?;
                if args.len() != f.arity() {
                    return Err(format!("{name} takes {} argument(s)", f.arity()));
                }
                Ok(Expr::Call(f, args))
            }
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("formula ends too early".into()),
        }
    }
}

/// `BTC` -> `BTCUSDT`; a symbol with a known quote stays as it is.
fn spot_symbol(s: &str) -> String {
    const QUOTES: [&str; 6] = ["USDT", "USDC", "FDUSD", "BTC", "ETH", "BNB"];
    if s.len() > 4 && QUOTES.iter().any(|q| s.ends_with(q) && s.len() > q.len()) {
        s.to_string()
    } else {
        format!("{s}USDT")
    }
}

/// Is this ticker a formula?
pub fn is_formula(ticker: &str) -> bool {
    ticker.starts_with('=')
}

/// Parse `=...` (or the text without `=`) with the built-in templates.
pub fn parse(text: &str) -> Result<Formula, String> {
    parse_with(text, &builtin_templates())
}

/// Parse with these templates (built-in plus the user's).
pub fn parse_with(text: &str, templates: &Templates) -> Result<Formula, String> {
    let body = text.strip_prefix('=').unwrap_or(text).trim();
    if body.is_empty() {
        return Err("empty formula".into());
    }
    if body.chars().count() > MAX_LEN {
        return Err(format!("formula longer than {MAX_LEN} characters"));
    }
    let mut p = Parser {
        toks: tokenize(body)?,
        pos: 0,
        depth: 0,
        params: Vec::new(),
        templates,
    };
    let expr = p.expr()?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected {:?}", p.toks[p.pos]));
    }
    check(&expr)?;
    let f = Formula {
        expr,
        text: body.to_string(),
    };
    let n = f.sources().len();
    if n == 0 {
        return Err("a formula needs at least one symbol".into());
    }
    if n > MAX_SOURCES {
        return Err(format!("at most {MAX_SOURCES} symbols in a formula"));
    }
    Ok(f)
}

impl Formula {
    /// Every series the formula reads, sorted, once each.
    pub fn sources(&self) -> Vec<Source> {
        fn walk(e: &Expr, out: &mut Vec<Source>) {
            match e {
                Expr::Num(_) => {}
                Expr::Sym(s, _) => {
                    if !out.contains(s) {
                        out.push(s.clone());
                    }
                }
                Expr::Neg(a) => walk(a, out),
                Expr::Bin(_, a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
                Expr::Call(_, args) => args.iter().for_each(|a| walk(a, out)),
                Expr::Param(_) => {}
                Expr::FieldOf(_, a) => walk(a, out),
            }
        }
        let mut v = Vec::new();
        walk(&self.expr, &mut v);
        v.sort();
        v
    }

    /// The candles of the formula on `tf` from each source's candles.
    pub fn evaluate(
        &self,
        data: &HashMap<Source, Vec<Ohlc>>,
        tf: Timeframe,
    ) -> Result<Vec<Ohlc>, String> {
        self.candles(&align(&self.sources(), data, tf)?)
    }

    /// The candles of the formula on series aligned once ([`align`]): the
    /// way to evaluate many formulas over the same data.
    pub fn candles(&self, a: &Aligned) -> Result<Vec<Ohlc>, String> {
        let pass = |f: Field| self.series(a, f);
        let (o, h, l, c) = (
            pass(Field::Open)?,
            pass(Field::High)?,
            pass(Field::Low)?,
            pass(Field::Close)?,
        );
        let mut out = Vec::with_capacity(a.times.len());
        let mut started = false;
        for (i, &t) in a.times.iter().enumerate() {
            let q = [o[i], h[i], l[i], c[i]];
            let ok = q.iter().all(|v| v.is_finite());
            // Windows warming up at the start: no candle yet.
            if !ok && !started {
                continue;
            }
            started = true;
            if !ok {
                out.push(Ohlc {
                    time_ms: t,
                    date: crate::markets::unix_secs_to_iso_date(t / 1000),
                    is_gap: true,
                    ..Default::default()
                });
                continue;
            }
            let hi = q.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let lo = q.iter().copied().fold(f64::INFINITY, f64::min);
            out.push(Ohlc {
                open: q[0],
                high: hi,
                low: lo,
                close: q[3],
                volume: 0.0,
                date: crate::markets::unix_secs_to_iso_date(t / 1000),
                time_ms: t,
                is_gap: false,
            });
        }
        if out.iter().filter(|k| !k.is_gap).count() < 2 {
            return Err(
                "the formula gives no numbers here (windows longer than the history?)".into(),
            );
        }
        Ok(out)
    }

    /// One pass of the formula (`Field::Close`: its close series) on aligned
    /// data; NaN where it has no value.  The fast path for searches.
    pub fn series(&self, a: &Aligned, pass: Field) -> Result<Vec<f64>, String> {
        if let Some(s) = self.sources().into_iter().find(|s| !a.cols.contains_key(s)) {
            return Err(format!("{} is not in the aligned data", s.label()));
        }
        Ok(Ctx { a }.eval(&self.expr, pass))
    }
}

/// Several sources' candles on one timeline, as columns: built once, read
/// by any number of formulas.
pub struct Aligned {
    /// Candle open times every source has, ascending.
    pub times: Vec<i64>,
    /// Per source: open, high, low, close, volume, one value per time.
    cols: HashMap<Source, [Vec<f64>; 5]>,
}

impl Aligned {
    /// Is this source's data in here?
    pub fn has(&self, s: &Source) -> bool {
        self.cols.contains_key(s)
    }
}

/// Match `sources`' candles by open time on `tf`; keep the times all the
/// candle sources have.  Step sources ([`Source::is_step`]) give each kept
/// time their last reading at or before it (none older than three of their
/// own intervals: NaN, a gap); they never narrow the timeline.  A formula of
/// step sources only runs on the first one's times.
pub fn align(
    sources: &[Source],
    data: &HashMap<Source, Vec<Ohlc>>,
    tf: Timeframe,
) -> Result<Aligned, String> {
    let get = |s: &Source| {
        data.get(s)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("no data for {}", s.label()))
    };
    let mut bars: Vec<(&Source, HashMap<i64, &Ohlc>)> = Vec::new();
    let mut steps: Vec<(&Source, Vec<&Ohlc>)> = Vec::new();
    for s in sources {
        let v = get(s)?;
        let real = v.iter().filter(|k| !k.is_gap && k.time_ms > 0);
        if s.is_step() {
            let mut pts: Vec<&Ohlc> = real.collect();
            pts.sort_by_key(|k| k.time_ms);
            steps.push((s, pts));
        } else {
            bars.push((s, real.map(|k| (tf.open_ms(k.time_ms), k)).collect()));
        }
    }
    let mut times: Vec<i64> = match (bars.first(), steps.first()) {
        (Some((_, first)), _) => {
            let mut t: Vec<i64> = first.keys().copied().collect();
            t.retain(|t| bars.iter().all(|(_, m)| m.contains_key(t)));
            t
        }
        (None, Some((_, pts))) => pts.iter().map(|k| tf.open_ms(k.time_ms)).collect(),
        (None, None) => return Err("no sources".into()),
    };
    times.sort_unstable();
    times.dedup();
    if times.len() < 2 {
        return Err(format!(
            "the symbols share {} candle time(s) on {}: too few (different markets or hours?)",
            times.len(),
            tf.name()
        ));
    }
    let mut cols: HashMap<Source, [Vec<f64>; 5]> = bars
        .into_iter()
        .map(|(s, m)| {
            let col = |f: fn(&Ohlc) -> f64| times.iter().map(|t| f(m[t])).collect::<Vec<f64>>();
            (
                s.clone(),
                [
                    col(|k| k.open),
                    col(|k| k.high),
                    col(|k| k.low),
                    col(|k| k.close),
                    col(|k| k.volume),
                ],
            )
        })
        .collect();
    for (s, pts) in steps {
        // How stale a reading may be: three of the series' own intervals.
        let mut gaps: Vec<i64> = pts
            .windows(2)
            .map(|w| w[1].time_ms - w[0].time_ms)
            .collect();
        gaps.sort_unstable();
        let max_age = gaps.get(gaps.len() / 2).copied().unwrap_or(i64::MAX / 4) * 3;
        let at: Vec<Option<&Ohlc>> = times
            .iter()
            .map(|&t| {
                let i = pts.partition_point(|k| k.time_ms <= t);
                (i > 0)
                    .then(|| pts[i - 1])
                    .filter(|k| t - k.time_ms <= max_age)
            })
            .collect();
        let col = |f: fn(&Ohlc) -> f64| {
            at.iter()
                .map(|k| k.map_or(f64::NAN, f))
                .collect::<Vec<f64>>()
        };
        cols.insert(
            s.clone(),
            [
                col(|k| k.open),
                col(|k| k.high),
                col(|k| k.low),
                col(|k| k.close),
                col(|k| k.volume),
            ],
        );
    }
    Ok(Aligned { times, cols })
}

struct Ctx<'a> {
    a: &'a Aligned,
}

impl Ctx<'_> {
    fn eval(&self, e: &Expr, pass: Field) -> Vec<f64> {
        let n = self.a.times.len();
        match e {
            Expr::Num(v) => vec![*v; n],
            Expr::Sym(s, f) => {
                let field = if *f == Field::Auto { pass } else { *f };
                let i = match field {
                    Field::Open => 0,
                    Field::High => 1,
                    Field::Low => 2,
                    Field::Close | Field::Auto => 3,
                    Field::Volume => 4,
                };
                self.a.cols[s][i].clone()
            }
            Expr::Neg(a) => {
                let mut v = self.eval(a, pass);
                v.iter_mut().for_each(|x| *x = -*x);
                v
            }
            Expr::Bin(op, a, b) => {
                let mut x = self.eval(a, pass);
                let y = self.eval(b, pass);
                for (x, y) in x.iter_mut().zip(&y) {
                    *x = match op {
                        Op::Add => *x + y,
                        Op::Sub => *x - y,
                        Op::Mul => *x * y,
                        Op::Div => *x / y,
                        Op::Pow => x.powf(*y),
                        Op::Gt | Op::Lt | Op::Ge | Op::Le if x.is_nan() || y.is_nan() => f64::NAN,
                        Op::Gt => f64::from(u8::from(*x > *y)),
                        Op::Lt => f64::from(u8::from(*x < *y)),
                        Op::Ge => f64::from(u8::from(*x >= *y)),
                        Op::Le => f64::from(u8::from(*x <= *y)),
                    };
                }
                x
            }
            Expr::Param(_) | Expr::FieldOf(..) => vec![f64::NAN; n],
            Expr::Call(f, args) => {
                let mut x = self.eval(&args[0], pass);
                // The window is the last argument (checked by `check`).
                let win = || match args.last() {
                    Some(Expr::Num(v)) => *v as usize,
                    _ => 1,
                };
                let map =
                    |x: &mut Vec<f64>, g: fn(f64) -> f64| x.iter_mut().for_each(|v| *v = g(*v));
                match f {
                    Func::Log => map(&mut x, f64::log10),
                    Func::Ln => map(&mut x, f64::ln),
                    Func::Exp => map(&mut x, f64::exp),
                    Func::Abs => map(&mut x, f64::abs),
                    Func::Sqrt => map(&mut x, f64::sqrt),
                    Func::Min | Func::Max => {
                        let y = self.eval(&args[1], pass);
                        for (a, b) in x.iter_mut().zip(y) {
                            *a = if *f == Func::Min { a.min(b) } else { a.max(b) };
                        }
                    }
                    Func::Sma => return moments(&x, win(), |s, _, n, _| s / n),
                    Func::Sum => return moments(&x, win(), |s, _, _, _| s),
                    Func::Std => return moments(&x, win(), |s, ss, n, _| var(s, ss, n).sqrt()),
                    Func::Z => {
                        return moments(&x, win(), |s, ss, n, last| {
                            (last - s / n) / var(s, ss, n).sqrt()
                        })
                    }
                    Func::Lag => return lag(&x, win()),
                    Func::Rank => return rank(&x, win()),
                    Func::Corr | Func::Cov | Func::Beta => {
                        let y = self.eval(&args[1], pass);
                        return co_moments(&x, &y, win(), |m| match f {
                            Func::Corr => m.cov() / (m.var_x() * m.var_y()).sqrt(),
                            Func::Cov => m.cov(),
                            _ => m.cov() / m.var_y(),
                        });
                    }
                    Func::If => {
                        let (a, b) = (self.eval(&args[1], pass), self.eval(&args[2], pass));
                        for ((c, a), b) in x.iter_mut().zip(a).zip(b) {
                            *c = if c.is_nan() {
                                f64::NAN
                            } else if *c > 0.0 {
                                a
                            } else {
                                b
                            };
                        }
                    }
                    Func::Chg => {
                        let prev = lag(&x, win());
                        for (a, b) in x.iter_mut().zip(prev) {
                            *a = (*a / b - 1.0) * 100.0;
                        }
                    }
                    Func::Ema => return ema(&x, win()),
                    Func::Idx => {
                        // One base for all four passes: the first close, so
                        // the candles keep their shape.
                        let base = if pass == Field::Close {
                            x.iter().copied().find(|v| v.is_finite() && *v != 0.0)
                        } else {
                            self.eval(&args[0], Field::Close)
                                .into_iter()
                                .find(|v| v.is_finite() && *v != 0.0)
                        }
                        .unwrap_or(f64::NAN);
                        x.iter_mut().for_each(|v| *v = *v / base * 100.0);
                    }
                }
                x
            }
        }
    }
}

/// Population variance from a window's sum and sum of squares.
fn var(s: f64, ss: f64, n: f64) -> f64 {
    (ss / n - (s / n).powi(2)).max(0.0)
}

/// `f(sum, sum of squares, n, last value)` over each window of `n` values
/// ending at i, in one pass (running sums: O(len), not O(len * n)); NaN
/// until the first full window and while the window holds a non-number.
fn moments(x: &[f64], n: usize, f: impl Fn(f64, f64, f64, f64) -> f64) -> Vec<f64> {
    let n = n.max(1);
    let (mut s, mut ss, mut bad) = (0.0, 0.0, 0usize);
    let mut out = vec![f64::NAN; x.len()];
    for i in 0..x.len() {
        let v = x[i];
        if v.is_finite() {
            s += v;
            ss += v * v;
        } else {
            bad += 1;
        }
        if i >= n {
            let old = x[i - n];
            if old.is_finite() {
                s -= old;
                ss -= old * old;
            } else {
                bad -= 1;
            }
        }
        if i + 1 >= n && bad == 0 {
            out[i] = f(s, ss, n as f64, v);
        }
    }
    out
}

/// Sums over a window of two series (for correlation, covariance, beta).
struct CoMoments {
    n: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
}

impl CoMoments {
    fn cov(&self) -> f64 {
        self.sxy / self.n - (self.sx / self.n) * (self.sy / self.n)
    }
    fn var_x(&self) -> f64 {
        var(self.sx, self.sxx, self.n)
    }
    fn var_y(&self) -> f64 {
        var(self.sy, self.syy, self.n)
    }
}

/// `f` over each window of `n` pairs ending at i, in one pass; NaN until
/// the first full window and while the window holds a non-number.
fn co_moments(x: &[f64], y: &[f64], n: usize, f: impl Fn(&CoMoments) -> f64) -> Vec<f64> {
    let n = n.max(1);
    let mut m = CoMoments {
        n: n as f64,
        sx: 0.0,
        sy: 0.0,
        sxx: 0.0,
        syy: 0.0,
        sxy: 0.0,
    };
    let mut bad = 0usize;
    let mut out = vec![f64::NAN; x.len()];
    let add = |m: &mut CoMoments, a: f64, b: f64, sign: f64| {
        m.sx += sign * a;
        m.sy += sign * b;
        m.sxx += sign * a * a;
        m.syy += sign * b * b;
        m.sxy += sign * a * b;
    };
    for i in 0..x.len() {
        let (a, b) = (x[i], y[i]);
        if a.is_finite() && b.is_finite() {
            add(&mut m, a, b, 1.0);
        } else {
            bad += 1;
        }
        if i >= n {
            let (a, b) = (x[i - n], y[i - n]);
            if a.is_finite() && b.is_finite() {
                add(&mut m, a, b, -1.0);
            } else {
                bad -= 1;
            }
        }
        if i + 1 >= n && bad == 0 {
            out[i] = f(&m);
        }
    }
    out
}

/// Percentile (0-100) of each value among the last `n` (itself included).
fn rank(x: &[f64], n: usize) -> Vec<f64> {
    let n = n.max(1);
    (0..x.len())
        .map(|i| {
            if i + 1 < n {
                return f64::NAN;
            }
            let w = &x[i + 1 - n..=i];
            if w.iter().any(|v| !v.is_finite()) {
                return f64::NAN;
            }
            if n == 1 {
                return 100.0;
            }
            let below = w.iter().filter(|v| **v < x[i]).count();
            below as f64 / (n - 1) as f64 * 100.0
        })
        .collect()
}

fn lag(x: &[f64], n: usize) -> Vec<f64> {
    (0..x.len())
        .map(|i| if i >= n { x[i - n] } else { f64::NAN })
        .collect()
}

/// EMA seeded with the SMA of the first `n` values.
fn ema(x: &[f64], n: usize) -> Vec<f64> {
    let n = n.max(1);
    let k = 2.0 / (n as f64 + 1.0);
    let mut out = vec![f64::NAN; x.len()];
    let mut prev: Option<f64> = None;
    for i in 0..x.len() {
        prev = match prev {
            Some(p) if x[i].is_finite() => Some(p + k * (x[i] - p)),
            Some(_) => None,
            None if i + 1 >= n && x[i + 1 - n..=i].iter().all(|v| v.is_finite()) => {
                Some(x[i + 1 - n..=i].iter().sum::<f64>() / n as f64)
            }
            None => None,
        };
        if let Some(p) = prev {
            out[i] = p;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(t: i64, o: f64, h: f64, l: f64, c: f64) -> Ohlc {
        Ohlc {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 10.0,
            date: String::new(),
            time_ms: T0 + t,
            is_gap: false,
        }
    }

    const H: i64 = 3_600_000;
    /// An hour boundary (2023-11-14 22:00 UTC).
    const T0: i64 = 1_699_999_200_000;

    fn data(pairs: &[(Source, Vec<Ohlc>)]) -> HashMap<Source, Vec<Ohlc>> {
        pairs.iter().cloned().collect()
    }

    #[test]
    fn formulas_parse_with_precedence_sources_and_errors() {
        let f = parse("=btc/eth").unwrap();
        assert_eq!(
            f.sources(),
            [
                Source::Spot("BTCUSDT".into()),
                Source::Spot("ETHUSDT".into())
            ]
        );
        let f = parse("=F:BTCUSDT - BTCUSDT + Y:SPY * 2 ^ 2").unwrap();
        assert_eq!(f.sources().len(), 3);
        assert!(f.sources().contains(&Source::Yahoo("SPY".into())));
        assert!(parse("=Y:\"BTC-USD\" / Y:^GSPC")
            .unwrap()
            .sources()
            .contains(&Source::Yahoo("BTC-USD".into())));
        // ETHBTC keeps its quote.
        assert_eq!(
            parse("ETHBTC").unwrap().sources(),
            [Source::Spot("ETHBTC".into())]
        );
        for bad in [
            "=",
            "=1+2",
            "=BTC +",
            "=(BTC",
            "=FOO(BTC)",
            "=SMA(BTC)",
            "=SMA(BTC, 2.5)",
            "=SMA(BTC, 0)",
            "=OPEN(BTC+ETH)",
            "=BTC $ ETH",
            "=A+B+C+D+E+F+G+H+I",
            "=Y:\"OPEN",
        ] {
            assert!(parse(bad).is_err(), "{bad} should fail");
        }
        let deep = format!("={}BTC{}", "(".repeat(40), ")".repeat(40));
        assert!(parse(&deep).unwrap_err().contains("deep"));
        assert!(parse(&format!("={}", "B".repeat(400))).is_err());
    }

    #[test]
    fn a_ratio_builds_candles_from_the_four_passes() {
        let a = vec![
            k(0, 100.0, 110.0, 90.0, 105.0),
            k(H, 105.0, 120.0, 100.0, 118.0),
        ];
        let b = vec![k(0, 50.0, 55.0, 45.0, 50.0), k(H, 50.0, 50.0, 50.0, 59.0)];
        let d = data(&[
            (Source::Spot("AUSDT".into()), a),
            (Source::Spot("BUSDT".into()), b),
        ]);
        let out = parse("=A/B").unwrap().evaluate(&d, Timeframe::H1).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].open, out[0].close), (2.0, 2.1));
        assert!(out[0].high >= out[0].open && out[0].low <= out[0].close);
        // Pinned fields ignore the pass.
        let v = parse("=VOL(A) + CLOSE(B)")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert_eq!((v[1].open, v[1].close), (69.0, 69.0));
    }

    #[test]
    fn times_are_matched_on_the_timeframe_and_missing_ones_dropped() {
        // Yahoo-style 13:30 against Binance 13:00, plus a time only A has.
        let a = vec![
            k(0, 1.0, 1.0, 1.0, 1.0),
            k(H, 2.0, 2.0, 2.0, 2.0),
            k(2 * H, 3.0, 3.0, 3.0, 3.0),
        ];
        let b = vec![
            k(H / 2, 1.0, 1.0, 1.0, 1.0),
            k(H + H / 2, 1.0, 1.0, 1.0, 1.0),
        ];
        let d = data(&[
            (Source::Spot("AUSDT".into()), a),
            (Source::Yahoo("B".into()), b),
        ]);
        let out = parse("=A*Y:B")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert_eq!(
            out.iter().map(|o| o.time_ms).collect::<Vec<_>>(),
            [T0, T0 + H]
        );
        // Nothing in common: a clear error.
        let c = vec![k(10 * H, 1.0, 1.0, 1.0, 1.0)];
        let d2 = data(&[
            (Source::Spot("AUSDT".into()), vec![k(0, 1.0, 1.0, 1.0, 1.0)]),
            (Source::Spot("CUSDT".into()), c),
        ]);
        assert!(parse("=A+C")
            .unwrap()
            .evaluate(&d2, Timeframe::H1)
            .unwrap_err()
            .contains("share 0"));
        assert!(parse("=A")
            .unwrap()
            .evaluate(&HashMap::new(), Timeframe::H1)
            .unwrap_err()
            .contains("no data"));
    }

    #[test]
    fn running_windows_match_the_naive_ones_and_skip_bad_values() {
        let x: Vec<f64> = (0..200)
            .map(|i| {
                if i == 120 {
                    f64::NAN
                } else {
                    1000.0 + (i as f64 * 0.7).sin() * 50.0
                }
            })
            .collect();
        let n = 20;
        let sma = moments(&x, n, |s, _, n, _| s / n);
        let sd = moments(&x, n, |s, ss, n, _| var(s, ss, n).sqrt());
        for i in 0..x.len() {
            let w = &x[(i + 1).saturating_sub(n)..=i];
            if i + 1 < n || w.iter().any(|v| !v.is_finite()) {
                assert!(sma[i].is_nan() && sd[i].is_nan(), "{i}");
                continue;
            }
            let mean = w.iter().sum::<f64>() / n as f64;
            let naive = (w.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
            assert!((sma[i] - mean).abs() < 1e-9, "{i}");
            assert!((sd[i] - naive).abs() < 1e-6, "{i}: {} vs {naive}", sd[i]);
        }
        // The NaN leaves the window again.
        assert!(sma[140].is_finite());
    }

    #[test]
    fn many_formulas_read_one_alignment() {
        let a: Vec<Ohlc> = (0..30)
            .map(|i| k(i * H, 1.0, 2.0, 0.5, 1.0 + i as f64))
            .collect();
        let b: Vec<Ohlc> = (0..30).map(|i| k(i * H, 2.0, 2.0, 2.0, 2.0)).collect();
        let (sa, sb) = (Source::Spot("AUSDT".into()), Source::Spot("BUSDT".into()));
        let d = data(&[(sa.clone(), a), (sb.clone(), b)]);
        let al = align(&[sa, sb], &d, Timeframe::H1).unwrap();
        assert_eq!(al.times.len(), 30);
        let r = parse("=A/B").unwrap().series(&al, Field::Close).unwrap();
        assert_eq!((r[0], r[29]), (0.5, 15.0));
        assert!(parse("=C")
            .unwrap()
            .series(&al, Field::Close)
            .unwrap_err()
            .contains("CUSDT"));
    }

    #[test]
    fn correlation_beta_rank_and_conditions() {
        let n = 60;
        let xs: Vec<f64> = (0..n).map(|i| (i as f64 * 0.3).sin()).collect();
        // y = 2x + 1: correlation 1, beta of y on x 2, of x on y 0.5.
        let ys: Vec<f64> = xs.iter().map(|v| 2.0 * v + 1.0).collect();
        let c = co_moments(&xs, &ys, 20, |m| m.cov() / (m.var_x() * m.var_y()).sqrt());
        assert!((c[30] - 1.0).abs() < 1e-9 && c[18].is_nan());
        let b = co_moments(&ys, &xs, 20, |m| m.cov() / m.var_y());
        assert!((b[40] - 2.0).abs() < 1e-9, "{}", b[40]);
        let r = rank(&[1.0, 5.0, 3.0, 9.0, 2.0], 3);
        assert_eq!((r[2], r[3], r[4]), (50.0, 100.0, 0.0));
        let a: Vec<Ohlc> = (0..40)
            .map(|i| {
                let v = 10.0 + (i as f64 * 0.5).sin();
                k(i * H, v, v, v, v)
            })
            .collect();
        let bb: Vec<Ohlc> = (0..40)
            .map(|i| {
                let v = 20.0 + 2.0 * (i as f64 * 0.5).sin();
                k(i * H, v, v, v, v)
            })
            .collect();
        let d = data(&[
            (Source::Spot("AUSDT".into()), a),
            (Source::Spot("BUSDT".into()), bb),
        ]);
        let corr = parse("=CORR(A,B,10)")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        let off: Vec<_> = corr
            .iter()
            .filter(|k| k.is_gap || (k.close - 1.0).abs() >= 1e-6)
            .collect();
        assert!(off.is_empty(), "{off:?}");
        let cond = parse("=IF(A>10, 1, -1)")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert!(cond.iter().all(|k| k.close == 1.0 || k.close == -1.0));
        let ge = parse("=A>=A").unwrap().evaluate(&d, Timeframe::H1).unwrap();
        assert!(ge.iter().all(|k| k.close == 1.0));
        assert!(parse("=CORR(A,B)").is_err() && parse("=CORR(A,B,0)").is_err());
    }

    #[test]
    fn templates_take_parameters_and_nest() {
        let t = builtin_templates();
        assert_eq!(t.len(), BUILTIN_TEMPLATES.len());
        // A built-in template equals its expansion.
        assert_eq!(
            parse("=PAIRZ(BTC,ETH,50)").unwrap().expr,
            parse("=Z(BTC/ETH,50)").unwrap().expr
        );
        // Templates inside templates; fields on parameters.
        let f = parse("=RESID(SOL,BTC,30) + VOLZ(SOL,20)").unwrap();
        assert_eq!(
            f.sources(),
            [
                Source::Spot("BTCUSDT".into()),
                Source::Spot("SOLUSDT".into())
            ]
        );
        // The user's own, used next to the built-ins.
        let mut mine = t.clone();
        let my = define("spread3(a, b, c, n) = Z((a+b)/2 - c, n)", &mine).unwrap();
        assert_eq!(my.name, "SPREAD3");
        mine.insert(my.name.clone(), my);
        let g = parse_with("=SPREAD3(BTC,ETH,SOL,40)", &mine).unwrap();
        assert_eq!(g.sources().len(), 3);
        assert!(parse("=SPREAD3(BTC,ETH,SOL,40)").is_err(), "not built in");
        // Wrong use is refused with the definition shown.
        assert!(parse("=PAIRZ(BTC,ETH)")
            .unwrap_err()
            .contains("PAIRZ(A,B,N)"));
        assert!(
            parse("=PAIRZ(BTC,ETH,0)").is_err(),
            "window checked after expansion"
        );
        assert!(parse("=VOLZ(BTC+ETH,20)").is_err(), "VOL of a sum");
        for bad in [
            "X = 1",
            "SMA(a) = a",
            "OK() = 1",
            "OK(a) =",
            "9X(a) = a",
            "OK(a,a,a,a,a,a,a,a,a) = a",
        ] {
            assert!(define(bad, &t).is_err(), "{bad}");
        }
    }

    #[test]
    fn readings_join_candles_as_of_their_time_and_never_narrow_the_timeline() {
        let f = parse("=FR:btc * 100 + oi:ETH + LS:SOL + TK:XRPUSDT - F:BTC").unwrap();
        let labels: Vec<String> = f.sources().iter().map(Source::label).collect();
        assert!(labels.contains(&"FR:BTCUSDT".into()) && labels.contains(&"OI:ETHUSDT".into()));
        assert!(labels.contains(&"F:BTCUSDT".into()) && labels.contains(&"TK:XRPUSDT".into()));
        // Hourly candles; funding every 8 h, starting at hour 3.
        let a: Vec<Ohlc> = (0..72).map(|i| k(i * H, 1.0, 1.0, 1.0, 1.0)).collect();
        let fr: Vec<Ohlc> = (0..5)
            .map(|j| k(3 * H + j * 8 * H, j as f64, j as f64, j as f64, j as f64))
            .collect();
        let (sa, sf) = (
            Source::Spot("AUSDT".into()),
            Source::Funding("AUSDT".into()),
        );
        let d = data(&[(sa.clone(), a), (sf.clone(), fr)]);
        let al = align(&[sa, sf], &d, Timeframe::H1).unwrap();
        assert_eq!(
            al.times.len(),
            72,
            "the readings do not narrow the timeline"
        );
        let v = parse("=FR:AUSDT")
            .unwrap()
            .series(&al, Field::Close)
            .unwrap();
        assert!(v[2].is_nan(), "before the first reading");
        assert_eq!(
            (v[3], v[10], v[11], v[12]),
            (0.0, 0.0, 1.0, 1.0),
            "the last reading at the open"
        );
        // The last reading (hour 35) is good for 3 x 8 h, then stale.
        assert_eq!(v[59], 4.0);
        assert!(v[60].is_nan());
        // Readings alone: their own times.
        let only = parse("=FR:AUSDT * 2")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert_eq!(only.len(), 5);
    }

    #[test]
    fn weather_sources_parse_with_places_and_variables() {
        let f = parse("=W:kyiv.temp - W:\"New York\".TEMP + BTC").unwrap();
        let s = f.sources();
        assert!(s.contains(&Source::Weather("KYIV".into(), "TEMP".into())));
        assert!(s.contains(&Source::Weather("NEW YORK".into(), "TEMP".into())));
        assert_eq!(
            Source::Weather("NEW YORK".into(), "RAIN".into()).label(),
            "W:\"NEW YORK\".RAIN"
        );
        assert!(Source::Weather("KYIV".into(), "TEMP".into()).is_step());
        let m = parse("=M:DGS10 - M:DGS2 + H:FNG / 100 + BTC").unwrap();
        let e = parse("=H:FNG / 1e2 + 2.5E-3 * BTC").unwrap();
        assert_eq!(e.sources().len(), 2);
        assert!(m.sources().contains(&Source::Macro("DGS10".into())));
        assert!(m.sources().contains(&Source::Http("FNG".into())));
        assert!(Source::Http("FNG".into()).is_step() && Source::Macro("X".into()).is_step());
        for bad in [
            "=W:KYIV",
            "=W:KYIV.SNOWFLAKES",
            "=W:\"KYIV.TEMP",
            "=W:.TEMP",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn windows_warm_up_and_bad_values_become_gaps() {
        let a: Vec<Ohlc> = (0..10)
            .map(|i| {
                let v = i as f64 - 4.0; // crosses zero
                k(i * H, v, v, v, v)
            })
            .collect();
        let d = data(&[(Source::Spot("AUSDT".into()), a)]);
        let sma = parse("=SMA(A,3)")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert_eq!(sma.len(), 8, "the first two candles have no full window");
        assert_eq!(sma[0].close, -3.0);
        let inv = parse("=1/A").unwrap().evaluate(&d, Timeframe::H1).unwrap();
        assert!(inv[4].is_gap, "1/0 is a gap");
        assert!(!inv[5].is_gap);
        let idx = parse("=IDX(A+10)")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert_eq!(idx[0].close, 100.0);
        let chg = parse("=CHG(A+10,1)")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert!((chg[0].close - (7.0 / 6.0 - 1.0) * 100.0).abs() < 1e-9);
        let e = ema(&[1.0, 2.0, 3.0, 4.0], 2);
        assert!(e[0].is_nan() && e[1] == 1.5 && (e[2] - 2.5).abs() < 1e-9);
        let z = parse("=Z(A,5)")
            .unwrap()
            .evaluate(&d, Timeframe::H1)
            .unwrap();
        assert!(z.iter().all(|k| k.is_gap || k.close.is_finite()));
    }
}
