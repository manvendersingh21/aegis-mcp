//! Phase 6 UI: "My rules" (standing rules) + "Connect a real agent" cards,
//! advocate advice (`#advice-<request_id>`) and "Remember this decision" on
//! consent cards, and the `rule_applied` ledger row. Vanilla JS, no modules;
//! every dynamic string rendered into HTML goes through `p6esc`.
use maud::{html, Markup};

/// Phase 6 cards for `/u/{subject}`. The session identifies the subject
/// server-side, so the markup is static (mirrors `p5::keys_card`).
pub fn cards(_subject: &str) -> Markup {
    html! {
        section.card.cred #p6-rules-card {
            div.cred-h {
                div.eyebrow { "Standing rules" }
                span.dim { "matched before a consent prompt is shown" }
            }
            h3 { "My rules" }
            div #rules-list { div.dim { "loading…" } }
            div #rules-err.err {}
            h3 { "Quick add" }
            div.presets {
                button.sec type="button" data-rule="calendar" { "Calendar: free/busy only, any agent" }
                button.sec type="button" data-rule="health" { "Never share health data" }
                button.sec type="button" data-rule="payments" { "Payments under $500: clean room" }
            }
        }
        section.card.cred #p6-agent-card {
            div.cred-h {
                div.eyebrow { "For real agents" }
                span.dim { "MCP over HTTP · any standards-compliant client" }
            }
            h3 { "Connect a real agent" }
            form #mcp-form.cred-row autocomplete="off" {
                input #mcp-agent type="text" placeholder="agent id (e.g. my-claude)" autocomplete="off";
                button.sec type="submit" { "Issue MCP credential" }
            }
            div #mcp-err.err {}
            div #mcp-issued {}
        }
        section.card.cred #p6-passkey-card {
            div.cred-h {
                div.eyebrow { "Hardware security" }
                span.dim { "WebAuthn · TouchID / FaceID" }
            }
            h3 { "Passkeys" }
            div.cred-row {
                button.sec #passkey-reg type="button" { "Add a passkey" }
            }
            div #passkey-reg-status.hint {}
            div #passkey-reg-err.err {}
        }
        section.card.cred #p6-vault-limits-card {
            div.cred-h {
                div.eyebrow { "Vault Controls & Privacy" }
                span.dim { "editable only when logged in · agents never see raw street address" }
            }
            h3 { "Protected Delivery Address" }
            div.cred-row {
                code #vault-addr-display { "loading…" }
            }
            form #vault-addr-form.cred-row autocomplete="off" {
                input #vault-addr-input type="text" placeholder="Update delivery address" autocomplete="street-address";
                button.sec type="submit" { "Update Address" }
            }
            div #vault-addr-status.hint {}
            div #vault-addr-err.err {}

            h3 { "Daily Agent Spending Limit" }
            div.cred-row {
                span.dim { "Limit: " }
                b #vault-limit-display { "loading…" }
                span.dim { " · Spent today: " }
                b #vault-spent-display { "loading…" }
            }
            form #vault-limit-form.cred-row autocomplete="off" {
                input #vault-limit-input type="number" step="10" placeholder="New daily limit ($)" autocomplete="off";
                button.sec type="submit" { "Set Daily Limit" }
            }
            div #vault-limit-status.hint {}
            div #vault-limit-err.err {}
        }
    }
}

/// "Sign in with a passkey" card for `/login`.
pub fn passkey_login_button() -> Markup {
    html! {
        div.card.form.web3 #p6-passkey-login {
            label { "Biometric passkey" }
            button.primary #passkey-login type="button" { "Sign in with a passkey" span.arrow { "↘" } }
            div #passkey-err.err {}
            p.hint { "Fast and biometric. Uses your device's secure enclave (FaceID, TouchID, Windows Hello)." }
        }
    }
}

/// Script for passkey login on `/login`.
pub const PASSKEY_LOGIN_JS: &str = r#"
(function(){
  const b64uToBuf = b64 => {
    let s = b64.replace(/-/g, '+').replace(/_/g, '/');
    while (s.length % 4) s += '=';
    const bin = atob(s);
    const buf = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
    return buf.buffer;
  };
  const bufToB64u = buf => {
    const bin = String.fromCharCode(...new Uint8Array(buf));
    return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  };
  const g = id => document.getElementById(id);
  const btn = g('passkey-login');
  if (!btn) return;
  btn.addEventListener('click', async () => {
    const errEl = g('passkey-err');
    errEl.textContent = '';
    if (!navigator.credentials || !navigator.credentials.get) {
      errEl.textContent = 'WebAuthn passkeys not supported in this browser'; return;
    }
    try {
      const res = await fetch('/api/passkey/login/begin', {method: 'POST', headers: {'content-type':'application/json'}, body: '{}'});
      if (!res.ok) throw new Error('Could not begin passkey login');
      const opt = await res.json();
      opt.challenge = b64uToBuf(opt.challenge);
      const assertion = await navigator.credentials.get({publicKey: opt});
      const assertJson = {
        id: assertion.id,
        rawId: bufToB64u(assertion.rawId),
        type: assertion.type,
        response: {
          authenticatorData: bufToB64u(assertion.response.authenticatorData),
          clientDataJSON: bufToB64u(assertion.response.clientDataJSON),
          signature: bufToB64u(assertion.response.signature),
        }
      };
      const fin = await fetch('/api/passkey/login/finish', {method: 'POST', headers: {'content-type':'application/json'}, body: JSON.stringify(assertJson)});
      if (!fin.ok) {
        const e = await fin.json().catch(() => ({}));
        throw new Error(e.error || 'Passkey login failed');
      }
      const data = await fin.json();
      const next = new URLSearchParams(location.search).get('next');
      if (next && next.startsWith('/') && !next.startsWith('//')) location.href = next;
      else if (data.subject) location.href = '/u/' + encodeURIComponent(data.subject);
      else location.href = '/';
    } catch (x) { errEl.textContent = (x && x.message) || 'Passkey sign-in failed'; }
  });
})();
"#;

/// Script inlined after `KEYS_JS` on `/u/{subject}`. Exposes
/// `window.renderAdvice`, `window.rememberChoice` and `window.ruleApplied`
/// (all called from the page's SSE handler).
pub const P6_JS: &str = r#"
(function(){
  const p6esc = s => String(s == null ? '' : s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const g = id => document.getElementById(id);
  const J = {'content-type': 'application/json'};

  // Advocate advice injected into the pending consent card #advice-<request_id>.
  window.renderAdvice = function(request_id, advice) {
    const el = g('advice-' + request_id);
    if (!el || !advice) return;
    const rec = advice.recommend;
    const pill = rec
      ? '<span class="pill-b">Advocate: ' + p6esc(String(rec).toUpperCase().replace(/_/g, ' ')) + '</span>'
      : '<span class="pill-r">Advocate: DENY</span>';
    const rawFlags = advice.red_flags || [];
    const flags = rawFlags.map(f => '<li>' + p6esc(f) + '</li>').join('');
    el.innerHTML = '<div class="sum">' + pill + ' <span class="dim">· ' + p6esc(advice.source) + '</span></div>'
      + '<div class="sum">' + p6esc(advice.summary) + '</div>'
      + (flags ? '<ul class="flags">' + flags + '</ul>' : '');
  };

  // "Remember this decision" scope chosen on the consent card, or null.
  window.rememberChoice = function(request_id) {
    const s = g('remember-' + request_id);
    return (s && s.value) || null;
  };

  // A standing rule resolved a call without prompting: ledger row + pill RULE.
  window.ruleApplied = function(ev) {
    const lg = g('ledger');
    if (!lg) return;
    const rid = ev.rule_id, tool = ev.tool, agent = ev.agent_id;
    const d = document.createElement('div');
    d.className = 'le';
    d.innerHTML = '<span class="pill-b">RULE</span> Rule ' + p6esc(rid) + ' resolved <code>' + p6esc(tool) + '</code> for ' + p6esc(agent);
    lg.prepend(d);
  };

  if (!g('p6-rules-card')) return;   // the cards only live on /u/{subject}

  // -- My rules: CRUD against /api/rules ----------------------------------
  const q6 = encodeURIComponent(SUBJECT);
  const err = m => { g('rules-err').textContent = m || ''; };
  const PRESETS = {
    calendar: {tool: 'check_schedule', resource: 'calendar', decision: {action: 'allow', mode: 'redact'}, label: 'Calendar: free/busy only, any agent'},
    health: {tool: 'get_health_metrics', resource: 'health', decision: {action: 'deny'}, label: 'Never share health data'},
    payments: {tool: 'make_payment', resource: 'financials', max_amount_cents: 50000, decision: {action: 'allow', mode: 'clean_room'}, label: 'Payments under $500: clean room'}
  };
  const decisionText = d => !d ? '' : d.action === 'deny' ? 'deny' : d.action === 'ask' ? 'ask' : 'allow · ' + String(d.mode || '').toLowerCase();
  function drawRules(rules) {
    g('rules-list').innerHTML = rules.length ? rules.map(r => {
      const dt = decisionText(r.decision);
      return '<div class="le rule-row"><b>' + p6esc(r.label) + '</b>'
        + ' <span class="dim">' + p6esc(dt) + '</span>'
        + '<button class="sec" data-id="' + p6esc(r.rule_id) + '">Delete</button></div>';
    }).join('') : '<div class="dim">no standing rules yet — add one below</div>';
    g('rules-list').querySelectorAll('button[data-id]').forEach(b => b.addEventListener('click', () => delRule(b.dataset.id)));
  }
  async function loadRules() {
    try {
      const r = await fetch('/api/rules?subject=' + q6);
      if (!r.ok) throw new Error('Could not load rules');
      const data = await r.json();
      drawRules((data && data.rules) || []);
      err('');
    } catch (x) { err((x && x.message) || 'Network error'); }
  }
  async function addRule(body) {
    try {
      const r = await fetch('/api/rules?subject=' + q6, {method: 'POST', headers: J, body: JSON.stringify(body)});
      if (!r.ok) throw new Error('Could not add rule');
      await loadRules();
    } catch (x) { err((x && x.message) || 'Network error'); }
  }
  async function delRule(rule_id) {
    // DELETE /api/rules/<rule_id>?subject=<subject>
    try {
      const r = await fetch('/api/rules/' + encodeURIComponent(rule_id) + '?subject=' + q6, {method: 'DELETE'});
      if (!r.ok) throw new Error('Could not delete rule');
      await loadRules();
    } catch (x) { err((x && x.message) || 'Network error'); }
  }
  g('p6-rules-card').querySelectorAll('button[data-rule]').forEach(b => b.addEventListener('click', () => addRule(PRESETS[b.dataset.rule])));

  // -- Connect a real agent: issue an mcp credential, show it once ---------
  const mcpErr = m => { g('mcp-err').textContent = m || ''; };
  g('mcp-form').addEventListener('submit', async e => {
    e.preventDefault();
    const agent = g('mcp-agent').value.trim();
    if (!agent) { mcpErr('Enter an agent id'); return; }
    try {
      const r = await fetch('/api/keys/fake', {method: 'POST', headers: J,
        body: JSON.stringify({agent_id: agent, provider:"mcp", ttl_secs: 86400})});
      if (!r.ok) throw new Error('Could not issue credential');
      const k = await r.json();
      const issuer = k && k.info && k.info.agent_id;
      g('mcp-issued').innerHTML = '<div class="consent pay"><h4>MCP credential for ' + p6esc(issuer) + ' — shown once</h4>'
        + '<div class="cred-row"><code id="mcp-cred"></code> <button class="sec" type="button" id="mcp-copy">Copy</button></div>'
        + '<h3>Paste into Claude</h3>'
        + '<div class="cred-row"><code id="mcp-cmd"></code></div>'
        + '<p class="hint">This key only works through Aegis — worthless anywhere else.</p></div>';
      g('mcp-cred').textContent = k.fake_key;
      g('mcp-cmd').textContent = 'claude mcp add --transport http aegis http://127.0.0.1:8787/mcp --header "Authorization: Bearer ' + k.fake_key + '"';
      g('mcp-copy').addEventListener('click', async () => {
        try { await navigator.clipboard.writeText(k.fake_key); g('mcp-copy').textContent = 'Copied ✓'; }
        catch (_) { const rg = document.createRange(); rg.selectNode(g('mcp-cred')); getSelection().removeAllRanges(); getSelection().addRange(rg); }
      });
      mcpErr('');
    } catch (x) { mcpErr((x && x.message) || 'Network error'); }
  });

  // -- Passkeys: WebAuthn registration against /api/passkey/register -------
  const b64uToBuf = b64 => {
    let s = b64.replace(/-/g, '+').replace(/_/g, '/');
    while (s.length % 4) s += '=';
    const bin = atob(s);
    const buf = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
    return buf.buffer;
  };
  const bufToB64u = buf => {
    const bin = String.fromCharCode(...new Uint8Array(buf));
    return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  };
  const regBtn = g('passkey-reg');
  if (regBtn) {
    regBtn.addEventListener('click', async () => {
      const errEl = g('passkey-reg-err'), statusEl = g('passkey-reg-status');
      errEl.textContent = ''; statusEl.textContent = '';
      if (!navigator.credentials || !navigator.credentials.create) {
        errEl.textContent = 'WebAuthn passkeys not supported in this browser'; return;
      }
      try {
        const res = await fetch('/api/passkey/register/begin', {method: 'POST', headers: J, body: '{}'});
        if (!res.ok) throw new Error('Could not begin passkey registration');
        const opt = await res.json();
        opt.challenge = b64uToBuf(opt.challenge);
        if (opt.user && opt.user.id) opt.user.id = b64uToBuf(opt.user.id);
        const cred = await navigator.credentials.create({publicKey: opt});
        const credJson = {
          id: cred.id,
          rawId: bufToB64u(cred.rawId),
          type: cred.type,
          response: {
            attestationObject: bufToB64u(cred.response.attestationObject),
            clientDataJSON: bufToB64u(cred.response.clientDataJSON),
          }
        };
        const fin = await fetch('/api/passkey/register/finish', {method: 'POST', headers: J, body: JSON.stringify(credJson)});
        if (!fin.ok) {
          const e = await fin.json().catch(() => ({}));
          throw new Error(e.error || 'Registration failed');
        }
        statusEl.textContent = 'Passkey added successfully ✓';
      } catch (x) { errEl.textContent = (x && x.message) || 'Registration error'; }
    });
  }

  // -- Vault Settings & Limits: Address & Daily Limit ----------------------
  const vAddrDisplay = g('vault-addr-display');
  const vLimitDisplay = g('vault-limit-display');
  const vSpentDisplay = g('vault-spent-display');
  async function loadVaultSettings() {
    if (!vAddrDisplay) return;
    try {
      const r = await fetch('/api/vault/settings?subject=' + q6);
      if (!r.ok) return;
      const data = await r.json();
      vAddrDisplay.textContent = data.home_address || '(none)';
      vLimitDisplay.textContent = '$' + Number(data.daily_limit_usd || 0).toFixed(2);
      vSpentDisplay.textContent = '$' + Number(data.spent_today_usd || 0).toFixed(2);
    } catch (_) {}
  }

  const addrForm = g('vault-addr-form');
  if (addrForm) {
    addrForm.addEventListener('submit', async e => {
      e.preventDefault();
      const val = g('vault-addr-input').value.trim();
      const errEl = g('vault-addr-err'), stEl = g('vault-addr-status');
      errEl.textContent = ''; stEl.textContent = '';
      if (!val) return;
      try {
        const r = await fetch('/api/vault/settings?subject=' + q6, {
          method: 'POST',
          headers: J,
          body: JSON.stringify({home_address: val})
        });
        if (!r.ok) throw new Error('Could not update address');
        stEl.textContent = 'Delivery address updated ✓';
        g('vault-addr-input').value = '';
        await loadVaultSettings();
      } catch (x) { errEl.textContent = (x && x.message) || 'Error updating address'; }
    });
  }

  const limitForm = g('vault-limit-form');
  if (limitForm) {
    limitForm.addEventListener('submit', async e => {
      e.preventDefault();
      const val = parseFloat(g('vault-limit-input').value);
      const errEl = g('vault-limit-err'), stEl = g('vault-limit-status');
      errEl.textContent = ''; stEl.textContent = '';
      if (isNaN(val) || val <= 0) return;
      try {
        const r = await fetch('/api/vault/settings?subject=' + q6, {
          method: 'POST',
          headers: J,
          body: JSON.stringify({daily_limit_cents: Math.round(val * 100)})
        });
        if (!r.ok) throw new Error('Could not update daily limit');
        stEl.textContent = 'Daily limit updated ✓';
        g('vault-limit-input').value = '';
        await loadVaultSettings();
      } catch (x) { errEl.textContent = (x && x.message) || 'Error updating limit'; }
    });
  }

  loadVaultSettings();
  loadRules();
})();
"#;
