// AUTO-GENERATED FROM shared/tokens.toml — DO NOT EDIT BY HAND.
//! Generated design tokens.  Source: shared/tokens.toml.

use ratatui::style::Color;

pub struct Palette {
    pub bg: Color,
    pub panel: Color,
    pub black: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_dim: Color,
    pub light_cyan: Color,
    pub cursor_bg: Color,
    pub cursor_fg: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    pub name: &'static str,
}

pub const CLASSIC: Palette = Palette {
    bg: Color::Rgb(0, 0, 170),
    panel: Color::Rgb(0, 170, 170),
    black: Color::Rgb(0, 0, 0),
    text: Color::Rgb(255, 255, 255),
    muted: Color::Rgb(170, 170, 170),
    accent: Color::Rgb(255, 255, 85),
    accent_dim: Color::Rgb(0, 170, 170),
    light_cyan: Color::Rgb(85, 255, 255),
    cursor_bg: Color::Rgb(255, 255, 255),
    cursor_fg: Color::Rgb(0, 0, 170),
    ok: Color::Rgb(85, 255, 85),
    warn: Color::Rgb(255, 255, 85),
    error: Color::Rgb(170, 0, 0),
    name: "dos-classic",
};

pub const MODERN: Palette = Palette {
    bg: Color::Rgb(20, 22, 28),
    panel: Color::Rgb(28, 32, 40),
    black: Color::Rgb(10, 11, 14),
    text: Color::Rgb(220, 225, 235),
    muted: Color::Rgb(140, 148, 165),
    accent: Color::Rgb(98, 152, 255),
    accent_dim: Color::Rgb(58, 92, 160),
    light_cyan: Color::Rgb(120, 180, 255),
    cursor_bg: Color::Rgb(98, 152, 255),
    cursor_fg: Color::Rgb(20, 22, 28),
    ok: Color::Rgb(120, 200, 140),
    warn: Color::Rgb(220, 180, 90),
    error: Color::Rgb(220, 80, 80),
    name: "dos-modern",
};

pub mod glyphs {
    pub const BORDER_CORNER: &str = "+";
    pub const BORDER_H: &str = "-";
    pub const BORDER_V: &str = "|";
    pub const CHECKBOX_ON: &str = "[X]";
    pub const CHECKBOX_OFF: &str = "[ ]";
    pub const RADIO_ON: &str = "(*)";
    pub const RADIO_OFF: &str = "( )";
    pub const SWITCH_ON: &str = "[ON ]";
    pub const SWITCH_OFF: &str = "[off]";
    pub const SELECT_DOWN: &str = "v";
    pub const SELECT_UP: &str = "^";
    pub const SCROLLBAR_THUMB: &str = "#";
    pub const SCROLLBAR_TRACK: &str = " ";
    pub const DIR_PREFIX: &str = "/";
    pub const DIR_SIZE: &str = "[DIR]";
    pub const UP_DIR: &str = "/..";
    pub const UP_DIR_SIZE: &str = "UP--DIR";
    pub const SECTION_RULE: &str = "--";
    pub const TRUNCATE: &str = "~";
}

pub mod heights {
    pub const MENU_BAR: u16 = 1;
    pub const STATUS_LINE: u16 = 1;
    pub const FUNCTION_BAR: u16 = 1;
    pub const FORM_LABEL: u16 = 1;
    pub const FORM_ROW: u16 = 1;
    pub const FORM_BUTTON: u16 = 1;
    pub const FORM_INPUT: u16 = 1;
    pub const FORM_SELECT: u16 = 1;
    pub const FORM_CHECK: u16 = 1;
    pub const FORM_RADIO: u16 = 1;
    pub const FORM_SWITCH: u16 = 1;
    pub const FORM_SECTION: u16 = 1;
    pub const FORM_SPACER: u16 = 1;
}

pub mod spacing {
    pub const FORM_LABEL_WIDTH: u16 = 10;
    pub const FORM_BUTTON_MIN: u16 = 10;
    pub const MODAL_WIDTH: u16 = 64;
    pub const MODAL_PADDING_V: u16 = 1;
    pub const MODAL_PADDING_H: u16 = 2;
}
