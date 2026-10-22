//! "Keys" card for `/u/{subject}`: provider secrets (write-only), fake-key issuance, listing, revoke.
use maud::{html, Markup};

/// The "Keys" card. The session identifies the subject server-side, so the markup is static.
pub fn keys_card(_subject: &str) -> Markup {
    html! {
        section.card.cred #keys-card {
            div.cred-h {
                div.eyebrow { "Keys" }
                span.dim { "agents hold " code { "aegis_fk_…" } " fake keys; the real key stays in the broker" }
            }
            h3 { "Provider secrets" }
            div #keys-secrets { div.dim { "loading…" } }
            form #key-secret-form.cred-row autocomplete="off" {
                select #ks-provider aria-label="Provider" {
                    option value="stripe" { "stripe" }
                    option value="jev" { "jev" }
                    option value="llm" { "llm" }
                }
                input #ks-label type="text" placeholder="label" autocomplete="off";
                input #ks-value type="password" placeholder="secret value (never shown again)" autocomplete="new-password";
                button.sec type="submit" { "Add secret" }
            }
            h3 { "Issue fake key for an agent" }
            form #key-fake-form.cred-row autocomplete="off" {
                input #kf-agent type="text" placeholder="agent id" autocomplete="off";
                select #kf-provider aria-label="Provider" {
                    option value="stripe" { "stripe" }
                    option value="jev" { "jev" }
                    option value="llm" { "llm" }
                }
                select #kf-ttl aria-label="Lifetime" {
                    option value="900" { "15 min" }
                    option value="3600" selected { "1 hour" }
                    option value="86400" { "24 hours" }
                }
                button.sec type="submit" { "Issue fake key" }
            }
            div #keys-issued {}
            div #keys-err.err {}
            h3 { "Fake keys" }
            div #keys-fake { div.dim { "loading…" } }
        }
    }
}

/// Script driving the Keys card. Exposes `keysRefresh()` and `keysRevokeAll()` (for the kill switch).
pub const KEYS_JS: &str = r#"
(function(){
  const g = id => document.getElementById(id);
  if (!g('keys-card')) return;
  const kesc = s => String(s == null ? '' : s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const J = {'content-type': 'application/json'};
  const err = m => { g('keys-err').textContent = m || ''; };
  const nowS = () => Math.floor(Date.now() / 1000);
  const countdown = t => { const d = Number(t) - nowS(); if (d <= 0) return 'expired'; return d < 60 ? d + 's' : d < 3600 ? Math.floor(d / 60) + 'm' : Math.floor(d / 3600) + 'h ' + Math.floor(d % 3600 / 60) + 'm'; };
  const ago = t => { if (t == null) return 'never'; const d = Math.max(0, nowS() - Number(t)); return d < 60 ? d + 's ago' : d < 3600 ? Math.floor(d / 60) + 'm ago' : Math.floor(d / 3600) + 'h ago'; };
  let fakeKeys = [];

  function draw(data) {
    const secrets = (data && data.secrets) || [];
    fakeKeys = (data && data.fake_keys) || [];
    g('keys-secrets').innerHTML = secrets.length ? secrets.map(s =>
      '<div class="le"><span class="chip">' + kesc(s.provider) + '</span> ' + kesc(s.label)
      + ' <code>••••' + kesc(s.last4) + '</code>' + (s.revoked ? ' <span class="pill-r">revoked</span>' : '') + '</div>').join('')
      : '<div class="dim">no secrets stored</div>';
    drawFake();
  }
  function drawFake() {
    g('keys-fake').innerHTML = fakeKeys.length ? fakeKeys.map((k, i) => {
      const dead = k.revoked || Number(k.expires_at) <= nowS();
      return '<div class="le ' + (dead ? 'denied' : 'released') + '"><code>' + kesc(k.prefix) + '…</code> '
        + '<span class="chip">' + kesc(k.provider) + '</span> agent <b>' + kesc(k.agent_id) + '</b>'
        + '<div class="h">' + (k.revoked ? 'revoked' : 'expires in ' + kesc(countdown(k.expires_at)))
        + ' · uses ' + kesc(k.uses) + ' · last used ' + kesc(ago(k.last_used_at)) + '</div>'
        + (k.revoked ? '' : '<button class="sec" data-i="' + i + '">Revoke</button>') + '</div>';
    }).join('') : '<div class="dim">no fake keys issued</div>';
    g('keys-fake').querySelectorAll('button[data-i]').forEach(b => b.addEventListener('click', () => revoke(fakeKeys[Number(b.dataset.i)].prefix)));
  }
  async function refresh() {
    try {
      const r = await fetch('/api/keys');
      if (!r.ok) throw new Error('Could not load keys');
      draw(await r.json()); err('');
    } catch (x) { err((x && x.message) || 'Network error'); }
  }
  async function revoke(prefix) {
    try {
      const r = await fetch('/api/keys/revoke', {method: 'POST', headers: J, body: JSON.stringify({prefix: prefix})});
      if (!r.ok) throw new Error('Revoke failed');
      await refresh();
    } catch (x) { err((x && x.message) || 'Network error'); }
  }
  g('key-secret-form').addEventListener('submit', async e => {
    e.preventDefault();
    const value = g('ks-value').value;
    if (!value) { err('Enter a secret value'); return; }
    try {
      const r = await fetch('/api/keys/secrets', {method: 'POST', headers: J,
        body: JSON.stringify({provider: g('ks-provider').value, label: g('ks-label').value, value: value})});
      g('ks-value').value = '';
      if (!r.ok) throw new Error('Could not store secret');
      g('ks-label').value = ''; await refresh();
    } catch (x) { g('ks-value').value = ''; err((x && x.message) || 'Network error'); }
  });
  g('key-fake-form').addEventListener('submit', async e => {
    e.preventDefault();
    const agent = g('kf-agent').value.trim();
    if (!agent) { err('Enter an agent id'); return; }
    try {
      const r = await fetch('/api/keys/fake', {method: 'POST', headers: J,
        body: JSON.stringify({agent_id: agent, provider: g('kf-provider').value, ttl_secs: Number(g('kf-ttl').value)})});
      if (!r.ok) throw new Error('Could not issue key');
      const k = await r.json();
      g('keys-issued').innerHTML = '<div class="consent pay"><h4>Fake key for ' + kesc(k.info && k.info.agent_id) + ' — shown once</h4>'
        + '<div class="cred-row"><code id="fk-val"></code> <button class="sec" type="button" id="fk-copy">Copy</button></div>'
        + '<p class="hint">This key is worthless outside Aegis — only the broker can swap it for the real credential.</p></div>';
      g('fk-val').textContent = k.fake_key;
      g('fk-copy').addEventListener('click', async () => {
        try { await navigator.clipboard.writeText(k.fake_key); g('fk-copy').textContent = 'Copied ✓'; }
        catch (_) { const rg = document.createRange(); rg.selectNode(g('fk-val')); getSelection().removeAllRanges(); getSelection().addRange(rg); }
      });
      err(''); await refresh();
    } catch (x) { err((x && x.message) || 'Network error'); }
  });
  window.keysRefresh = refresh;
  window.keysRevokeAll = async () => { await revoke(null); g('keys-issued').innerHTML = ''; };
  refresh();
  setInterval(drawFake, 1000);
  setInterval(refresh, 15000);
})();
"#;
