# The artifact linker: composing a venture without cargo (issue #159)

## What the two dependencies already give

**#59 — artifact identity.** `cratefield_manifest::build_key` computes the
content address of a composition: `sha256` over the sorted
`(slug, exact version, release digest)` list plus `harness_api`, the rustc
version and the profile. Its wire form is pinned by a golden test. Deliberate
exclusions — venture name, host, config, seed data, sidecar mounts — are
exactly what makes a *configuration-only change* a non-event for the artifact:
the key does not move, so the cached artifact is still correct. What #59 does
**not** provide is any store: where artifacts physically live and the hit/miss
paths were left as control-plane decisions.

**#139 — the reviewed catalog.** `Catalog::resolve` turns a selection into a
`ModuleSet` whose every entry is a pinned, reviewed `PinnedRelease`: an exact
`N.N.N` version plus a `sha256:` digest, with revoked and unpinned releases
refused at resolution. The published `CATALOG.json` carries the same pins and
is drift-checked against the crates in CI.

**Missing between them, which #159 asks for:**

1. The per-module precompiled artifacts themselves. No module has a published
   release digest today — every pin in `CATALOG.json` carries the placeholder
   `sha256:000…0`. Until real releases are cut and digested, any linker runs
   on synthetic segments; that is a release-process gap, not a code gap.
2. A store for per-module segments and composed artifacts. Still a
   control-plane decision (unchanged from #59); the linker defines the *ports*
   it needs and ships in-memory implementations for tests.
3. The composition step itself: turn per-module segments into one venture
   artifact, deterministically, without invoking cargo.

## What the linker is — and is not

The linker composes a **venture artifact bundle**: a canonical header (the
sorted pins, the build-key inputs) followed by the per-module segments in slug
order, digest-verified against the catalog pins before use. The composed
bundle is stored under the #59 `build_key` of the set, so:

- a **configuration-only change** computes the same key, finds the cache hit,
  and never touches a segment or a compiler;
- two ventures on the same module set share the bundle, which is safe for the
  reasons `docs/ARTIFACT-CACHE.md` already states (stateless Worker, per-
  customer database/secrets/mounts);
- a segment whose bytes do not hash to its pinned digest is refused by name.

It is **not** a wasm-level link. Producing per-module wasm objects and linking
them (wasm-bindgen, wasm-opt) is toolchain work and the produced bundle is a
composition artifact, not a deployable `.wasm` — the runtime consumer of the
bundle is future work. The linker also does not decide where the store lives.

## Numbers

Measured 2026-09-13 on the machine this was written on (Apple Silicon Mac,
rustc 1.98.1, harness commit at `fix/issue-159` — see the commit message for
the exact tree state), with the toolchain pinned via
`rust-toolchain.toml`/`1.98.1-aarch64-apple-darwin`. The workload is the
synthetic six-module set from `crates/control-plane-linker/tests` (segments of
32–64 KiB of pseudorandom bytes, since no real digested releases exist yet):
the linker's cost is dominated by hashing those bytes, so real module sets of
similar byte size should land in the same range; nothing larger has been
measured.

| Path | Measured |
| :--- | ---: |
| Cache hit (config-only change): key lookup + digest check | **~40 µs** |
| Compose six segments, cold (~350 KiB total) | **~2 ms** |

Both include the sha256 of every segment and of the bundle.

Against the issue's targets, honestly:

- **"Configuration change live in under 10 s."** The linker's part of that
  path is the cache hit above — microseconds, four orders of magnitude inside
  the target. What this measurement does **not** cover is the rest of "live":
  deploying the Worker, applying schema, routing and health checks are
  `Deployer` steps this crate does not perform, and no end-to-end number for
  them exists yet.
- **"A new venture live in under 60 s."** The compose step is ~2 ms, but the
  dominant cost of a *new* module set is producing the segments in the first
  place — and per `docs/BUILD-COST.md` (issue #58's measurements) that is
  26.7 s cold or 5.3 s warm per module-set change, most of it
  wasm-bindgen/wasm-opt. If segments exist, a new venture's linker cost is
  the ~2 ms compose plus the same unmeasured deploy pipeline as above. If the
  segments do not exist, the venture pays the build — the linker cannot and
  does not claim to remove that. **The 60 s end-to-end target is therefore
  unmeasured**: no deploy pipeline exists to run it against, and this
  document will not publish a number for a path nobody has executed.

The measured claim this crate stands behind is narrower: artifact resolution
and composition do not go through cargo, and they cost milliseconds, not
seconds.
