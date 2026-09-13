# Onboarding and offboarding a tenant

Issue #36, part of epic #23. Six tenants were set up from memory; the seventh
should follow a written path that ends in a verifiable state, and the last one
should be removed by a path that ends in a verifiable absence.

Each tenant is one database (ADR 0008). Nothing here is automated yet — see
§4 for exactly which piece is missing and why the command this issue asks for
is not in this commit.

## 1. Onboarding

Steps marked **operator** are a person's: they touch a cloud console or hold a
credential, and the harness cannot do them.

| # | Step | Who | Verified by |
| --: | :--- | :--- | :--- |
| 1 | Provision the database: cluster, size, PITR on | **operator** | the provider console shows PITR enabled |
| 2 | Create an application role scoped to that database only | **operator** | the role cannot see another tenant's database |
| 3 | Store the DSN as a global secret under the name that becomes `db_ref` | **operator** | `harness_secrets` holds it; the DSN is never pasted into the registry |
| 4 | Insert the registry row with status `provisioning` | *missing, §4* | the row exists and `status = 'provisioning'` |
| 5 | Reconcile that tenant: bootstrap, every module's base layer, then its extension layer | harness | `harness_migrations` in the tenant's own database |
| 6 | Seed reference data, if the tenant needs any | operator | the seed's own check |
| 7 | Flip status to `active` | *missing, §4* | `status = 'active'` |
| 8 | Configure routing — host or key — and confirm the tenant answers | **operator** | `/__health` reports the tenant |
| 9 | Record the tenant and its database in the SOC 2 asset inventory | **operator** | the row is in the inventory |

Step 3 keeps the DSN out of the registry on purpose: the registry says *which*
secret holds the connection string, never the string. A registry readable by
anything that resolves tenants would otherwise be a registry that hands out
credentials.

Step 5 applies **base then extension** per module, in `depends_on` order
(`docs/RECONCILIATION.md` §2). Extensions are `x/<schema_id>/NNNN_name`
(`docs/TENANT-EXTENSIONS.md`).

## 2. Offboarding

The mirror image, and the order matters: the export must happen while the data
is still readable, and the drop must happen after the hold, not before.

| # | Step | Who | Verified by |
| --: | :--- | :--- | :--- |
| 1 | Set status `offboarding`; the tenant stops serving | *missing, §4* | requests no longer resolve to it |
| 2 | Final export with `fz data export` | operator | the export file exists and opens |
| 3 | Crypto-shred: destroy the tenant's data keys | operator | `docs/KEY-ROTATION.md`; ciphertext no longer unwraps |
| 4 | Retention hold for the agreed period | operator | calendar entry; nothing is dropped during it |
| 5 | Drop the database after the hold | **operator** | the provider console |
| 6 | Set status `archived`; remove from the asset inventory | *missing, §4* | the tenant is absent from the inventory |

Crypto-shred before the drop, not instead of it: destroying the keys makes any
copy of the ciphertext — including backups the provider keeps past the drop —
unreadable. Dropping alone leaves readable bytes wherever a backup lives.

## 3. What each step is worth to an auditor

`docs/SOC2-MAPPING.md` cites this document for provisioning and offboarding.
The rows that lean on it: CC6.1 (a tenant's role reaches one database), CC6.7
(the shred in offboarding step 3), A1.2 (PITR from onboarding step 1).

## 4. Why the command in this issue is not here

The issue asks for `harness tenant create <id>` performing the mechanical
steps. Steps 4, 7 and the two offboarding flips are marked *missing* above
because the harness has no path to make them:

- **Nothing writes a registry row.** `crates/core/src/tenant.rs` defines
  `Tenant`, `TenantId` and resolution — all of it read-side. There is no
  insert anywhere outside the control plane, so a `create` command would have
  nothing to call.
- **Two of the statuses do not exist.** `TenantStatus` is `Active`,
  `Provisioning` and `Degraded`. Offboarding needs `offboarding` and
  `archived`, and adding them is a change to how every reader treats an
  unknown status — `provisioning_is_refused_like_degraded_and_not_like_unknown`
  is the test that pins the current behaviour.

Both belong to #154 (tenants as a product: lifecycle API), not to a CLI
wrapper. Writing the command first would mean inventing a registry write path
in the CLI that the runtime does not share — two sources of truth for what a
tenant is.

Until then this document is the procedure, and the steps a person performs are
marked as such rather than implied to be automatic.

## 5. Not yet drilled

No tenant has been onboarded by following only this document, and none has
been offboarded through it. The issue asks for both, timed. Neither is
possible yet: no tenant database has been provisioned, which is also why
`docs/ROLLBACK.md` §7 records its restore drill as unrun. The first execution
of this runbook is the thing that tells you whether it is right.
