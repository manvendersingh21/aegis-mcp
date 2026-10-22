//! Phase 5 UI: wallet-only login + Keys card. Stub — owned by the HACP worker.
use maud::{html, Markup};

/// Full `/login` page (wallet-only; demo wallet fallback).
pub fn login_page() -> Markup {
    html! {}
}

/// The "Keys" card for `/u/{subject}`.
pub fn keys_card(_subject: &str) -> Markup {
    html! {}
}

/// Script driving the Keys card (inlined after the page's main script).
pub const KEYS_JS: &str = "";
