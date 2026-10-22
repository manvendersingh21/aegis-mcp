//! Phase 6 UI: advocate advice on consent cards, "Remember this decision",
//! "My rules" card, "Connect a real agent" card. Stub — owned by the HACP worker.
use maud::{html, Markup};

/// Cards inserted into `/u/{subject}` (rules + connect-agent).
pub fn cards(_subject: &str) -> Markup {
    html! {}
}

/// Script inlined after the page's main script and KEYS_JS.
pub const P6_JS: &str = "";
