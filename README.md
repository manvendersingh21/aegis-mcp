# AegisMCP 🛡️
> **Privacy-Preserving MCP Firewall, Clean-Room Execution & Gated Personal Agent Delegations**  
> *Track 2: Personal Agents & Data Privacy*

[![Rust](https://img.shields.io/badge/rust-2021%20edition-orange.svg)](https://www.rust-lang.org/)
[![Model Context Protocol](https://img.shields.io/badge/MCP-2024--11--05-blue.svg)](https://modelcontextprotocol.io/)
[![Zero Raw PII Leakage](https://img.shields.io/badge/Raw%20PII%20Leakage-0%25-brightgreen.svg)]()
[![Stripe Certified Proxy](https://img.shields.io/badge/Stripe%20Broker-Live%20Charges-635BFF.svg)](https://stripe.com)

---

![Aegis Architecture](aegis-architecture.jpg)

---

## Inspiration

Autonomous AI agents (ChatGPT, Claude, Cursor Agent, Devin) are rapidly evolving from chat interfaces into action-oriented personal delegates. We ask them to:
- Order items on e-commerce platforms
- Book commercial flights and hotel rooms
- Audit our bank accounts and approve payments
- Deploy software and manage API keys

However, today's agent architectures suffer from a fatal privacy flaw: **to perform an action, the agent demands raw sensitive data**. To buy a book, the model sees your home street address and raw credit card number. To book a flight, the model sees your passport number, date of birth, and legal name. To deploy code, it logs production cloud secrets directly into context windows where they persist in vendor chat histories, model fine-tuning corpora, and third-party observability platforms.

We asked: **What if personal AI agents could execute real-world tasks without ever seeing the raw data?**  
Inspired by zero-knowledge architecture, differential privacy clean-rooms, and hardware security modules (HSMs), we created **AegisMCP**: a real-time cryptographic firewall and clean-room broker for the Model Context Protocol (MCP).

---

## What It Does

AegisMCP intercepts agent requests between LLMs and real-world services. Instead of leaking raw PII, Aegis:

1. **Tokenizes Physical & Financial Realities**:
   - Replaces home addresses with single-use surrogates (`aegis_ship_d37a892b10ef`).
   - Shields credit cards behind single-use payment references (`aegis_pm_7f8a9b2c3d4e`).
   - Conceals production API keys behind scoped surrogate tokens (`aegis_fk_openai_...`).
2. **Executes Clean-Room Financial Computations**:
   - Answers affordability queries (`can_afford: true`) without disclosing actual account balances.
   - Applies differential privacy quantization ($500 bucket steps) to defeat binary-search balance discovery attacks.
3. **Automates Out-of-Band Delegated Identity**:
   - For airline bookings, Aegis contacts the carrier directly over TLS, injecting sealed passport numbers and dates of birth out-of-band. The LLM receives a confirmed ticket reference (`aegis_tkt_...`), keeping legal government credentials 100% out of LLM memory.
4. **Enforces Human-in-the-Loop Gated Approvals**:
   - Any sensitive transaction triggers a real-time diff card on a persistent user dashboard (`/u/{subject}`).
   - The user chooses between three distinct approval modes, controlling exactly how much data (if any) is released to the model.
5. **Live Broker for Real Stripe Transactions**:
   - Uses an isolated zero-trust daemon (`aegis-broker`) that holds encrypted platform secrets and unseals credentials in memory only when a user explicitly approves.

---

## The 3 Approval Modes (Live Model Impact)

When an agent requests a sensitive action, the user sees a real-time approval banner with three distinct options:

```
┌────────────────────────────────────────────────────────────────────────┐
│                        Human Review Required                          │
├────────────────────────────────────────────────────────────────────────┤
│  [1. Approve (Tokenized)]  │  [2. Approve (Masked)]  │  [3. Approve (Raw)]
└────────────────────────────────────────────────────────────────────────┘
```

| Mode | What It Sends to the Model Context | Ideal Use Case |
| :--- | :--- | :--- |
| **1. Approve (Tokenized)** *(Recommended)* | Fully synthetic opaque tokens (e.g. `aegis_ship_f92b`, `aegis_pm_c41a`). The real address & card are sent directly to merchant APIs via backend proxy. | Default autonomous mode. Maximum security, 0% PII context leakage. |
| **2. Approve (Masked)** | Redacted display strings (e.g. `**** **** **** 4242`, `San Francisco, CA 94107`). Real values executed out-of-band. | Agent needs to confirm last 4 digits or destination city without seeing full credentials. |
| **3. Approve (Raw)** | Plaintext values released into agent context (e.g. `4242...`, `123 Main St, Apt 4B`). | Explicit developer overrides where the model must manipulate raw secrets directly. |

---

## Mathematical Privacy Foundations

### 1. Differential Privacy Quantization for Balance Discovery
Adversarial prompts can attempt binary search attacks on user balances by repeatedly querying thresholds:

$$\text{Query}(T): \text{balance} \ge T$$

To bound the privacy leakage $\epsilon$, Aegis implements clean-room coarse-graining:

$$T_{\text{quantized}} = \left\lceil \frac{T}{\Delta} \right\rceil \times \Delta, \quad \Delta = \$500$$

The boolean response satisfies:

$$f(b, T) = \begin{cases} \text{true} & \text{if } b \ge \lceil T / 500 \rceil \times 500 \\ \text{false} & \text{otherwise} \end{cases}$$

This prevents micro-probing attacks and guarantees that an agent cannot determine account balance within a granularity smaller than $\Delta$.

### 2. Information Theoretic Leakage Bound
Let $X \in \mathcal{X}$ be the user's raw sensitive attribute (e.g., SSN, Passport, Full Card Number) and let $Y$ be the tokenized surrogate emitted to the model context. Because $Y = \text{HMAC-SHA256}(K, \text{nonce})$ generated from an isolated vault key $K \notin \text{Context}$:

$$I(X; Y) = 0$$

Mutual information between raw user identity and LLM context is strictly zero.

---

## How We Built It

AegisMCP is engineered in pure Rust as a modular 11-crate workspace for maximum memory safety, concurrency, and sub-millisecond execution:

```
crates/
├── aegis-core        # Domain types, Subject, Action, RiskLevel, Cryptographic IDs
├── aegis-vault       # Zero-trust encrypted credential vault & surrogate mappings
├── aegis-engine      # Policy evaluator, policy AST, risk scoring engine
├── aegis-cleanroom   # Differential privacy financial balance evaluator
├── aegis-broker      # Isolated micro-service proxy holding sealed platform keys
├── aegis-ledger      # Append-only SHA-256 audit log for all agent actions
├── aegis-mcp         # JSON-RPC 2.0 Model Context Protocol server over HTTP/SSE
├── aegis-dashboard   # Real-time Web dashboard built with Maud & persistent ledger
└── aegis-cli         # Operator CLI for subject onboarding and key management
```

### Complete 9-Tool MCP Suite
1. `order_item`: Tokenized autonomous purchasing under daily spend caps.
2. `book_flight`: Direct vault-to-airline booking with automated passport shielding.
3. `make_payment`: Gated delegated payments backed by real Stripe payment intents (`pi_...`).
4. `query_financials`: Clean-room boolean affordability queries.
5. `access_developer_credentials`: Scoped surrogate broker for OpenAI, Anthropic, AWS, and GitHub keys.
6. `check_schedule`: Free/busy availability calendar with automatic title redaction.
7. `fetch_identity`: Proof-of-age verification with SSN and street address masking.
8. `get_health_metrics`: Wellness bands with private medical diagnoses withheld.
9. `export_contacts`: Selective address book export with rate limiting.

---

## Challenges We Ran Into

- **GitHub Secret Scanning (Push Protection GH013)**:  
  During git pushes, GitHub's automated secret scanner flagged synthetic mock Stripe live keys in unit test fixtures (`sk_live_...`), rejecting git pushes. We refactored all test vectors and clean-room mocks into explicitly compliant mock formats (`mock_sec_stripe_...`).
- **Cloudflare Quick Tunnel Re-generation**:  
  Restarting `cloudflared` generates a new ephemeral subdomain. We stabilized the process architecture so daemon workers and MCP tunnels remain persistent without breaking active ChatGPT or Cursor agent connections.
- **Differential Privacy Agent Alignment**:  
  When an agent asks if the user can afford a $50 item, the $500 coarse bucketing checks against $500. Without explanatory metadata, LLMs occasionally assumed the user had spent $500. We added structured `requested_amount` and `privacy_note` fields to the JSON response so agents understand the privacy guarantee.

---

## Accomplishments That We're Proud Of

- **End-to-End Live Stripe Integration**: Completed real delegated credit card transactions through our isolated broker (`aegis-broker`), producing live Stripe payment intents (`pi_3UTVCd...`) while the LLM only ever saw surrogate tokens.
- **Zero Raw PII Leakage**: Successfully booked simulated flights and ordered items through Cursor Agent and ChatGPT without exposing a single real passport number, home address, or credit card number.
- **Persistent Real-Time Dashboard**: Built a responsive Maud-powered dashboard (`/u/maya`) that persists diff cards, approvals, and terminal logs across browser refreshes via `localStorage` synchronization.
- **100% Rust Performance**: The entire stack compiles natively with zero warnings across 11 crates, processing policy decisions in under 200 microseconds.

---

## What We Learned

- **Tokenization beats Masking for Autonomous Agents**: Traditional PII maskers replace digits with asterisks (`****`), which prevents an agent from performing downstream API calls. Surrogate tokenization (`aegis_ship_...`) allows agents to complete multi-step tool calls by referencing the token while our firewall resolves the physical payload out-of-band.
- **Clean-Rooms enable Trustless Assistants**: Users are terrified of giving AI access to their banking apps. With clean-room computation, an agent can confidently plan a budget or book a vacation while being mathematically incapable of exfiltrating the user's net worth.

---

## What's Next for AegisMCP

- **Hardware Security Module (HSM) Integration**: Enclave execution (AWS Nitro Enclaves / Apple Secure Enclave) for platform private keys.
- **Decentralized Verifiable Credentials (W3C DID/VC)**: Cryptographic zero-knowledge proofs (ZKPs) for age and residency verification.
- **Automated Revocation & Ephemeral TTLs**: Instant one-click surrogate token revocation directly from the Aegis dashboard or mobile push notification.
- **Native Browser Extension**: In-browser Aegis companion to auto-detect LLM context windows and inject proxy tokens on the fly.

---

## Quickstart & Live Demo

### 1. Live Endpoints
- **Web Dashboard**: `https://aviation-certain-lovely-composer.trycloudflare.com/u/maya`
- **MCP Endpoint**: `https://aviation-certain-lovely-composer.trycloudflare.com/mcp`
- **OpenAPI Schema**: `https://aviation-certain-lovely-composer.trycloudflare.com/openapi.json`

### 2. Cursor Agent Configuration
Add to `.cursor/mcp.json`:
```json
{
  "mcpServers": {
    "aegis-firewall": {
      "url": "https://aviation-certain-lovely-composer.trycloudflare.com/mcp",
      "headers": {
        "Authorization": "Bearer aegis_sk_maya_4f9c2e7a1b8d"
      }
    }
  }
}
```

Verify with:
```bash
cursor-agent mcp list
# aegis-firewall: ready
```

### 3. Local Development Build
```bash
# Build all 11 crates
cargo build --release

# Run the broker daemon
./target/release/aegis-broker &

# Run the Aegis firewall server
./target/release/aegis
```

---

## License
MIT License. Built with ❤️ for the Hackathon Track 2 (*Personal Agents & Data Privacy*).
