//! AegisMCP dashboard: people index (`GET /`), login (`GET /login`) and the
//! per-person live consent console (`GET /u/{subject}`). Server-rendered with
//! maud; one inline vanilla-JS `<script>` per page, no build step.

mod p5;

use aegis_core::{demo_vaults, Person};
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use maud::{html, Markup, PreEscaped, DOCTYPE};

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/login", get(login))
        .route("/u/{subject}", get(person_page))
}

fn people() -> Vec<Person> {
    demo_vaults().iter().map(|v| v.person()).collect()
}

/// Floating pill header shared by every page.
fn pill_header(right: Markup) -> Markup {
    html! {
        header.pill {
            a.mark href="/" {
                canvas.globe-s width="28" height="28" data-globe="1" {}
                span.word { "AegisMCP" }
            }
            nav { a href="/" { "People" } a href="/login" { "Login" } }
            div.right { (right) }
        }
    }
}

async fn index() -> Markup {
    let people = people();
    layout(
        "AegisMCP — Consent Firewall for Personal AI Agents",
        html! {
            (pill_header(html! {
                a.cta href="/login" { "Log in" span.arrow { "↘" } }
            }))
            main.wrap {
                section.hero {
                    div.hero-text {
                        div.eyebrow { "Consent firewall · MCP" }
                        h1 { "Lease your " span.acc { "context" } ". Never give it " span.acc { "away" } "." }
                        p.lede { "AegisMCP — Consent Firewall for Personal AI Agents. Untrusted agents never touch your raw vault: every request is intercepted, consented, clean-roomed and signed." }
                    }
                    canvas.globe-l width="420" height="420" data-globe="1" {}
                }
                div.eyebrow { "Choose a person" }
                div.cards {
                    @for p in &people {
                        a.card href=(format!("/u/{}", p.subject)) {
                            div.avatar { (p.display_name.chars().next().unwrap_or('?')) }
                            div.cname { (p.display_name) }
                            div.csub { "subject · " (p.subject) }
                            div.go { "Open dashboard " span.arrow { "↘" } }
                        }
                    }
                }
            }
            script { (PreEscaped(GLOBE_JS)) }
        },
    )
}

async fn login() -> Markup {
    let people = people();
    layout(
        "AegisMCP — Log in",
        html! {
            (pill_header(html! {}))
            main.wrap.narrow {
                div.eyebrow { "Secure access" }
                h1.mid { "Log in to your " span.acc { "firewall" } }
                form #loginform.card.form {
                    label { "Person" }
                    select #who name="subject" {
                        @for p in &people { option value=(p.subject) { (p.display_name) } }
                    }
                    label { "Password" }
                    input #pw type="password" name="password" placeholder="password" autocomplete="current-password";
                    button.primary type="submit" { "Log in" span.arrow { "↘" } }
                    div #err.err {}
                    p.hint { "Demo password: " code { "<subject>-demo" } }
                }
                div.card.form.web3 {
                    label { "Or use a wallet" }
                    button.primary #siwe type="button" { "Sign in with Ethereum" span.arrow { "↘" } }
                    div #siwe-err.err {}
                    p #nowallet.hint style="display:none" { "No wallet detected — install MetaMask or another EIP-1193 wallet." }
                }
            }
            script { (PreEscaped(SIWE_JS)) }
            script { (PreEscaped(LOGIN_JS)) }
            script { (PreEscaped(GLOBE_JS)) }
        },
    )
}

async fn person_page(Path(subject): Path<String>) -> Response {
    let people = people();
    let Some(me) = people.iter().find(|p| p.subject == subject) else {
        return (StatusCode::NOT_FOUND, "unknown person").into_response();
    };
    // JSON-encode, then neutralise `</script>` openers.
    let subj_js = serde_json::to_string(&me.subject)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('<', "\\u003c");
    let page = layout(
        &format!("AegisMCP — {}", me.display_name),
        html! {
            (pill_header(html! {
                span.lbl { "Protecting" }
                span.name { (me.display_name) }
                select #switcher onchange="location.href='/u/'+this.value" aria-label="Switch person" {
                    @for p in &people {
                        option value=(p.subject) selected[p.subject == me.subject] { (p.display_name) }
                    }
                }
                a.sec.rtlink href=(format!("/u/{}/redteam", me.subject)) { "Red team" }
                button.sec onclick="logout()" { "Log out" }
            }))
            main.dash {
                div.bar {
                    div {
                        div.eyebrow { "Dashboard" }
                        h1.mid { (me.display_name) "'s " span.acc { "firewall" } }
                    }
                    div.bar-r {
                        div.scen {
                            button.sec onclick="scenario(1)" { "1 Travel broker" }
                            button.sec onclick="scenario(2)" { "2 Prompt injection" }
                            button.sec onclick="scenario(3)" { "3 Lease revocation" }
                        }
                        button.kill onclick="kill()" { "KILL SWITCH — revoke all my leases" }
                    }
                }
                div.card.cred {
                    div.cred-h { div.eyebrow { "Your MCP credential" } span.dim { "agents send " code { "Authorization: Bearer <token>" } } }
                    div.cred-row {
                        code #token { "loading…" }
                        button.sec onclick="copyToken()" #copybtn { "Copy" }
                    }
                    div.cred-row.wallet {
                        span.lbl { "Wallet" }
                        span #wallet { span.dim { "loading…" } }
                        button.sec #linkwallet type="button" onclick="linkWallet()" style="display:none" { "Link wallet" }
                        span #wallet-err.err {}
                    }
                }
                div.cols {
                    section.col {
                        div.eyebrow { "Agent" }
                        h2 { "Untrusted Agent" }
                        div #agent.term { div.dim2 { "$ waiting for agent activity… run a scenario above" } }
                    }
                    section.col {
                        div.eyebrow { "Firewall" }
                        h2 { "Aegis Firewall" }
                        div #alerts {}
                        div #consents {}
                        div #releases {}
                        div #fw-empty.empty { span.pill-ok { "ARMED" } " No pending consent." }
                    }
                    section.col {
                        div.eyebrow { "Ledger" }
                        h2 { "Telemetry & Consent Ledger" }
                        div.metrics {
                            div.m.inv { div.v #m-pct { "0%" } div.k { "PII blocked" } }
                            div.m { div.v #m-leases { "0" } div.k { "Active leases" } }
                            div.m { div.v.redtxt #m-viol { "0" } div.k { "Violations" } }
                            div.m { div.v #m-bytes { "0 B" } div.k { "Bytes withheld" } }
                        }
                        h3 { "Active receipts" }
                        div #receipts { div.dim { "none" } }
                        div #receipt-view {}
                        h3 { "Recent ledger" }
                        div #ledger {}
                        div.dim.pk #pk {}
                        div #chain-anchor.anchor {}
                    }
                }
            }
            script { (PreEscaped(SIWE_JS)) }
            script { (PreEscaped(format!("const SUBJECT = {subj_js};\n{JS}"))) }
            script { (PreEscaped(GLOBE_JS)) }
        },
    );
    page.into_response()
}

fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="preconnect" href="https://fonts.googleapis.com";
                link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter+Tight:wght@600;700&family=Inter:wght@400;500;600&family=JetBrains+Mono:wght@400;600&display=swap";
                style { (PreEscaped(CSS)) }
            }
            body { (body) }
        }
    }
}

const CSS: &str = r#"
:root{--bg:#EFEFEF;--card:#FFFFFF;--line:#E4E4E4;--blue:#0000FF;--blue-h:#0000CC;--soft:#C8C8FF;--wash:#EEEEFF;
--ink:#0A0A0A;--ink2:#5C5C5C;--ink3:#8A8A8A;--blk:#050505;--red:#E5002B;--redw:#FFE9EC;--amb:#B25E00;--ambw:#FFF2DE;
--grn:#007A3D;--grnw:#E3F7EC;
--disp:'Inter Tight',Inter,system-ui,sans-serif;--body:Inter,system-ui,-apple-system,sans-serif;
--mono:'JetBrains Mono',ui-monospace,SFMono-Regular,Menlo,monospace}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 var(--body)}
a{color:inherit;text-decoration:none}
code,pre{font-family:var(--mono)}
.pill{display:flex;align-items:center;gap:20px;background:var(--card);border:1px solid var(--line);border-radius:8px;
margin:16px auto;max-width:1480px;width:calc(100% - 32px);padding:8px 12px 8px 16px;position:sticky;top:12px;z-index:10}
.mark{display:flex;align-items:center;gap:10px}.word{font:700 18px var(--disp);letter-spacing:-.04em}
.pill nav{display:flex;gap:16px;color:var(--ink2);font-size:14px}.pill nav a:hover{color:var(--blue)}
.right{margin-left:auto;display:flex;align-items:center;gap:10px}
.lbl{font:600 11px var(--mono);color:var(--ink3);text-transform:uppercase;letter-spacing:.1em}
.name{font:600 15px var(--disp)}
select,input{font:14px var(--body);border:1px solid var(--line);border-radius:4px;padding:8px 10px;background:#fff;color:var(--ink)}
button{font:600 13px var(--body);cursor:pointer;border-radius:4px;border:1px solid var(--line);background:#fff;color:var(--ink);padding:10px 14px}
button.sec:hover{border-color:var(--blue);color:var(--blue)}
.cta,button.primary{display:inline-flex;align-items:center;gap:12px;background:var(--blue);color:#fff;border:0;border-radius:4px;
padding:0 0 0 18px;height:44px;font:600 14px var(--body)}
.cta:hover,button.primary:hover{background:var(--blue-h)}
.arrow{display:inline-flex;align-items:center;justify-content:center;width:44px;height:44px;background:#fff;color:var(--blue);
border:1px solid var(--blue);border-radius:4px;font-size:18px;margin-left:4px}
.go .arrow{width:28px;height:28px;font-size:14px}
.wrap{max-width:1100px;margin:0 auto;padding:30px 20px 80px}.wrap.narrow{max-width:520px}
.eyebrow{font:600 11px var(--mono);color:var(--blue);text-transform:uppercase;letter-spacing:.14em;margin-bottom:8px}
h1{font:700 clamp(44px,7vw,96px)/.95 var(--disp);letter-spacing:-.045em;margin:0 0 20px}
h1.mid{font-size:clamp(34px,4.5vw,56px);margin-bottom:12px}
.acc{color:var(--blue)}
h2{font:700 22px/1 var(--disp);letter-spacing:-.03em;margin:0 0 14px}
h3{font:600 11px var(--mono);text-transform:uppercase;letter-spacing:.12em;color:var(--ink3);margin:22px 0 8px}
.hero{display:flex;align-items:center;gap:20px;justify-content:space-between;margin-bottom:40px}
.hero-text{flex:1}.lede{color:var(--ink2);font-size:17px;max-width:560px}
.globe-l{width:min(420px,40vw);height:auto;aspect-ratio:1}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:16px}
.card{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:22px}
a.card:hover{border-color:var(--blue);box-shadow:0 0 0 3px var(--wash)}
.avatar{width:48px;height:48px;border-radius:4px;background:var(--wash);color:var(--blue);display:flex;align-items:center;
justify-content:center;font:700 22px var(--disp);margin-bottom:14px}
.cname{font:700 22px var(--disp);letter-spacing:-.03em}.csub{font:12px var(--mono);color:var(--ink3);margin:2px 0 18px}
.go{display:flex;align-items:center;justify-content:space-between;color:var(--blue);font-weight:600;font-size:14px}
.form{display:flex;flex-direction:column;gap:8px;padding:26px}.form label{font:600 12px var(--mono);color:var(--ink2);text-transform:uppercase;letter-spacing:.08em;margin-top:6px}
.form button{margin-top:14px;align-self:flex-start}.err{color:var(--red);font-weight:600;min-height:20px}.hint{color:var(--ink3);font-size:13px;margin:0}
.dash{max-width:1480px;margin:0 auto;padding:10px 16px 60px}
.bar{display:flex;flex-wrap:wrap;gap:16px;align-items:flex-end;justify-content:space-between;margin-bottom:16px}
.bar-r{display:flex;flex-wrap:wrap;gap:12px;align-items:center}.scen{display:flex;gap:8px;flex-wrap:wrap}
.kill{background:var(--blk);color:#fff;border:0;font:700 14px var(--body);padding:14px 20px;letter-spacing:.02em}
.kill:hover{background:var(--red)}
.cred{margin-bottom:16px;padding:16px 20px}.cred-h{display:flex;justify-content:space-between;align-items:baseline;gap:12px;flex-wrap:wrap}
.cred-row{display:flex;gap:10px;align-items:center;margin-top:6px}
#token{flex:1;background:var(--bg);padding:10px 12px;border-radius:4px;font-size:13px;word-break:break-all}
.cols{display:grid;grid-template-columns:1fr 1.3fr 1fr;gap:16px;align-items:start}
@media(max-width:1100px){.cols{grid-template-columns:1fr}}
.col{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:18px;min-width:0}
.dim,.dim2{font:12px var(--mono);color:var(--ink3)}.dim2{color:#8a8a8a}.pk{margin-top:14px;word-break:break-all}
.term{background:var(--blk);color:#fff;border-radius:6px;padding:14px;font:12px/1.5 var(--mono);height:620px;overflow-y:auto}
.ln{margin:0 0 8px;word-break:break-word}.ln .ts{color:#777}.ln.say{color:#e8e8e8}.ln.tool{color:#ff6b84}.ln.aegis{color:#5dffa5}
pre{margin:0;white-space:pre-wrap;word-break:break-word;font-size:11.5px}
.empty{color:var(--ink2);font-size:14px;padding:6px 0}
.pill-ok,.pill-w,.pill-b,.pill-r{display:inline-block;font:600 11px var(--body);text-transform:uppercase;letter-spacing:.1em;padding:3px 8px;border-radius:3px}
.pill-ok{color:var(--grn);background:var(--grnw)}.pill-w{color:var(--amb);background:var(--ambw)}
.pill-b{color:var(--blue);background:var(--wash)}.pill-r{color:var(--red);background:var(--redw)}
.consent{border:2px solid var(--amb);background:var(--ambw);border-radius:6px;padding:16px;margin-bottom:14px}
.consent h4{margin:0 0 10px;font:700 17px var(--disp);letter-spacing:-.02em;color:var(--amb)}
.kv{font-size:14px;margin:4px 0}.kv b{display:inline-block;min-width:64px;color:var(--ink3);font:600 11px var(--mono);text-transform:uppercase}
.chip{display:inline-block;font:12px var(--mono);padding:2px 7px;margin:2px 4px 2px 0;border-radius:3px;background:#fff;border:1px solid var(--line);color:var(--ink)}
.chip.sens{background:var(--redw);border-color:var(--red);color:var(--red);font-weight:600}
.cd{font:700 26px var(--mono);color:var(--amb);margin:8px 0}
.btns{display:flex;flex-wrap:wrap;gap:8px}
.b-clean{background:var(--blue);color:#fff;border:0}.b-clean:hover{background:var(--blue-h);color:#fff}
.b-red{border-color:var(--blue);color:var(--blue)}.b-raw{background:var(--red);color:#fff;border:0}.b-raw:hover{color:#fff;opacity:.85}
.b-deny{background:var(--blk);color:#fff;border:0}.b-deny:hover{color:#fff;background:#333}
.alert{background:var(--blk);color:#fff;border-left:6px solid var(--red);border-radius:6px;padding:14px;margin-bottom:14px}
.alert b{color:#ff4d6a;letter-spacing:.06em;font-family:var(--mono)}
.alert .chip{background:var(--redw)}
.rel{border:1px solid var(--line);border-radius:6px;padding:12px;margin-bottom:14px}
.rel .hd{font-size:13px;margin-bottom:8px}
.diff{display:grid;grid-template-columns:1fr 1fr;gap:10px}
.pane{border-radius:4px;padding:10px;max-height:270px;overflow:auto}
.pane .t{font:700 10.5px var(--mono);letter-spacing:.1em;margin-bottom:6px}
.pane.raw{background:var(--redw);border:1px solid #f4b3bf}.pane.raw .t{color:var(--red)}.pane.raw pre{color:#8c0019}
.pane.ok{background:var(--grnw);border:1px solid #a6e0c0}.pane.ok .t{color:var(--grn)}.pane.ok pre{color:#04502a}
.saved{margin-top:8px;font:600 12px var(--mono);color:var(--grn)}
.metrics{display:grid;grid-template-columns:1fr 1fr;gap:10px}
.m{background:var(--bg);border-radius:6px;padding:14px;text-align:left}
.m.inv{background:var(--blk);color:#fff}.m.inv .v{color:#fff}.m.inv .k{color:#aaa}
.m .v{font:700 36px/1 var(--disp);letter-spacing:-.04em;color:var(--blue)}.m .v.redtxt{color:var(--red)}
.m .k{margin-top:6px;font:600 11px var(--mono);color:var(--ink3);text-transform:uppercase;letter-spacing:.08em}
.rc{cursor:pointer;border:1px solid var(--line);border-radius:4px;padding:10px;margin-bottom:8px;font-size:13px}
.rc:hover{border-color:var(--blue);background:var(--wash)}.rc .ttl{float:right;font:700 13px var(--mono);color:var(--grn);background:var(--grnw);padding:2px 7px;border-radius:3px}
#receipt-view pre{background:var(--bg);border-radius:4px;padding:10px;max-height:240px;overflow:auto}
.valid{font:700 13px var(--mono);margin:6px 0}.valid.y{color:var(--grn)}.valid.n{color:var(--red)}
.le{font-size:12.5px;border-left:3px solid var(--line);padding:4px 10px;margin-bottom:6px}
.le.released{border-color:var(--grn)}.le.violation{border-color:var(--red)}.le.denied,.le.timed_out{border-color:var(--amb)}
.le .h{font:11px var(--mono);color:var(--ink3)}
.risk{display:inline-flex;flex-wrap:wrap;align-items:center;gap:6px 12px;background:var(--wash);color:var(--blue);border:1px solid var(--soft);border-radius:4px;padding:4px 8px;font:12px var(--mono)}
.risk b{font:700 11px var(--mono);text-transform:uppercase;letter-spacing:.1em}
.risk.blocked{background:var(--redw);color:var(--red);border-color:var(--red)}
.rb{display:inline-flex;align-items:center;gap:6px}.rt{width:56px;height:6px;background:#fff;border:1px solid currentColor;border-radius:1px;overflow:hidden;display:inline-block}
.rt i{display:block;height:100%;background:currentColor}
.amt{font:700 44px/1 var(--disp);letter-spacing:-.045em;color:var(--ink);margin:4px 0 2px}.mer{font:600 18px var(--disp);color:var(--ink2);margin-bottom:10px}
.consent.pay{background:#fff;border-color:var(--blue)}.consent.pay h4{color:var(--blue)}
.paycard{border-color:var(--grn);background:var(--grnw)}.tok{background:var(--blk);color:#5dffa5;padding:2px 6px;border-radius:3px}
button:disabled{opacity:.4;cursor:not-allowed}
.col,.card,.consent,.rel{box-shadow:0 1px 0 rgba(0,0,0,.02)}
.btns button{height:40px}
a.sec{font:600 13px var(--body);border:1px solid var(--line);border-radius:4px;padding:10px 14px;background:#fff}
a.sec:hover{border-color:var(--blue);color:var(--blue)}
.form.web3{margin-top:16px}.cred-row.wallet{margin-top:10px;flex-wrap:wrap}
.achip{display:inline-block;font:12px var(--mono);padding:2px 7px;border-radius:3px;background:var(--wash);color:var(--blue);border:1px solid var(--soft)}
a.achip:hover{border-color:var(--blue)}
.anchor{margin-top:10px;font:12px var(--mono);color:var(--ink3);word-break:break-all}.anchor a{color:var(--blue)}.anchor .ok{color:var(--grn);font-weight:700}
"#;

const GLOBE_JS: &str = r#"
(function(){
  const cvs = document.querySelectorAll('canvas[data-globe]');
  if (!cvs.length) return;
  const still = window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches;
  const land = (lat, lon) => Math.sin(lon * 2.1 + 0.6) + Math.cos(lat * 3.3 - lon * 1.2) + Math.sin(lat * 1.7 + lon * 0.8) > 0.35;
  function draw(c, rot) {
    const g = c.getContext('2d'), W = c.width, N = W > 100 ? 44 : 14, cell = W / N, R = N / 2 - 1;
    g.clearRect(0, 0, W, W);
    for (let y = 0; y < N; y++) for (let x = 0; x < N; x++) {
      const px = (x + 0.5 - N / 2) / R, py = (y + 0.5 - N / 2) / R, r2 = px * px + py * py;
      const ring = Math.abs(Math.hypot(px, py * 2.6) - 1.28) < 0.06 && py > -0.15;
      if (r2 <= 1) {
        const pz = Math.sqrt(1 - r2), lat = Math.asin(-py), lon = Math.atan2(px, pz) + rot;
        const light = 0.45 + 0.55 * (-px * 0.5 - py * 0.6 + pz * 0.6);
        const d = ((x * 7 + y * 13) % 5) / 5;
        const isLand = land(lat, lon);
        if (light * (isLand ? 1.0 : 0.55) + (d - 0.5) * 0.25 < 0.28) continue;
        g.fillStyle = isLand ? '#0000FF' : '#C8C8FF';
        const s = cell * (isLand ? 0.86 : 0.55);
        g.fillRect(x * cell + (cell - s) / 2, y * cell + (cell - s) / 2, s, s);
      } else if (ring) {
        g.fillStyle = '#0000FF'; g.fillRect(x * cell + cell * 0.25, y * cell + cell * 0.25, cell * 0.5, cell * 0.5);
      }
    }
  }
  let t = 0;
  function frame() { t += 0.012; cvs.forEach(c => draw(c, t)); if (!still) requestAnimationFrame(frame); }
  frame();
})();
"#;

const SIWE_JS: &str = r#"
async function siweSign() {
  const eth = window.ethereum;
  if (!eth) throw new Error('No wallet detected');
  const accts = await eth.request({method: 'eth_requestAccounts'});
  const address = accts && accts[0];
  if (!address) throw new Error('No account selected');
  const nres = await fetch('/api/siwe/nonce');
  if (!nres.ok) throw new Error('Could not get nonce');
  const nonce = (await nres.json()).nonce;
  const message = location.host + ' wants you to sign in with your Ethereum account:\n' + address + '\n\n'
    + 'Sign in to AegisMCP — your consent firewall.\n\n'
    + 'URI: ' + location.origin + '\nVersion: 1\nChain ID: 84532\nNonce: ' + nonce
    + '\nIssued At: ' + new Date().toISOString();
  const signature = await eth.request({method: 'personal_sign', params: [message, address]});
  return {message: message, signature: signature};
}
"#;

const LOGIN_JS: &str = r#"
(function(){
  const params = new URLSearchParams(location.search);
  const pre = params.get('subject');
  if (pre) document.getElementById('who').value = pre;
  const sb = document.getElementById('siwe');
  if (!window.ethereum) document.getElementById('nowallet').style.display = '';
  sb.addEventListener('click', async () => {
    const e2 = document.getElementById('siwe-err');
    e2.textContent = '';
    if (!window.ethereum) { document.getElementById('nowallet').style.display = ''; return; }
    try {
      const body = await siweSign();
      const res = await fetch('/api/login/siwe', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(body)});
      if (!res.ok) { e2.textContent = 'Wallet sign-in failed'; return; }
      const j = await res.json().catch(() => ({}));
      const next = params.get('next');
      if (next && next.startsWith('/') && !next.startsWith('//')) location.href = next;
      else if (j.subject) location.href = '/u/' + encodeURIComponent(j.subject);
      else { const me = await (await fetch('/api/me')).json(); location.href = '/u/' + encodeURIComponent(me.subject); }
    } catch (x) { e2.textContent = (x && x.message) || 'Wallet sign-in cancelled'; }
  });
  document.getElementById('loginform').addEventListener('submit', async e => {
    e.preventDefault();
    const err = document.getElementById('err');
    err.textContent = '';
    const subject = document.getElementById('who').value;
    try {
      const res = await fetch('/api/login', {method: 'POST', headers: {'content-type': 'application/json'},
        body: JSON.stringify({subject: subject, password: document.getElementById('pw').value})});
      if (!res.ok) { err.textContent = 'Invalid credentials'; return; }
      const next = params.get('next');
      location.href = (next && next.startsWith('/') && !next.startsWith('//')) ? next : '/u/' + encodeURIComponent(subject);
    } catch (x) { err.textContent = 'Network error'; }
  });
})();
"#;

const JS: &str = r#"
const $ = id => document.getElementById(id);
const esc = s => String(s == null ? '' : s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const q = encodeURIComponent(SUBJECT);
const pending = {};      // request_id -> consent request
let receipts = [];
let token = '';
const hhmm = () => new Date().toTimeString().slice(0, 8);
const pretty = v => esc(JSON.stringify(v, null, 2));
const fmtBytes = n => n >= 1048576 ? (n / 1048576).toFixed(1) + ' MB' : n >= 1024 ? (n / 1024).toFixed(1) + ' KB' : n + ' B';
const EXPLORER_DEFAULT = 'https://sepolia.basescan.org';
let explorer = EXPLORER_DEFAULT;
const isAddr = s => /^0x[0-9a-fA-F]{40}$/.test(String(s || ''));
const shortHex = (s, a, b) => String(s).slice(0, a) + '…' + String(s).slice(-b);
const addrLink = a => '<a class="achip" target="_blank" rel="noopener" href="' + esc(explorer) + '/address/' + esc(a) + '" title="' + esc(a) + '">' + esc(shortHex(a, 6, 4)) + '</a>';
const ag = (id, safe) => isAddr(id) ? addrLink(id) : safe;
const ago = ts => { const d = Math.max(0, nowS() - Number(ts || 0)); return d < 60 ? d + 's ago' : d < 3600 ? Math.floor(d / 60) + 'm ago' : d < 86400 ? Math.floor(d / 3600) + 'h ago' : Math.floor(d / 86400) + 'd ago'; };
const nowS = () => Math.floor(Date.now() / 1000);

function term(cls, html) {
  const t = $('agent');
  const d = document.createElement('div');
  d.className = 'ln ' + cls;
  d.innerHTML = '<span class="ts">[' + hhmm() + ']</span> ' + html;
  t.appendChild(d);
  t.scrollTop = t.scrollHeight;
}

function post(url, body) {
  return fetch(url, {method: 'POST', headers: {'content-type': 'application/json'}, body: body ? JSON.stringify(body) : undefined})
    .catch(e => console.error(url, e));
}
function scenario(n) { post('/api/scenario/' + n + '?subject=' + q); term('aegis', 'launching scenario ' + n + '…'); }
async function kill() { await post('/api/revoke?subject=' + q); loadState(); }
async function logout() { await post('/api/logout'); location.href = '/login'; }
function copyToken() {
  const done = () => { $('copybtn').textContent = 'Copied ✓'; setTimeout(() => $('copybtn').textContent = 'Copy', 1500); };
  if (navigator.clipboard) navigator.clipboard.writeText(token).then(done, () => {});
  else { const r = document.createRange(); r.selectNode($('token')); getSelection().removeAllRanges(); getSelection().addRange(r); done(); }
}

function decide(id, approve, mode) {
  post('/api/consent/' + encodeURIComponent(id), {approve: approve, mode: mode || 'CLEAN_ROOM', ttl_secs: 60});
  delete pending[id];
  renderConsents();
}

const REGULATED = ['ssn', 'account_number', 'routing_number', 'passport_number'];
const risks = {};        // call_id -> RiskScores event
const usd = n => '$' + Number(n || 0).toLocaleString(undefined, {minimumFractionDigits: 2, maximumFractionDigits: 2});

function riskChip(ev) {
  if (!ev) return '';
  const sc = ev.scores || {};
  const bar = (label, v) => {
    const pct = Math.max(0, Math.min(1, Number(v) || 0));
    return '<span class="rb"><span class="rl">' + label + '</span><span class="rt"><i style="width:' + Math.round(pct * 100) + '%"></i></span><span class="rv">' + pct.toFixed(2) + '</span></span>';
  };
  return '<span class="risk' + (ev.blocked ? ' blocked' : '') + '"><b>Jev risk</b>'
    + bar('promptInjection', sc.prompt_injection) + bar('dataExfiltration', sc.data_exfiltration)
    + (ev.blocked ? '<span class="pill-r">Blocked</span>' : '') + '</span>';
}

function renderConsents() {
  const t = nowS();
  for (const id of Object.keys(pending)) if (Number(pending[id].deadline) <= t) delete pending[id];
  const ids = Object.keys(pending);
  $('fw-empty').style.display = ids.length ? 'none' : '';
  $('consents').innerHTML = ids.map(id => {
    const r = pending[id], c = r.call || {};
    const sens = new Set(r.sensitive_fields || []);
    const fields = r.requested_fields || [];
    const chips = fields.map(f => '<span class="chip' + (sens.has(f) ? ' sens' : '') + '">' + esc(f) + '</span>').join('');
    const left = Math.max(0, r.deadline - nowS());
    const eid = esc(id);
    const risk = riskChip(risks[c.call_id]);
    const timer = '<div class="cd" data-deadline="' + Number(r.deadline) + '">auto-deny in ' + left + 's</div>';
    if (c.tool === 'make_payment') {
      const a = c.arguments || {};
      return '<div class="consent pay"><h4><span class="pill-w">Waiting</span> Payment requires approval</h4>'
        + '<div class="amt">' + usd(a.amount_usd) + '</div><div class="mer">to ' + esc(a.merchant) + '</div>'
        + '<div class="kv"><b>Agent</b> ' + ag(c.agent_id, esc(c.agent_id)) + '</div>'
        + '<div class="kv"><b>Purpose</b> ' + esc(c.purpose) + '</div>'
        + (a.description ? '<div class="kv"><b>Memo</b> ' + esc(a.description) + '</div>' : '')
        + (risk ? '<div class="kv">' + risk + '</div>' : '')
        + timer
        + '<div class="btns">'
        + '<button class="b-clean" data-id="' + eid + '" data-a="1" data-m="CLEAN_ROOM">Approve payment</button>'
        + '<button class="b-deny" data-id="' + eid + '" data-a="0" data-m="BLOCK">Deny</button>'
        + '</div></div>';
    }
    const regulated = fields.some(f => REGULATED.includes(f));
    const raw = regulated
      ? '<button class="b-raw" disabled title="raw release disabled for regulated identifiers">Allow RAW 60s</button>'
      : '<button class="b-raw" data-id="' + eid + '" data-a="1" data-m="ALLOW">Allow RAW 60s</button>';
    return '<div class="consent"><h4><span class="pill-w">Waiting</span> Consent required</h4>'
      + '<div class="kv"><b>Agent</b> ' + ag(c.agent_id, esc(c.agent_id)) + '</div>'
      + '<div class="kv"><b>Tool</b> <code>' + esc(c.tool) + '</code> → ' + esc(r.resource) + '</div>'
      + '<div class="kv"><b>Purpose</b> ' + esc(c.purpose) + '</div>'
      + '<div class="kv"><b>Fields</b> ' + (chips || '<span class="dim">(all)</span>') + '</div>'
      + (risk ? '<div class="kv">' + risk + '</div>' : '')
      + timer
      + '<div class="btns">'
      + '<button class="b-clean" data-id="' + eid + '" data-a="1" data-m="CLEAN_ROOM">Approve CLEAN ROOM 60s</button>'
      + '<button class="b-red" data-id="' + eid + '" data-a="1" data-m="REDACT">Approve REDACTED 60s</button>'
      + raw
      + '<button class="b-deny" data-id="' + eid + '" data-a="0" data-m="BLOCK">Deny</button>'
      + '</div></div>';
  }).join('');
}
$('consents').addEventListener('click', e => {
  const b = e.target.closest('button[data-id]');
  if (b) decide(b.dataset.id, b.dataset.a === '1', b.dataset.m);
});

function renderReceipts() {
  const t = nowS();
  const live = receipts.filter(r => !r.revoked && r.expires_at > t);
  $('receipts').innerHTML = live.length ? live.map(r =>
    '<div class="rc" data-id="' + esc(r.receipt_id) + '"><span class="ttl" data-exp="' + Number(r.expires_at) + '">' + (r.expires_at - t) + 's</span>'
    + '<b>' + ag(r.grantee_agent, esc(r.grantee_agent)) + '</b><br><span class="dim">' + esc(r.resource) + ' · ' + esc(r.resolution_mode) + ' · ' + esc(r.receipt_id) + '</span></div>'
  ).join('') : '<div class="dim">none</div>';
}
$('receipts').addEventListener('click', async e => {
  const el = e.target.closest('.rc');
  if (!el) return;
  try {
    const res = await fetch('/api/receipts/' + encodeURIComponent(el.dataset.id));
    if (!res.ok) { $('receipt-view').innerHTML = '<div class="valid n">receipt not found</div>'; return; }
    const j = await res.json();
    $('receipt-view').innerHTML = '<h3>Signed receipt</h3><div class="valid ' + (j.valid ? 'y">valid ✓ Ed25519 signature verified' : 'n">INVALID ✗') + '</div><pre>' + pretty(j.receipt) + '</pre>';
  } catch (err) { $('receipt-view').innerHTML = '<div class="valid n">error: ' + esc(err) + '</div>'; }
});

function renderMetrics(m) {
  if (!m) return;
  $('m-pct').textContent = (Number(m.pii_blocked_pct) || 0).toFixed(0) + '%';
  $('m-leases').textContent = m.active_leases;
  $('m-viol').textContent = m.violations;
  $('m-bytes').textContent = fmtBytes(Math.max(0, (m.bytes_raw || 0) - (m.bytes_released || 0)));
}

function renderLedger(list) {
  $('ledger').innerHTML = (list || []).slice(0, 12).map(e =>
    '<div class="le ' + esc(e.outcome) + '"><b>' + esc(String(e.outcome).toUpperCase()) + '</b> ' + ag(e.agent_id, esc(e.agent_id)) + ' → <code>' + esc(e.tool) + '</code>'
    + ' <span class="dim">[' + esc(e.policy_applied) + ']</span><br><span class="h">#' + esc(String(e.entry_hash || '').slice(0, 10)) + '</span></div>'
  ).join('') || '<div class="dim">empty</div>';
}

async function loadState() {
  try {
    const res = await fetch('/api/state?subject=' + q);
    if (!res.ok) return;
    const s = await res.json();
    renderMetrics(s.metrics);
    receipts = s.active_receipts || [];
    renderReceipts();
    renderLedger(s.recent_ledger);
    const t = nowS();
    for (const k of Object.keys(pending)) delete pending[k];
    for (const r of (s.pending || [])) if (Number(r.deadline) > t) pending[r.request_id] = r;
    renderConsents();
    $('pk').textContent = s.public_key_hex ? 'ed25519 pubkey: ' + s.public_key_hex : '';
  } catch (e) { console.error(e); }
}

function addRelease(ev) {
  const saved = Math.max(0, (ev.bytes_raw || 0) - (ev.bytes_released || 0));
  const d = document.createElement('div');
  d.className = 'rel';
  d.innerHTML = '<div class="hd"><span class="pill-ok">Released</span> ' + ag(ev.agent_id, esc(ev.agent_id)) + ' → <b>' + esc(ev.tool) + '</b> · mode <b>' + esc(ev.mode) + '</b></div>'
    + '<div class="diff"><div class="pane raw"><div class="t">RAW REQUESTED</div><pre>' + pretty(ev.raw) + '</pre></div>'
    + '<div class="pane ok"><div class="t">RELEASED TO AGENT</div><pre>' + pretty(ev.released) + '</pre></div></div>'
    + '<div class="saved">' + fmtBytes(ev.bytes_raw || 0) + ' → ' + fmtBytes(ev.bytes_released || 0) + ' · ' + fmtBytes(saved) + ' withheld</div>';
  $('releases').prepend(d);
  while ($('releases').children.length > 6) $('releases').lastChild.remove();
}

function addPayment(ev) {
  const r = ev.receipt || {};
  const test = r.provider === 'stripe' ? (r.livemode ? 'Stripe LIVE' : 'Stripe TEST') : 'Demo provider';
  const d = document.createElement('div');
  d.className = 'rel paycard';
  d.innerHTML = '<div class="hd"><span class="pill-ok">' + esc(r.status === 'succeeded' ? 'executed' : r.status || 'executed') + '</span> ' + ag(ev.agent_id, esc(ev.agent_id)) + ' → <b>make_payment</b></div>'
    + '<div class="amt">' + usd((r.amount_cents || 0) / 100) + '</div><div class="mer">to ' + esc(r.merchant) + '</div>'
    + '<div class="kv"><b>Provider</b> ' + esc(test) + '</div>'
    + '<div class="kv"><b>Payment</b> <code>' + esc(r.payment_id) + '</code></div>'
    + '<div class="kv"><b>Card</b> <code class="tok">[TOKENIZED BY AEGIS]</code></div>';
  $('releases').prepend(d);
  while ($('releases').children.length > 6) $('releases').lastChild.remove();
  term('aegis', 'payment executed ' + usd((r.amount_cents || 0) / 100) + ' → ' + esc(r.merchant));
}

function onEvent(ev) {
  switch (ev.type) {
    case 'agent_log':
      term(ev.agent_id === 'aegis' ? 'aegis' : 'say', '<b>' + ag(ev.agent_id, esc(ev.agent_id)) + '</b>: ' + esc(ev.text)); break;
    case 'tool_requested': {
      const c = ev.call || {};
      term('tool', '▶ <b>' + ag(c.agent_id, esc(c.agent_id)) + '</b> calls <b>' + esc(c.tool) + '</b> ' + esc(JSON.stringify(c.arguments)));
      break;
    }
    case 'risk_scored':
      risks[ev.call_id] = ev;
      term(ev.blocked ? 'tool' : 'aegis', riskChip(ev) + ' ' + esc(ev.tool));
      renderConsents();
      break;
    case 'payment_executed': addPayment(ev); break;
    case 'consent_required':
      pending[ev.request.request_id] = ev.request; renderConsents(); break;
    case 'consent_resolved':
      delete pending[ev.request_id]; renderConsents(); loadState(); break;
    case 'payload_released': addRelease(ev); break;
    case 'call_denied':
      term('tool', '✖ denied ' + esc(ev.tool) + ': ' + esc(ev.reason)); break;
    case 'policy_violation': {
      const a = document.createElement('div');
      a.className = 'alert';
      a.innerHTML = '<span class="pill-r">Blocked</span> <b>POLICY VIOLATION</b><br>' + ag(ev.agent_id, esc(ev.agent_id)) + ' → ' + esc(ev.tool) + '<br>' + esc(ev.reason)
        + '<br>' + (ev.offending || []).map(o => '<span class="chip sens">' + esc(o) + '</span>').join('');
      $('alerts').prepend(a);
      while ($('alerts').children.length > 3) $('alerts').lastChild.remove();
      term('tool', '🚨 violation: ' + esc(ev.reason));
      loadState();
      break;
    }
    case 'lease_revoked':
      term('aegis', 'leases revoked: ' + esc((ev.receipt_ids || []).join(', ') || 'none')); loadState(); break;
    case 'metrics': renderMetrics(ev.metrics); loadState(); break;
  }
}

function connect() {
  const es = new EventSource('/events?subject=' + q);
  es.onmessage = m => { try { onEvent(JSON.parse(m.data)); } catch (e) { console.error(e); } };
}

setInterval(() => {
  const t = nowS();
  document.querySelectorAll('.cd').forEach(el => {
    el.textContent = 'auto-deny in ' + Math.max(0, Number(el.dataset.deadline) - t) + 's';
  });
  let expired = false;
  document.querySelectorAll('.ttl').forEach(el => {
    const left = Number(el.dataset.exp) - t;
    el.textContent = Math.max(0, left) + 's';
    if (left <= 0) expired = true;
  });
  if (expired) renderReceipts();
  // The snapshot is authoritative: clears consent cards whose waiter went away.
  if (++stateTick % 5 === 0) loadState();
}, 1000);
let stateTick = 0;

function renderWallet(w) {
  $('wallet').innerHTML = w ? addrLink(w) : '<span class="dim">none linked</span>';
  $('linkwallet').style.display = w ? 'none' : '';
}
async function linkWallet() {
  $('wallet-err').textContent = '';
  if (!window.ethereum) { $('wallet-err').textContent = 'No wallet detected'; return; }
  try {
    const res = await fetch('/api/wallet/link', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(await siweSign())});
    if (!res.ok) { $('wallet-err').textContent = 'Link failed'; return; }
    renderWallet((await res.json()).address);
  } catch (e) { $('wallet-err').textContent = (e && e.message) || 'Link cancelled'; }
}

let chainEnabled = null, myAnchor = null;
let sbAnchor = '';
function renderAnchor() {
  const el = $('chain-anchor');
  const sb = sbAnchor ? '<div>' + sbAnchor + '</div>' : '';
  if (chainEnabled === false) { el.innerHTML = sb + '<span class="dim">on-chain anchoring off</span>'; return; }
  if (!myAnchor) { el.innerHTML = sb + (chainEnabled ? '<span class="dim">awaiting on-chain anchor…</span>' : ''); return; }
  el.innerHTML = sb + '<span class="ok">Anchored on Base Sepolia ✓</span> <a target="_blank" rel="noopener" href="' + esc(myAnchor.explorer_url) + '">'
    + esc(shortHex(myAnchor.tx_hash, 8, 6)) + '</a> · ' + esc(ago(myAnchor.anchored_at));
}
async function loadChain() {
  try {
    const sr = await fetch('/api/anchors');
    if (sr.ok) { const sj = await sr.json(); const x = (sj.anchors || sj)[SUBJECT]; if (x) sbAnchor = 'Supabase anchor ✓ <span class="dim">' + esc(shortHex(x.head_hash || x.object || JSON.stringify(x), 10, 6)) + '</span>'; }
  } catch (e) { console.error(e); }
  try {
    const info = await (await fetch('/api/chain/info')).json();
    chainEnabled = !!info.enabled;
    if (info.explorer) explorer = info.explorer;
    if (chainEnabled) {
      const a = await (await fetch('/api/chain/anchors')).json();
      myAnchor = (a.anchors || {})[SUBJECT] || null;
    }
  } catch (e) { console.error(e); }
  renderAnchor();
  renderReceipts();
}
setInterval(loadChain, 15000);

(async function init() {
  try {
    const res = await fetch('/api/me');
    if (res.status === 401) { location.href = '/login?next=' + encodeURIComponent(location.pathname); return; }
    const me = await res.json();
    if (me.subject && me.subject !== SUBJECT) { location.href = '/u/' + encodeURIComponent(me.subject); return; }
    token = me.mcp_token || '';
    $('token').textContent = token || '(none)';
    renderWallet(me.wallet || null);
  } catch (e) { console.error(e); }
  await loadChain();
  loadState();
  connect();
})();
"#;

#[cfg(test)]
mod web3_tests;
