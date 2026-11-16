//! F8 — agent connector status: whether the socket is up, which roles have
//! called in, and the audit log of recent calls.

use crate::i18n::pick;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::data::db::{AgentLogRow, MandateRow};
use crate::tokens::Palette;
use crate::widgets::fmt::{fmt_clock_ms, render_clipped, truncate};

pub struct RoleRow {
    pub role: &'static str,
    /// Tools this role may call.
    pub tools: usize,
    pub calls: u64,
    pub denied: u64,
    /// Epoch millis of the last call (0 = never).
    pub last_call_ms: i64,
}

pub struct AgentsView<'a> {
    /// Socket path while the connector is listening.
    pub socket: Option<&'a str>,
    /// Why it is off, when `--agent` was given but start failed.
    pub error: Option<&'a str>,
    pub rows: Vec<RoleRow>,
    pub log: Vec<AgentLogRow>,
    pub now_ms: i64,
    /// Requests refused for carrying an unknown token.
    pub rejected: u64,
    /// The scalper's current mandate, one line each (empty = none).
    pub mandate: Vec<String>,
    /// The scalper's own paper ledger, one line each (empty = none yet).
    pub ledger: Vec<String>,
    /// Whether that mandate is currently allowing orders.
    pub mandate_active: bool,
    /// Past mandates, newest first.
    pub mandates: Vec<MandateRow>,
}

fn ago(now_ms: i64, then_ms: i64) -> String {
    if then_ms <= 0 {
        return pick("never", "ніколи").into();
    }
    let s = ((now_ms - then_ms) / 1000).max(0);
    match s {
        0..=59 => pick(format!("{s}s ago"), format!("{s} с тому")),
        60..=3599 => pick(format!("{}m ago", s / 60), format!("{} хв тому", s / 60)),
        _ => pick(format!("{}h ago", s / 3600), format!("{} г тому", s / 3600)),
    }
}

pub fn render(frame: &mut Frame, area: Rect, view: &AgentsView, palette: &Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.text).bg(palette.bg))
        .title(Span::styled(
            pick(" Agents ", " Агенти "),
            Style::default()
                .fg(palette.accent)
                .bg(palette.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(palette.text).bg(palette.bg));
    let inner = block.inner(area);
    render_clipped(frame, block, area);
    if inner.height == 0 || inner.width < 4 {
        return;
    }
    let text = Style::default().fg(palette.text).bg(palette.bg);
    let muted = Style::default().fg(palette.muted).bg(palette.bg);
    let bold = |c| {
        Style::default()
            .fg(c)
            .bg(palette.bg)
            .add_modifier(Modifier::BOLD)
    };

    let mut y = inner.y;
    let line = |y: u16| Rect {
        x: inner.x + 1,
        y,
        width: inner.width.saturating_sub(2),
        height: 1,
    };
    let put = |frame: &mut Frame, y: &mut u16, s: String, style: Style| {
        if *y < inner.y + inner.height {
            render_clipped(
                frame,
                Paragraph::new(truncate(&s, line(*y).width as usize).to_string()).style(style),
                line(*y),
            );
            *y += 1;
        }
    };

    match (view.socket, view.error) {
        (Some(p), _) => put(
            frame,
            &mut y,
            pick(
                format!("Connector ON   socket {p}"),
                format!("Конектор УВІМКНЕНО   сокет {p}"),
            ),
            bold(palette.ok),
        ),
        (None, Some(e)) => put(
            frame,
            &mut y,
            pick(
                format!("Connector FAILED: {e}"),
                format!("Конектор НЕ ЗАПУСТИВСЯ: {e}"),
            ),
            bold(palette.accent),
        ),
        (None, None) => put(
            frame,
            &mut y,
            pick(
                "Connector OFF. Start the terminal with:  dos-commander --agent",
                "Конектор ВИМКНЕНО. Запусти термінал так:  dos-commander --agent",
            )
            .to_string(),
            bold(palette.accent),
        ),
    }
    if view.rejected > 0 {
        put(
            frame,
            &mut y,
            pick(
                format!("{} request(s) refused: unknown token", view.rejected),
                format!("{} запит(ів) відхилено: невідомий токен", view.rejected),
            ),
            bold(palette.accent),
        );
    }
    put(
        frame,
        &mut y,
        pick(
            "Connect an agent: `dos-commander agent-token <role>`, then register the MCP server `dos-commander mcp --role <role>` with DOS_AGENT_TOKEN set.",
            "Підключення: `dos-commander agent-token <role>`, потім MCP-сервер `dos-commander mcp --role <role>` з DOS_AGENT_TOKEN.",
        )
        .to_string(),
        muted,
    );
    y += 1;

    put(
        frame,
        &mut y,
        format!(
            "{:<12} {:>6} {:>7} {:>8}  {}",
            pick("ROLE", "РОЛЬ"),
            pick("TOOLS", "ІНСТР."),
            pick("CALLS", "ВИКЛИКИ"),
            pick("DENIED", "ВІДМОВ."),
            pick("LAST CALL", "ОСТАННІЙ")
        ),
        bold(palette.accent_dim),
    );
    for r in &view.rows {
        let style = if r.denied > 0 {
            bold(palette.accent)
        } else {
            text
        };
        put(
            frame,
            &mut y,
            format!(
                "{:<12} {:>6} {:>7} {:>8}  {}",
                r.role,
                r.tools,
                r.calls,
                r.denied,
                ago(view.now_ms, r.last_call_ms)
            ),
            style,
        );
    }
    y += 1;

    put(
        frame,
        &mut y,
        pick(
            "Scalper mandate (issue with i, revoke with x)",
            "Мандат скальпера (видати: i, відкликати: x)",
        )
        .to_string(),
        bold(palette.accent_dim),
    );
    if view.mandate.is_empty() {
        put(
            frame,
            &mut y,
            pick(
                "(none: the scalper cannot place orders without one)",
                "(немає: без мандата скальпер не ставить ордерів)",
            )
            .to_string(),
            muted,
        );
    }
    for l in view.mandate.iter().chain(&view.ledger) {
        let style = if view.mandate_active { text } else { muted };
        put(frame, &mut y, l.clone(), style);
    }
    for m in view.mandates.iter().take(3) {
        // Mandates before separate ledgers (balance 0) traded the shared one.
        let ledger = if m.balance > 0.0 {
            format!("  bal ${} PnL {:+.2}", m.balance, m.pnl_usd)
        } else {
            String::new()
        };
        put(
            frame,
            &mut y,
            format!(
                "  #{} {}  ${}/order {}/h loss ${}  orders {}  loss ${:.2}{}  {}",
                m.id,
                m.list,
                m.max_order_usd,
                m.max_orders_hour,
                m.max_loss_usd,
                m.orders_total,
                m.loss_usd,
                ledger,
                m.status
            ),
            muted,
        );
    }
    y += 1;

    put(
        frame,
        &mut y,
        pick(
            format!("Recent calls ({} shown, newest first)", view.log.len()),
            format!("Останні виклики (показано {}, нові зверху)", view.log.len()),
        ),
        bold(palette.accent_dim),
    );
    if view.log.is_empty() {
        put(
            frame,
            &mut y,
            pick("(none yet)", "(ще немає)").to_string(),
            muted,
        );
    }
    for e in &view.log {
        let style = if e.status == "ok" {
            text
        } else {
            bold(palette.accent)
        };
        put(
            frame,
            &mut y,
            format!(
                "{}  {:<11} {:<22} {}",
                fmt_clock_ms(e.ts),
                e.role,
                e.tool,
                e.status
            ),
            style,
        );
    }
}
