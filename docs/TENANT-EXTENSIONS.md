# Tenant extensions: where a venture's own schema lives

Issue #31, part of epic #23. The rule for schema a venture needs that the
shared modules do not provide.

Without it, the first tenant-specific column lands as an edit to a module's
base migration — which the never-edit rule forbids, and which every other
tenant would then inherit.

## 1. Where they live

```
tenants/<tenant_id>/<schema_id>/NNNN_name.sql
```

`schema_id` is the module whose schema is being extended (`notifications`,
`waitlist`), or `venture` for tables that belong to no module. `NNNN` is a
zero-padded sequence, per `(tenant_id, schema_id)`, starting at `0001`.

They are **embedded at build** from a private tenants crate, the same way a
module embeds its own migrations with `include_str!`. Loading them from a
bucket at boot was considered and rejected: it makes a deploy
non-reproducible — the same artifact would do different things depending on
what a bucket held at the moment it booted, and a rollback would not roll the
schema back.

## 2. How they are recorded

Each tenant has its own database (ADR 0008, issue #32), so the tenant id is
**implicit in which database the row is in**. It is not part of the key.

Migrations are recorded in that database's `harness_migrations`:

```sql
harness_migrations (id TEXT PRIMARY KEY, applied_at TEXT NOT NULL, checksum TEXT)
```

The extension layer needs no new table and no `layer` column. The issue
proposed `harness.tenant_migrations` with `layer = 'extension'`; that predates
database-per-tenant, and a second table would be a second source of truth for
"what has been applied here". The layer is carried in the id instead:

| layer | id shape | example |
| :--- | :--- | :--- |
| base | `<module>/<NNNN>_<name>` | `notifications/0009_outbox_subject` |
| extension | `x/<schema_id>/<NNNN>_<name>` | `x/notifications/0001_deal_tags` |

The `x/` prefix makes the two streams unable to collide in the primary key,
which is the property `docs/MIGRATION-STREAMS.md` §2 shows matters: a name
collision is skipped and reported as success, so two streams that can share a
name lose migrations silently.

Reconciliation already applies base then extension per module
(`docs/RECONCILIATION.md` §2); this is the id shape it applies them under.

## 3. Namespacing: the `x_` prefix

Every object an extension creates is prefixed `x_`:

```sql
CREATE TABLE x_crm_deal_tags (...);
CREATE INDEX x_crm_deal_tags_by_deal ON x_crm_deal_tags (deal_id);
```

Module base migrations are **forbidden** from using the `x_` prefix. The two
namespaces therefore cannot meet, and a module author adding a table never has
to know what any tenant has created.

## 4. What an extension may and may not do

**May:**

- create `x_` tables, indexes on them, and views over them;
- add an index to a module-owned table;
- add a foreign key **from** an `x_` table **to** a module-owned table.

**May not:**

- alter or drop a module-owned table or column;
- add a column to a module-owned table;
- create any object without the `x_` prefix.

The boundary is one-directional on purpose: an extension may depend on module
schema, never the reverse. A module that had to know about `x_` tables would
be a module that cannot be upgraded without reading every tenant's
extensions.

The lint that enforces this is #34's to build; this document is its
specification. It must reject `ALTER TABLE` whose target is not `x_`-prefixed,
except `ADD CONSTRAINT` and `CREATE INDEX`.

**The rule is a prefix, not a substring, and the difference is not academic.**
Every module migration in the tree today is clean — no object is named `x_…` —
but `crates/auth-core` creates `idx_credentials_user_id`,
`idx_identities_user_id` and `idx_sessions_user_id`. A lint that asks whether
a name *contains* `x_` flags all three and fails on migrations that are
already applied and may never be edited. It must ask whether the object name
*starts with* `x_`.

## 5. Base upgrades underneath an extension

A base migration can break an extension without touching it — dropping a
column that an `x_` foreign key points at. The module author is the one who
can detect it, and the authoring guide requires reconciling against a fixture
of every tenant's extensions in CI:

```
fixtures/tenant-extensions/<tenant_id>/<schema_id>/NNNN_name.sql
```

The fixture is the union of what tenants have, checked in, so CI reconciles a
module's new base migration against every extension that exists before the
migration ships. A tenant whose extensions are not in the fixture is not
protected — which is the argument for extensions living in the repository
rather than in a bucket.

## 6. What this document cannot yet claim

- **No real examples.** The issue asks for two from current tenants. There
  are none: the multi-tenant runtime landed in #32, but no tenant database has
  been provisioned, so every example here is constructed. They are marked as
  such rather than presented as observed.
- **Not yet agreed with the tenant-facing team.** The issue makes that an
  acceptance criterion and it is a conversation, not a commit.
- **The lint does not exist.** #34 builds it. Until then the `x_` rule is a
  documented convention, and a convention with no check is a convention that
  will be broken — which is the reason #34 exists and this is its spec.
