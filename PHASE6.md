# PHASE 6 — judge fixes

Read SPEC.md first. New shared types are already in crates/aegis-core/src/lib.rs:
`Advice`, `Advocate`, `RuleDecision`, `PolicyRule`, `PolicyEngine`, `AgentIdentity`,
`AgentVerifier`, `Event::ConsentAdvice`, `Event::RuleApplied`, `ConsentDecision.remember`.

## 1. aegis-advocate: an agent that works for the person
- `RulesAdvocate::new()` implements `Advocate` with no network.
  - It classifies the purpose (travel, lease/credit, scheduling, health/insurance, shopping/payment, unknown).
  - It decides which requested fields that purpose actually needs.
  - It recommends the most protective mode that still serves the purpose: `CLEAN_ROOM` for money questions, `REDACT` for calendar, `None` (deny) for identifiers or medical data the purpose doesn't need, and for `export_contacts`.
  - `summary` is 1–2 plain sentences to the person. `red_flags` has one entry per field that isn't needed. `source` = "rules".
- `LlmAdvocate::new(LlmClient)` implements `Advocate`.
  - It asks the local model for JSON `{recommend, summary, red_flags}` with a 10s timeout.
  - The prompt gets request metadata only (agent, tool, purpose, field names), never vault values.
  - The final advice is the more protective of the model's and `RulesAdvocate`'s, in the order DENY > CLEAN_ROOM > REDACT > ALLOW. `source` = "local-9b", falling back to "rules" on any error.

## 2. aegis-policy: standing rules
- `StoredPolicy::new(Option<aegis_store::SupabaseStore>)` implements `PolicyEngine`.
  - It keeps rules in memory per subject and writes them through to Supabase table `aegis_policies(rule_id pk, subject, rule jsonb)`.
  - `async fn load(&self)` restores rules at boot.
- **Matching:** a rule applies only if all of these hold:
  - the subject matches;
  - `tool`, `resource` and `agent_id` match when they're set;
  - the requested fields are a subset of `fields_within`;
  - for `max_amount_cents`, the amount is set and at or below the limit.
- **Precedence:** Deny > Ask > Allow. Among Allows, the most specific wins.
- Payment rules may only be `Allow{CLEAN_ROOM}`.
- `pub fn rule_from_remember(call, spec, requested_fields, approve, mode, scope, amount_cents) -> PolicyRule` builds the rule for "Remember this decision".

## 3. Agent identity (aegis-broker, `src/resolve.rs`)
- `POST /admin/agent-credentials/resolve {"credential"}` (admin token) returns `AgentIdentity` when the credential is a valid `provider:"mcp"` fake key (by sha256: not revoked, not expired). It records the use. Otherwise it returns 404.
- `mcp` keys don't need a stored provider secret.
- `pub fn resolve_router(state: BrokerState) -> Router`, merged into `router()`.

## 4. aegis-passkey: real WebAuthn passkeys
- `PasskeyService::new(rp_id: String, origin: String, store: Option<SupabaseStore>)`, stored in table `aegis_passkeys(credential_id pk, subject, public_key_cose_b64, sign_count, created_at)`.
- Registration, which requires an existing session (the orchestrator wires the routes):
  - `begin_registration(subject) -> Value` returns `PublicKeyCredentialCreationOptions` JSON: a random challenge, rp `{id, name:"AegisMCP"}`, user `{id, name, displayName}`, ES256 (-7), attestation "none", residentKey "preferred".
  - `finish_registration(subject, credential_json) -> Result<()>`: check that `clientDataJSON` has type `webauthn.create`, the challenge and the origin; parse the CBOR `attestationObject`, read `authData`, check the rpIdHash and the user-present flag, extract the COSE EC2 P-256 key, and store it.
- Login:
  - `begin_login() -> Value` returns request options with a challenge and empty `allowCredentials` (discoverable).
  - `finish_login(credential_json) -> Result<String /*subject*/>`: check the type `webauthn.get`, challenge, origin, rpIdHash and UP flag; verify the ES256 signature over `authData || sha256(clientDataJSON)` with p256; check that the sign count increases, then update it.
- Challenges are single-use and expire after 5 minutes.
- Tests: build authenticator responses in-test with a p256 `SigningKey`, covering a valid round trip, wrong origin, wrong challenge, a replayed challenge, and a bad signature.

## 5. Dashboard (`crates/aegis-dashboard/src/p6/`)
- `pub fn cards(subject) -> Markup` adds:
  - a "My rules" card: list, delete, quick-add presets ("Calendar: free/busy only for any agent", "Never share health data", "Payments under $500: clean room");
  - a "Connect a real agent" card: issue an `mcp` credential through `POST /api/keys/fake {agent_id, provider:"mcp"}`, then show the ready-to-paste `claude mcp add --transport http aegis http://127.0.0.1:8787/mcp --header "Authorization: Bearer <credential>"`;
  - a "Passkeys" card: "Add a passkey" through `navigator.credentials.create`.
- `pub const P6_JS`:
  - `window.renderAdvice(request_id, advice)` fills `#advice-<request_id>` on the consent card (recommendation pill, summary, red flags);
  - `window.rememberChoice(request_id)` returns the "Remember this decision" scope or null;
  - rules CRUD against `GET/POST /api/rules` and `DELETE /api/rules/{id}`;
  - the passkey flows against `/api/passkey/register/{begin,finish}` and `/api/passkey/login/{begin,finish}`, with base64url encode/decode of the binary fields.
- `pub fn passkey_login_button() -> Markup` plus its JS for /login: "Sign in with a passkey".
- Escape every dynamic string.
