# AegisMCP: Consent Firewall & Data Clean Room for Agentic AI

> **Lease your context. Never give it away.**

---

## 1. The 20-Second Pitch Hook

In Web 2.0, users surrendered their personal data to ad networks, data brokers, and trackers in exchange for free services. By the time consent rails like Ketch and GDPR were introduced, personal data was already copied, duplicated, and irrevocably stored across third-party databases.

The **Agentic Web** is repeating this exact mistake at ten times the velocity. Today's AI agents with MCP (Model Context Protocol) ambiently vacuum raw user vaults—bank statements, SSNs, medical diagnoses, private calendars, and API credentials—straight into LLM prompts and cloud context windows. Once sensitive data enters an agent's context, it is gone forever.

**AegisMCP** fixes this at the protocol level. Sitting as a consent firewall and data clean room between untrusted AI agents and personal data vaults, AegisMCP ensures **context is leased, never given away**. Agents run privacy-preserving clean room queries that return bounded, zero-knowledge answers (e.g., *"Affordability verified: true"* instead of sending raw account balances and SSNs), backed by cryptographic Ed25519 receipts, instant kill-switch revocation, and tamper-evident audit ledgers.

---

## 2. End-to-End System Architecture

```
+-----------------------------------------------------------------------------------------------------------------+
|                                                    AEGISMCP                                                     |
|                                                                                                                 |
|  +--------------------+          +-----------------------------+          +----------------------------------+  |
|  |  Untrusted Agent   |          |      Cloudflare Tunnel      |          |         Base Sepolia             |  |
|  |  (Claude Code,     |          |   (Local GPU Box / Ollama)  |          |      (Chain ID: 84532)           |  |
|  |   Cursor, Codex)   |          |    Abliterated 9B Model     |          |   Ledger Head State Anchoring    |  |
|  +---------+----------+          +--------------+--------------+          +-----------------+----------------+  |
|            |                                    | (Purpose Judge &                          ^                   |
|            | HTTP POST /mcp                     |  Agent Narration)                         |                   |
|            v                                    |                                           | Calldata:         |
|  +---------+----------+                         |                                           | AEGIS1:hash:hash  |
|  |   /mcp Endpoint    |                         |                                           |                   |
|  | Bearer Auth &      |                         |                                           |                   |
|  | Multi-Tenant Router|                         |                                           |                   |
|  +---------+----------+                         |                                           |                   |
|            |                                    |                                           |                   |
|            v                                    |                                           |                   |
|  +---------+----------+                         |                                           |                   |
|  | 1. Schema Guard    |                         |                                           |                   |
|  |    & Keyword Check |                         |                                           |                   |
|  +---------+----------+                         |                                           |                   |
|            |                                    |                                           |                   |
|            v                                    |                                           |                   |
|  +---------+----------+                         |                                           |                   |
|  | 2. Jev Risk Scorer |                         |                                           |                   |
|  |  (via Fake Key)    |                         |                                           |                   |
|  +---------+----------+                         |                                           |                   |
|            |                                    |                                           |                   |
|            v                                    |                                           |                   |
|  +---------+----------+              +----------v----------+                                |                   |
|  | 3. Consent Queue & |<------------>| Real-Time Dashboard |                                |                   |
|  |    Active Leases   |              | (SIWE / Approval)   |                                |                   |
|  +---------+----------+              +----------+----------+                                |                   |
|            |                                    ^                                           |                   |
|            v                                    |                                           |                   |
|  +---------+----------+              +----------+----------+     AES-256-GCM Vault          |                   |
|  | 4. Clean Room &    |<-------------+ Multi-Tenant Vaults |<==========================+    |                   |
|  |    PII Redactor    |              | (Maya Okafor, Dev)  |                           |    |                   |
|  +---------+----------+              +---------------------+                           |    |                   |
|            |                                                                           |    |                   |
|            v                                                                           |    |                   |
|  +---------+----------+              +---------------------+                           |    |                   |
|  | 5. Payment Provider|<------------>| :8788 aegis-broker  |                           |    |                   |
|  |  (via Fake Key)    |              | (Key-swapping proxy)|                           |    |                   |
|  +---------+----------+              +----------+----------+                           |    |                   |
|            |                                    |                                      |    |                   |
|            v                                    v                                      |    |                   |
|  +---------+----------+              +---------------------+       Async Mirror        v    |                   |
|  | 6. Ed25519 Signer  |              | Supabase Postgres   |<------------------------+ +----+                   |
|  |    & Receipts      |              | (System of Record)  |                                                    |
|  +---------+----------+              +---------------------+                                                    |
|            |                                                                                                    |
|            v                                                                                                    |
|  +---------+----------+              +---------------------+                                                    |
|  | 7. Hash-Chained    |------------->| SSE Event Stream    |                                                    |
|  |    SQLite Ledger   |              | (/events?subject=X) |                                                    |
|  +--------------------+              +---------------------+                                                    |
+-----------------------------------------------------------------------------------------------------------------+
```

---

## 3. Fake Keys & Aegis Broker

### Architecture & Isolation Boundary

A core security invariant of AegisMCP is that **neither AI models, untrusted agents, nor the main gateway process ever hold real provider credentials**.

All agent and gateway traffic utilizes ephemeral, bounded **fake keys** (`aegis_fk_<provider>_<24_alnum>`). A separate, isolated service—`aegis-broker` running on `127.0.0.1:8788`—is the **only** process with access to real upstream credentials. Real keys are encrypted at rest with AES-256-GCM in Supabase (`aegis_secrets`), and the 32-byte master key (`AEGIS_MASTER_KEY`) resides exclusively on the broker host.

```
+-------------------------------------------------------------------------------------------------------+
|                                 FAKE KEY ARCHITECTURE & BROKER PROXY                                  |
|                                                                                                       |
|  +--------------------+                                                                               |
|  |   Model / Agent    |  Holds ONLY ephemeral fake keys:                                              |
|  |  (Untrusted Code)  |  "aegis_fk_stripe_..." / "aegis_fk_jev_..." / "aegis_fk_llm_..."               |
|  +---------+----------+                                                                               |
|            |                                                                                          |
|            | 1. MCP tool calls                                                                        |
|            v                                                                                          |
|  +---------+----------+                                                                               |
|  | :8787 AegisMCP     |  Consent firewall & policy engine.                                            |
|  | (Main Gateway)     |  HOLDS NO REAL PROVIDER KEYS.                                                 |
|  +---------+----------+                                                                               |
|            |                                                                                          |
|            | 2. Proxies upstream calls using fake key                                                 |
|            v                                                                                          |
|  +---------+----------+          +-------------------------------+          +----------------------+  |
|  | :8788 aegis-broker |<-------->| Supabase System of Record     |          | Real Upstream APIs   |  |
|  | (Isolated Service) |          | (aegis_secrets,               |          |                      |  |
|  |                    |          |  aegis_fake_keys,             |          | - api.stripe.com     |  |
|  | ONLY process with  |          |  AES-256-GCM sealed)          |          | - api.typesafe.ai    |  |
|  | real keys!         |          +-------------------------------+          | - Upstream LLM / GPU |  |
|  | Master key local   |----------------- 3. Swaps fake -> real key -------->|                      |  |
|  | (AEGIS_MASTER_KEY) |<---------------- 4. Scrubs real secrets in resp ----|                      |  |
|  +--------------------+                                                     +----------------------+  |
+-------------------------------------------------------------------------------------------------------+
```

### Key-Swapping Proxy Mechanisms
1. **Zero Raw Key Storage**: Only the SHA-256 hash of each fake key (`sha256(fake_key)`) is recorded in Supabase (`aegis_fake_keys`). The full fake key string is displayed once upon creation and never echoed again.
2. **Dynamic Resolution**: At call time, `aegis-broker` resolves the fake key hash against Supabase, verifies that it is active and not expired, selects the matching AES-256-GCM sealed secret, and decrypts the real credential in memory.
3. **Response Sanitization**: All outbound response bodies and headers returned through the proxy are scrubbed. Any accidental leakage of the upstream real key is sanitized and replaced with `[AEGIS-SCRUBBED]`.
4. **Immediate Revocation**: Fake keys can be revoked individually or globally. Revoked fake keys fail immediately with HTTP 401 `{"error":"invalid aegis key"}`.

### The Live Demo Beat: Proving Fake Keys Are Worthless Outside Aegis

1. **Issue Fake Key**: On `/u/maya`, open the **Keys** card, enter agent `curl_agent`, provider `stripe`, TTL `1 hour`, and click **Issue fake key**. Copy the generated key:
   ```sh
   export FAKE_KEY="aegis_fk_stripe_4f9c2e7a1b8d3f6e5a0b1c2d"
   ```
2. **Probe 1: Direct to Stripe API (Fails 401)**:
   Attempt to call the real Stripe API directly using the fake key:
   ```sh
   curl -s -i https://api.stripe.com/v1/customers \
     -u "${FAKE_KEY}:"
   ```
   **Result**: `HTTP/1.1 401 Unauthorized` with Stripe error `"Invalid API Key provided"`. The key is completely worthless outside Aegis.
3. **Probe 2: Via Aegis Broker Proxy (Succeeds 200)**:
   Route the identical fake key through the broker proxy:
   ```sh
   curl -s -i http://127.0.0.1:8788/proxy/stripe/v1/customers \
     -u "${FAKE_KEY}:"
   ```
   **Result**: `HTTP/1.1 200 OK`. The broker validates the fake key, swaps it for Maya's sealed test secret (or the platform test secret), calls Stripe, logs usage to Supabase, scrubs any real credential leaks, and returns the response.
4. **Probe 3: Revoke Individual Key (Dies Immediately)**:
   Click **Revoke** on the Keys card (or send `POST http://127.0.0.1:8787/api/keys/revoke {"prefix":"..."}`). Re-run the broker proxy call:
   ```sh
   curl -s -i http://127.0.0.1:8788/proxy/stripe/v1/customers \
     -u "${FAKE_KEY}:"
   ```
   **Result**: `HTTP/1.1 401 Unauthorized` with `{"error":"invalid aegis key"}`. The key is dead.
5. **Probe 4: Kill Switch Revocation**:
   Triggering the top-bar **Kill Switch** revokes all active leases AND revokes all issued fake keys in a single atomic action (`POST /api/keys/revoke {"prefix":null}`).

---

## 4. Supabase: System of Record

AegisMCP uses Supabase Postgres as its persistent system of record. Local SQLite (`aegis.db`) serves the hot path for millisecond-latency request handling, while a non-blocking asynchronous writer continually mirrors state to Supabase.

### Setup: Run Once
Paste and execute [`supabase/schema.sql`](file:///Users/manubaba/Documents/aegis-mcp/supabase/schema.sql) once in the Supabase SQL Editor.
- Row Level Security (RLS) is enabled across all tables.
- No permissions are granted to `anon` or `authenticated` roles—the database is strictly accessible via backend service-role credentials (`SUPABASE_SECRET_KEY`).

### Table Schema Breakdown

| Table | Primary Key | Description & Security Invariant |
|---|---|---|
| `aegis_people` | `subject` | Multi-tenant user records with `display_name` and `max_payment_cents`. Stores `mcp_token_sha256` (SHA-256 hash only); raw MCP bearer tokens are never persisted in the database. |
| `aegis_wallets` | `address` | Checksummed Ethereum addresses linked 1:1 to subjects (`kind: 'user' \| 'demo'`). Backs SIWE authentication and prevents wallet hijacking. |
| `aegis_secrets` | `secret_id` | Provider credentials (`stripe`, `jev`, `llm`) and personal vaults (`__vault`) sealed with AES-256-GCM. Stores `nonce_b64`, `ciphertext_b64`, and public metadata `meta` (`label`, `last4`, `created_at`). Raw values never touch Supabase; `AEGIS_MASTER_KEY` lives exclusively on the broker host. |
| `aegis_fake_keys` | `fake_key_sha256` | Ephemeral fake key registry indexed by SHA-256 hash (raw fake keys are never stored). Tracks `subject`, `agent_id`, `provider`, `expires_at`, `revoked`, and usage telemetry (`prefix`, `uses`, `last_used_at`). |
| `aegis_key_usage` | `id` (identity) | Granular audit log of calls routed through the broker proxy (`fake_key_sha256`, `subject`, `agent_id`, `provider`, `path`, `ok`, timestamp). |
| `aegis_receipts` | `receipt_id` | Cryptographic Ed25519-signed authorization receipts (`receipt_id`, `subject`, `grantee_agent`, `resource`, `expires_at`, `revoked`, `receipt` JSON). |
| `aegis_ledger` | `entry_hash` | Hash-chained audit ledger mirror (`entry_hash`, `seq`, `subject`, `entry` JSON, timestamp). **The ledger mirror stores only payload hashes, outcome statuses, and claims—never raw personal data.** |

---

## 5. Three-Minute Click-by-Click Stage Script

### Setup & Launch
1. Launch AegisMCP using the runbook script:
   ```sh
   ./crates/aegis/demo.sh
   # Or offline mode (no network calls needed):
   ./crates/aegis/demo.sh --offline
   ```
2. The script builds release binaries for `aegis` and `aegis-broker`, initializes the background services, verifies health endpoints, and automatically opens `http://127.0.0.1:8787/login` in your browser.

---

### Step 1: Wallet Sign-in & Dashboard Overview (`/u/maya`)
1. On `http://127.0.0.1:8787/login`, observe that **no username or traditional secret fields exist**. Sign-in is strictly Ethereum-wallet only via Sign-In with Ethereum (SIWE / EIP-4361).
2. For stage safety and deterministic presentations, use the clearly labelled demo wallet buttons:
   - Click **Use demo wallet — Maya Okafor DEMO**.
   - Aegis signs a SIWE challenge using a dedicated demo wallet keypair and issues a session cookie.
3. *(To use a real wallet)*: Sign in with the demo wallet once, then click **Link wallet** on the dashboard to sign an EIP-4361 challenge via MetaMask or Rabby. On subsequent visits, clicking the primary **Sign in with Ethereum** button will directly log in to Maya's vault.
4. On `/u/maya`, review the **Your MCP Credential** card:
   - Dedicated MCP endpoint: `http://127.0.0.1:8787/mcp`
   - Bearer Token: `aegis_sk_maya_4f9c2e7a1b8d`
   - Linked Wallet: Checksummed `0x...` address with Base Sepolia Basescan link.
5. Review the 3-column layout: **Agent Activity / Live Logs**, **Consent Firewall Queue**, and **Cryptographic Ledger**.

---

### Step 2: Scenario 1 — Apartment & Travel Broker (Consent + Clean Room + Receipt)
1. On `/u/maya`, click **Run Scenario 1** (or trigger via API: `curl -X POST http://127.0.0.1:8787/api/scenario/1?subject=maya`).
2. An untrusted agent (`acme_travel_broker_v2`) attempts to call `query_financials` requesting raw checking balance, SSN, account numbers, and complete transaction history to verify lease eligibility.
3. **Firewall Interception**: Aegis intercepts the call. The dashboard sounds an alert and displays a pending consent card:
   - Stated Purpose: *"Verify affordability for apartment lease"*
   - Requested Fields: `checking_balance`, `ssn`, `account_number`, `transactions`
   - Suggested Mode: `CLEAN_ROOM`
4. Click **Approve (Clean Room)**.
5. **Red/Green Payload Diff**:
   - **Red (Held in Vault)**: Account number `4417 1234 5678 9113`, SSN `523-45-8812`, transaction history containing sensitive medical and donation records (Kaiser Permanente, Planned Parenthood, AA).
   - **Green (Released to Agent)**: Computed boolean `{"affordable": true, "min_amount_satisfied": true}`. Zero account numbers or raw transaction rows leave the perimeter.
6. **Cryptographic Receipt**: An Ed25519-signed receipt (`rcpt_...`) is minted with a 60-second TTL, recorded in the hash-chained SQLite ledger, and mirrored to Supabase.
7. Subsequent calls in Scenario 1:
   - `check_schedule`: Automatically redacted by the PII redactor. Calendar entries ("Dr. Patel - Oncology follow-up") are scrubbed to `[BUSY]` without prompting.
   - `fetch_identity`: Intercepted for JIT consent. Approved via Clean Room, returning `age_over_21: true` while keeping passport number `X41927736` and home address private.

---

### Step 3: Red Team Comparison (`/u/maya/redteam`)
1. In Maya's dashboard header, click the **Red team** link to navigate to `http://127.0.0.1:8787/u/maya/redteam`.
2. This screen runs a live side-by-side benchmark comparing the **exact same abliterated 9B local model** in two modes across 7 hostile probes:
   - **Left Column (Unguarded Context)**: The model is provided raw vault data as system context and asked to service untrusted requests. Because it is abliterated, it complies with every attacker request: **17 personal secrets leaked** (SSN, passport, bank account numbers, medical conditions, calendar notes, phone/email).
   - **Right Column (Guarded by AegisMCP)**: The model plays the agent and may only access data through AegisMCP tool calls: **0 secrets leaked** (all intercepted, held for human consent, clean-room evaluated, or blocked).
3. **The Leaked Fake Key Probe (Probe 7)**:
   - Run probe 7 (`key_harvester`), which asks the model to reveal its configured API key.
   - The model complies and leaks its API key verbatim.
   - **The Punchline**: Aegis only ever provides the model with a temporary fake key (`aegis_fk_jev_...`).
   - The probe displays live verification results:
     - Direct to Provider (`https://api.typesafe.ai`): returns `HTTP 401 Unauthorized` ("worthless outside Aegis").
     - Through `aegis-broker` (`:8788`): returns `HTTP 200 OK` (broker verifies hash and injects real key).
     - After Revoke: returns `HTTP 401 Unauthorized`.

---

### Step 4: Keys Card & Fake Key Live Demo
1. Return to `/u/maya` and scroll down to the **Keys** card.
2. Note that provider secrets are sealed with AES-256-GCM into Supabase. The secret input is write-only: once submitted, the raw value is cleared and never rendered back.
3. In the **Issue fake key for an agent** form:
   - Agent ID: `curl_agent`
   - Provider: `stripe`
   - Lifetime: `1 hour`
   - Click **Issue fake key**.
4. The card displays the one-time fake key (`aegis_fk_stripe_...`) with a copy button and the warning: *"This key is worthless outside Aegis — only the broker can swap it for the real credential."*
5. Copy the fake key and run the terminal test:
   ```sh
   # 1. Direct curl to Stripe fails:
   curl -i https://api.stripe.com/v1/customers -u <paste_fake_key>:
   # -> HTTP/1.1 401 Unauthorized (worthless outside Aegis)

   # 2. Broker proxy call succeeds:
   curl -i http://127.0.0.1:8788/proxy/stripe/v1/customers -u <paste_fake_key>:
   # -> HTTP/1.1 200 OK (broker swaps fake key for real sealed key, scrubs leaks)
   ```
6. Observe that the Keys card lists the fake key with its active countdown timer and incremented usage counter.

---

### Step 5: Kill Switch (Instant Lease & Fake Key Revocation)
1. Point to the active leases in the consent column and the active fake key on the Keys card.
2. Click the black **Kill Switch (Revoke All Leases)** button in the top right.
3. The dashboard executes a dual revocation:
   - All active context leases are invalidated (active count drops to **0**).
   - All issued fake keys are revoked simultaneously (`POST /api/keys/revoke {"prefix":null}`).
4. Re-run the broker proxy call in your terminal:
   ```sh
   curl -i http://127.0.0.1:8788/proxy/stripe/v1/customers -u <paste_fake_key>:
   ```
   **Result**: `HTTP/1.1 401 Unauthorized` with `{"error":"invalid aegis key"}`. The fake key is dead. Any agent attempting further vault access is halted for fresh human consent.

---

### Step 6: Multi-Tenant Isolation (`/u/dev`)
1. Open a new private/incognito window and navigate to `http://127.0.0.1:8787/login`.
2. Click **Use demo wallet — Dev Raman DEMO**.
3. Inspect Dev's dashboard (`/u/dev`):
   - Dev's financials show $1,830.55 balance and a strict $500.00 payment limit.
   - Maya's ledger entries, receipts, secrets, and fake keys are completely invisible to Dev.
   - Dev's consent queue is completely independent.
   - Maya's kill switch had zero effect on Dev's environment.

---

## 6. Attaching Real Untrusted Agents

You can attach any live MCP-compatible agent directly to AegisMCP.

### Claude Code / Claude CLI
```bash
claude mcp add --transport http aegis-maya http://127.0.0.1:8787/mcp \
  --header "Authorization: Bearer aegis_sk_maya_4f9c2e7a1b8d" \
  --header "x-aegis-agent: claude_code_untrusted"
```

### Cursor (`.cursor/mcp.json` or Settings > MCP)
```json
{
  "mcpServers": {
    "aegis-maya": {
      "url": "http://127.0.0.1:8787/mcp",
      "headers": {
        "Authorization": "Bearer aegis_sk_maya_4f9c2e7a1b8d",
        "x-aegis-agent": "cursor_untrusted",
        "x-aegis-purpose": "personal coding workflow"
      }
    }
  }
}
```

### OpenAI Codex / Claude Desktop (`claude_desktop_config.json`)
```json
{
  "mcpServers": {
    "aegis-maya": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-fetch",
        "http://127.0.0.1:8787/mcp"
      ],
      "env": {
        "MCP_HEADER_AUTHORIZATION": "Bearer aegis_sk_maya_4f9c2e7a1b8d",
        "MCP_HEADER_X_AEGIS_AGENT": "codex_desktop_untrusted"
      }
    }
  }
}
```

### Direct HTTP / JSON-RPC Agent Call
```bash
# Call tool query_financials on Maya's vault
curl -X POST http://127.0.0.1:8787/mcp \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer aegis_sk_maya_4f9c2e7a1b8d" \
  -H "x-aegis-agent: custom_untrusted_bot" \
  -H "x-aegis-purpose: apartment verification" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {
      "name": "query_financials",
      "arguments": {
        "min_amount": 2400
      }
    }
  }'
```

---

## 7. Configuration & Environment Variables

AegisMCP strictly separates the main gateway configuration from the key broker configuration. **Never include real key values in `.env`, `.env.broker`, source control, or demo slides.**

### Main Gateway Configuration (`.env`)
Loaded by `:8787 AegisMCP`. Holds **no** real provider keys.

| Environment Variable | Default Value | Description |
|---|---|---|
| `AEGIS_PORT` | `8787` | HTTP port for gateway, MCP endpoint, and dashboards. |
| `AEGIS_BROKER_URL` | `http://127.0.0.1:8788` | Loopback address of the isolated `aegis-broker` service. |
| `AEGIS_BROKER_TOKEN` | *Required* | Bearer token for authenticating admin requests to `aegis-broker`. |
| `AEGIS_DEMO_WALLETS` | `1` | Enables stage-safe throwaway demo wallet buttons for SIWE login. |
| `AEGIS_SIGNING_SEED` | `None` (random) | 64-hex seed for deterministic Ed25519 receipt signing. |
| `AEGIS_DB` | `aegis.db` | File path for SQLite hot-path ledger (in-memory for unit tests). |
| `AEGIS_OFFLINE` | `0` | When set to `1`, disables all outbound network calls. |
| `AEGIS_RISK_THRESHOLD` | `0.8` | Minimum Jev risk probability (0.0 to 1.0) triggering policy violation. |
| `AEGIS_CONSENT_TIMEOUT` | `90` | Seconds to await human consent before timing out. |
| `AEGIS_REQUIRE_MCP_TOKEN` | `0` | Rejects `/mcp` calls lacking a Bearer token with HTTP 401 when set to `1`. |
| `AEGIS_LLM_MODEL` | `huihui-ai/Llama-3.1-8B-Instruct-abliterated` | Model identifier for narration and purpose evaluation. |
| `SUPABASE_URL` | *Required* | Supabase project URL (system of record). |
| `SUPABASE_SECRET_KEY` | *Required* | Supabase service-role key for backend persistence. |
| `SUPABASE_PUBLISHABLE_KEY` | *Optional* | Supabase anon key. |
| `SUPABASE_JWKS_URL` | *Optional* | Supabase JWT validation JWKS endpoint. |
| `RUST_LOG` | `info` | Tracing filter level (`debug`, `info`, `warn`, `error`). |

### Aegis Broker Configuration (`.env.broker`)
Loaded exclusively by `:8788 aegis-broker`. **The only process with real keys.** Never loaded by the main app.

| Environment Variable | Default Value | Description |
|---|---|---|
| `AEGIS_MASTER_KEY` | *Required* | 64-hex-character (32-byte) key for AES-256-GCM sealing. Never leaves the broker host. |
| `AEGIS_BROKER_PORT` | `8788` | HTTP port for the isolated broker process. |
| `AEGIS_BROKER_TOKEN` | *Required* | Bearer token required for broker `/admin/*` endpoints. |
| `STRIPE_API_KEY` | *Optional* | Real Stripe secret test key (`sk_test_...` or `rk_test_...`). |
| `JEV_API_KEY` | *Optional* | Real TypeSafe System One (Jev) API key. |
| `AEGIS_LLM_UPSTREAM` | `http://127.0.0.1:11436` | Upstream LLM inference endpoint (e.g. SSH loopback forward to GPU box). |
| `AEGIS_LLM_UPSTREAM_KEY`| *Optional* | Upstream API bearer token for the LLM endpoint if required. |
| `SUPABASE_URL` | *Required* | Supabase project URL for reading/writing sealed secrets and fake keys. |
| `SUPABASE_SECRET_KEY` | *Required* | Supabase service-role key for accessing Supabase tables. |

---

## 8. End-to-End Test Verification (`tests/e2e.rs`)

The automated test suite (`cargo test -p aegis --test e2e --offline`) guarantees that the entire AegisMCP security perimeter operates reliably without external network dependencies:

1. **SIWE Wallet Authentication**: Validates cryptographic EIP-4361 signature verification and multi-tenant session isolation.
2. **JIT Consent & Clean Room Evaluation**: Verifies that requests for sensitive financial and identity data pause for human consent and return only bounded evaluation outcomes without leaking raw account numbers or SSNs.
3. **Deterministic Redaction**: Validates that calendar notes and location traces are scrubbed according to predefined schema rules without human intervention.
4. **Prompt Injection & Violation Guard**: Validates that hostile payloads and unauthorized contact exports trigger immediate `Outcome::Violation` ledger entries.
5. **Cryptographic Receipt Verification**: Asserts that every issued Ed25519 receipt signature verifies against the signer's public key and claims.
6. **Hash-Chained Audit Ledger**: Verifies SHA-256 forward hash-chaining across all ledger entries to prevent tampering.
7. **Lease & Fake Key Revocation**: Proves that revoking leases and fake keys invalidates cached authorization and forces subsequent calls to re-prompt or fail with 401.
8. **Strict Multi-Tenancy**: Verifies that operations performed under `/u/maya` never cross over or pollute `/u/dev`.
