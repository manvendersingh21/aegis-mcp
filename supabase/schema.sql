-- AegisMCP system of record. Paste once into the Supabase SQL editor.
-- Access is service-key only: RLS is on and no policies are granted to anon/authenticated.
-- Secret values and personal vaults (provider '__vault') are AES-256-GCM ciphertext; the key (AEGIS_MASTER_KEY)
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



create table if not exists aegis_secrets (
  secret_id   text primary key,
  subject     text,                      -- null = platform secret (Jev, LLM, ...)
  provider    text not null,             -- stripe | jev | llm | __vault (sealed personal vault)
  revoked     boolean not null default false,
  meta        jsonb not null,            -- SecretMeta (label, last4, created_at) — never the value
  nonce_b64   text not null,
  ciphertext_b64 text not null           -- AES-256-GCM; key lives only on the broker host
);
create index if not exists aegis_secrets_provider on aegis_secrets(provider, subject);

create table if not exists aegis_fake_keys (
  fake_key_sha256 text primary key,      -- the fake key itself is never stored
  subject     text not null,
  agent_id    text not null,
  provider    text not null,
  revoked     boolean not null default false,
  expires_at  bigint not null,
  info        jsonb not null             -- FakeKeyInfo (prefix, uses, last_used_at, ...)
);
create index if not exists aegis_fake_keys_subject on aegis_fake_keys(subject, revoked);

create table if not exists aegis_key_usage (
  id          bigint generated always as identity primary key,
  fake_key_sha256 text not null,
  subject     text not null,
  agent_id    text not null,
  provider    text not null,
  path        text not null,
  ok          boolean not null,
  at          bigint not null
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
  foreach t in array array['aegis_people','aegis_wallets','aegis_secrets',
                           'aegis_fake_keys','aegis_key_usage','aegis_receipts','aegis_ledger']
  loop
    execute format('alter table %I enable row level security', t);
  end loop;
end $$;
