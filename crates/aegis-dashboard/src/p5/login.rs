//! Wallet-only `/login`: SIWE via `window.ethereum`, plus clearly labelled DEMO wallets.
use aegis_core::demo_vaults;
use maud::{html, Markup, PreEscaped};

use super::super::{layout, pill_header, GLOBE_JS, SIWE_JS};

/// Full `/login` page (wallet-only; demo wallet fallback). No password input.
pub fn login_page() -> Markup {
    let people: Vec<_> = demo_vaults().iter().map(|v| v.person()).collect();
    layout(
        "AegisMCP — Log in",
        html! {
            (pill_header(html! {}))
            main.wrap.narrow {
                div.eyebrow { "Secure access" }
                h1.mid { "Sign in with your " span.acc { "passkey" } " or " span.acc { "wallet" } }
                (crate::p6::passkey_login_button())
                div.card.form.web3 {
                    label { "Ethereum wallet" }
                    button.primary #siwe type="button" { "Sign in with Ethereum" span.arrow { "↘" } }
                    div #siwe-err.err {}
                    p #nowallet.hint style="display:none" { "No wallet detected — install MetaMask or Rabby, or use a demo wallet below." }
                    p.hint { "No passwords. You sign a message; your key never leaves the wallet." }
                }
                div.card.form.web3 {
                    label { "Demo wallets " span.pill-w { "DEMO" } }
                    p.hint { "Stage-safe fallback: Aegis signs for a throwaway demo wallet. Not a real identity." }
                    @for p in &people {
                        button.sec.demo type="button" data-subject=(p.subject) {
                            "Use demo wallet — " (p.display_name) " " span.pill-w { "DEMO" }
                        }
                    }
                    div #demo-err.err {}
                }
            }
            script { (PreEscaped(SIWE_JS)) }
            script { (PreEscaped(LOGIN_JS)) }
            script { (PreEscaped(crate::p6::PASSKEY_LOGIN_JS)) }
            script { (PreEscaped(GLOBE_JS)) }
        },
    )
}

const LOGIN_JS: &str = r#"
(function(){
  const params = new URLSearchParams(location.search);
  const post = (url, body) => fetch(url, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(body)});
  const safeNext = () => { const n = params.get('next'); return (n && n.startsWith('/') && !n.startsWith('//')) ? n : null; };
  async function finish(res, errEl) {
    if (!res.ok) { errEl.textContent = 'Sign-in failed'; return; }
    const j = await res.json().catch(() => ({}));
    const next = safeNext();
    if (next) location.href = next;
    else if (j.subject) location.href = '/u/' + encodeURIComponent(j.subject);
    else { const me = await (await fetch('/api/me')).json(); location.href = '/u/' + encodeURIComponent(me.subject); }
  }
  if (!window.ethereum) document.getElementById('nowallet').style.display = '';
  document.getElementById('siwe').addEventListener('click', async () => {
    const e = document.getElementById('siwe-err');
    e.textContent = '';
    if (!window.ethereum) { document.getElementById('nowallet').style.display = ''; return; }
    try { await finish(await post('/api/login/siwe', await siweSign()), e); }
    catch (x) {
      const m = (x && x.message) || 'Wallet sign-in cancelled';
      e.textContent = /not linked/i.test(m)
        ? 'This wallet is not linked to anyone yet. Sign in once with your demo wallet below, then use “Link wallet” on your dashboard — after that this wallet signs you in.'
        : m;
    }
  });
  document.querySelectorAll('button.demo').forEach(b => b.addEventListener('click', async () => {
    const e = document.getElementById('demo-err');
    e.textContent = '';
    const subject = b.dataset.subject;
    try {
      const wres = await fetch('/api/demo-wallet/' + encodeURIComponent(subject));
      if (!wres.ok) throw new Error('Demo wallets are disabled');
      const address = (await wres.json()).address;
      const nres = await fetch('/api/siwe/nonce');
      if (!nres.ok) throw new Error('Could not get nonce');
      const nonce = (await nres.json()).nonce;
      const message = location.host + ' wants you to sign in with your Ethereum account:\n' + address + '\n\n'
        + 'Sign in to AegisMCP — your consent firewall.\n\n'
        + 'URI: ' + location.origin + '\nVersion: 1\nChain ID: 84532\nNonce: ' + nonce
        + '\nIssued At: ' + new Date().toISOString();
      const sres = await post('/api/demo-wallet/sign', {subject: subject, message: message});
      if (!sres.ok) throw new Error('Demo signing failed');
      const signature = (await sres.json()).signature;
      await finish(await post('/api/login/siwe', {message: message, signature: signature}), e);
    } catch (x) { e.textContent = (x && x.message) || 'Demo sign-in failed'; }
  }));
})();
"#;
