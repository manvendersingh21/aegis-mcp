-- AegisMCP system of record. Paste once into the Supabase SQL editor.
-- Access is service-key only: RLS is on and no policies are granted to anon/authenticated.
-- Secret values and vault blobs are AES-256-GCM ciphertext; the key (AEGIS_MASTER_KEY)
-- never leaves the broker host.

create table if not exists aegis_people (
  subject       text primary key,
  display_name  text not null,
  mcp_token_sha256 text,                 -- hash only; token shown once on the dashboard
  max_payment_cents bigint not null default 0,
  created_at    timestamptz not null default now()
);

create table if not exists aegis_wallets (
  address     text primary key,          -- checksummed 0x address
  subject     text not null references aegis_people(subject) on delete cascade,
  kind        text not null default 'user' check (kind in ('user', 'demo')),
  linked_at   timestamptz not null default now()
);
create unique index if not exists aegis_wallets_subject_kind on aegis_wallets(subject, kind);

create table if not exists aegis_vaults (
  subject     text primary key references aegis_people(subject) on delete cascade,
  nonce_b64   text not null,
  ciphertext_b64 text not null,
  updated_at  timestamptz not null default now()
);

create table if not exists aegis_secrets (
  secret_id   text primary key,
  subject     text,                      -- null = platform secret (e.g. Jev, LLM)
  provider    text not null,             -- stripe | jev | llm | chain | ...
  label       text not null,
  nonce_b64   text not null,
  ciphertext_b64 text not null,
  last4       text not null,             -- for display only
  created_at  timestamptz not null default now(),
  revoked     boolean not null default false
);
create index if not exists aegis_secrets_provider on aegis_secrets(provider, subject);

create table if not exists aegis_fake_keys (
  fake_key_sha256 text primary key,      -- the fake key itself is never stored
  prefix      text not null,             -- e.g. aegis_fk_stripe_ab12 (display)
  secret_id   text not null references aegis_secrets(secret_id) on delete cascade,
  subject     text not null,
  agent_id    text not null,
  provider    text not null,
  actions     text[] not null,
  issued_at   timestamptz not null default now(),
  expires_at  timestamptz not null,
  revoked     boolean not null default false,
  last_used_at timestamptz
);
create index if not exists aegis_fake_keys_subject on aegis_fake_keys(subject, revoked);

create table if not exists aegis_key_usage (
  id          bigint generated always as identity primary key,
  fake_key_sha256 text not null,
  subject     text not null,
  agent_id    text not null,
  action      text not null,
  ok          boolean not null,
  detail      text not null default '',
  at          timestamptz not null default now()
);

create table if not exists aegis_receipts (
  receipt_id  text primary key,
  subject     text not null,
  grantee_agent text not null,
  resource    text not null,
  expires_at  bigint not null,
  revoked     boolean not null default false,
  receipt     jsonb not null
);
create index if not exists aegis_receipts_subject on aegis_receipts(subject);

create table if not exists aegis_ledger (
  seq         bigint primary key,
  subject     text not null,
  entry_hash  text not null,
  entry       jsonb not null,
  at          timestamptz not null default now()
);
create index if not exists aegis_ledger_subject on aegis_ledger(subject, seq);

do $$
declare t text;
begin
  foreach t in array array['aegis_people','aegis_wallets','aegis_vaults','aegis_secrets',
                           'aegis_fake_keys','aegis_key_usage','aegis_receipts','aegis_ledger']
  loop
    execute format('alter table %I enable row level security', t);
  end loop;
end $$;
