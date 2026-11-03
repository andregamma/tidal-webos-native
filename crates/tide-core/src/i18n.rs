//! Interface / content languages.
//!
//! One list drives three things: the Settings select, the value persisted by `tide-api`'s
//! settings storage, and (later) the lookup key of the app's own translated strings. Codes use the
//! `language-REGION` form (`pt-BR`). The Tidal client wants `pt_BR`; that conversion belongs to the
//! backend (`tide-api::tidlers_backend`), not here.
//!
//! The list follows the languages TIDAL itself offers in its apps / help centre. TIDAL publishes no
//! machine-readable list, so to add a language append a row to [`LANGUAGES`].

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Language {
    /// `language-REGION`, e.g. `pt-BR`.
    pub code: &'static str,
    /// Name in the language itself, as shown in the select.
    pub name: &'static str,
}

/// What the app uses until the user picks something else (also the Tidal client's own default).
pub const DEFAULT_LOCALE: &str = "en-US";

pub const LANGUAGES: &[Language] = &[
    Language { code: "en-US", name: "English" },
    Language { code: "pt-BR", name: "Português (Brasil)" },
    Language { code: "pt-PT", name: "Português (Portugal)" },
    Language { code: "es-ES", name: "Español" },
    Language { code: "fr-FR", name: "Français" },
    Language { code: "de-DE", name: "Deutsch" },
    Language { code: "it-IT", name: "Italiano" },
    Language { code: "pl-PL", name: "Polski" },
    Language { code: "cs-CZ", name: "Čeština" },
    Language { code: "bg-BG", name: "Български" },
    Language { code: "nb-NO", name: "Norsk" },
    Language { code: "hr-HR", name: "Hrvatski" },
    Language { code: "sl-SI", name: "Slovenščina" },
    Language { code: "sr-RS", name: "Srpski" },
];

/// Map any reasonable spelling (`pt_br`, `PT-BR`, bare `pt`) to a code from [`LANGUAGES`];
/// anything unknown becomes [`DEFAULT_LOCALE`]. Used on every value that comes from outside
/// (saved file, UI), so the rest of the app can trust the string.
pub fn resolve(code: &str) -> &'static str {
    let norm = code.trim().replace('_', "-");
    if let Some(l) = LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(&norm)) {
        return l.code;
    }
    // Language only (`pt`) or an unlisted region (`pt-AO`): first entry of that language.
    let lang = norm.split('-').next().unwrap_or("");
    LANGUAGES
        .iter()
        .find(|l| !lang.is_empty() && l.code.split('-').next().is_some_and(|p| p.eq_ignore_ascii_case(lang)))
        .map_or(DEFAULT_LOCALE, |l| l.code)
}

/// Display name for a code (falls back to the code itself).
pub fn language_name(code: &str) -> &str {
    let code = resolve(code);
    LANGUAGES.iter().find(|l| l.code == code).map_or(code, |l| l.name)
}
