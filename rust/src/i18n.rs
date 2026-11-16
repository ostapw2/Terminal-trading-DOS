//! Interface language.  One process-wide choice (`:lang uk|en`, F9 Settings,
//! kv `lang`); text that is translated says both versions where it is drawn:
//! `i18n::pick("Together", "Разом")`.  Ukrainian is the default.  Screens not
//! yet translated stay English in either mode.

use std::sync::atomic::{AtomicBool, Ordering};

// Unit tests read English text: only the shipped binary starts in Ukrainian.
static UK: AtomicBool = AtomicBool::new(!cfg!(test));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Uk,
    En,
}

impl Lang {
    /// `uk` / `en` (what kv `lang` holds).
    pub fn code(self) -> &'static str {
        match self {
            Lang::Uk => "uk",
            Lang::En => "en",
        }
    }

    pub fn parse(s: &str) -> Option<Lang> {
        match s.trim().to_ascii_lowercase().as_str() {
            "uk" | "ua" => Some(Lang::Uk),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

pub fn set(l: Lang) {
    UK.store(l == Lang::Uk, Ordering::Relaxed);
}

pub fn get() -> Lang {
    if UK.load(Ordering::Relaxed) {
        Lang::Uk
    } else {
        Lang::En
    }
}

/// The text in the chosen language.
pub fn pick<T>(en: T, uk: T) -> T {
    if UK.load(Ordering::Relaxed) {
        uk
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not `set`: the choice is process-wide and other tests read it.
    #[test]
    fn codes_round_trip() {
        assert_eq!(Lang::parse("UA"), Some(Lang::Uk));
        assert_eq!(Lang::parse(Lang::En.code()), Some(Lang::En));
        assert_eq!(Lang::parse("de"), None);
    }
}
