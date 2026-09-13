//! The artifact linker (issue #159): a venture composed from precompiled
//! per-module segments, without invoking cargo.
//!
//! Provisioning's [`Step::Artifact`] implies a build today. This crate is the
//! part that does not have to: given a resolved [`ModuleSet`] (the reviewed
//! catalog's pinned releases, issue #139) and per-module **segments** —
//! precompiled bytes already addressed by their release digests — it composes
//! one venture artifact bundle and stores it under the content address the
//! artifact cache already uses ([`build_key`], issue #59). No second cache is
//! invented: the bundle's identity *is* the #59 build key, and a
//! configuration-only change (name, host, config, seed data, sidecar mounts)
//! does not move it, so it finds the cached bundle and composes nothing.
//!
//! Two ports keep the control plane's decisions where they belong:
//!
//! - [`SegmentSource`] — where per-module segments live. The linker only
//!   requires lookup by `(slug, pinned digest)`; it verifies every segment's
//!   bytes hash to the pinned digest before use, so a store that hands back
//!   the wrong bytes is refused by name, never composed in.
//! - [`ComposedStore`] — where composed bundles live, keyed by build key.
//!   Hits are verified (sha256 of the stored bundle against the recorded
//!   composition digest) so a corrupted entry fails loudly instead of
//!   deploying as someone's venture.
//!
//! In-memory implementations of both exist for tests. Where the real stores
//! physically live remains a control-plane decision (unchanged from #59).
//!
//! ## Honesty about what this is
//!
//! This is the **resolution and composition** half of the linker. It is not a
//! wasm-level link: producing per-module wasm objects and linking them
//! (wasm-bindgen, wasm-opt — where `docs/BUILD-COST.md` shows the time
//! actually goes) is toolchain work nobody has done, and the bundle this
//! produces is a composition artifact, not a deployable `.wasm`. No real
//! release digests exist yet (the catalog pins carry placeholders), so every
//! measured number in `docs/control-plane/LINKER.md` is over synthetic
//! segments and says so.

#![forbid(unsafe_code)]

use std::collections::HashMap;
// Test/tooling fixtures, not request state (ADR 0007) — the scoped allow
// follows the policy in the workspace `clippy.toml`.
#[allow(clippy::disallowed_types)]
use std::sync::Mutex;

use cratefield_manifest::{
    BUILD_PROFILE, BuildKeyError, BuildKeyInputs, ModuleSet, PinnedRelease, build_key,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// The first line of every composed bundle: a format tag, so a consumer can
/// refuse bytes it does not understand rather than misparse them.
pub const BUNDLE_MAGIC: &[u8] = b"cratefield-link-bundle-1\n";

/// The boundary between the bundle's canonical header and the concatenated
/// segments. Part of the digested bytes; changing it changes every digest.
const SEPARATOR: &[u8] = b"\n--segments--\n";

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// The non-module inputs to [`build_key`] and to the bundle header: what the
/// composed artifact is additionally a function of beyond the module set.
/// Mirrors [`BuildKeyInputs`] minus the releases, which come from the set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInputs {
    /// The harness API version the segments were compiled against.
    pub harness_api: u32,
    /// The rustc version the segments were compiled by, verbatim
    /// (`rustc --version`). Supplied by the caller — the linker never
    /// spawns a process, so what goes into the key is exactly what the
    /// operator saw.
    pub rustc_version: String,
    /// The build profile. Callers that do not know better want
    /// [`Self::release`], the only profile any deploy path uses.
    pub profile: String,
}

impl LinkInputs {
    /// The deploy-path inputs: the release profile [`BUILD_PROFILE`].
    #[must_use]
    pub fn release(harness_api: u32, rustc_version: impl Into<String>) -> Self {
        Self {
            harness_api,
            rustc_version: rustc_version.into(),
            profile: BUILD_PROFILE.to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why linking failed. Every variant names what failed; nothing here is a
/// warning, because a bundle composed around a bad segment would deploy as
/// someone's venture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// No segment exists for this slug under its pinned digest. Either the
    /// release was never built, or the store cannot serve it.
    SegmentMissing { slug: String, digest: String },
    /// The segment's bytes do not hash to the digest the catalog pinned.
    /// The pinned digest is the release's content address; composing bytes
    /// that fail it would be composing something else.
    DigestMismatch {
        slug: String,
        expected: String,
        actual: String,
    },
    /// The cached bundle's bytes do not hash to the composition digest
    /// recorded beside them. A corrupted cache entry must fail the deploy,
    /// not ship.
    CorruptCache { build_key: String },
    /// The build key could not be computed — a duplicate slug reached the
    /// linker without going through resolution.
    Key(BuildKeyError),
    /// A store failed. Carries the store's own message.
    Store(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::SegmentMissing { slug, digest } => {
                write!(
                    f,
                    "no precompiled segment for module `{slug}` at its pinned digest {digest}; the release was never built or the store cannot serve it"
                )
            }
            LinkError::DigestMismatch {
                slug,
                expected,
                actual,
            } => write!(
                f,
                "segment for `{slug}` hashes to {actual}, not the pinned {expected}; refusing to compose bytes the catalog did not pin"
            ),
            LinkError::CorruptCache { build_key } => write!(
                f,
                "cached artifact for {build_key} does not match its recorded digest; the cache entry is corrupt and the deploy is refused"
            ),
            LinkError::Key(err) => write!(f, "{err}"),
            LinkError::Store(message) => write!(f, "artifact store failed: {message}"),
        }
    }
}

impl std::error::Error for LinkError {}

// ---------------------------------------------------------------------------
// The two ports
// ---------------------------------------------------------------------------

/// Where per-module precompiled segments live. Lookup is by the *pinned*
/// digest, not by slug alone: two publications of one version must not be
/// interchangeable, which is the whole reason the digest is in the key.
///
/// A source that cannot find a segment answers `Ok(None)` — the linker
/// refuses with [`LinkError::SegmentMissing`] — and never fabricates bytes.
#[allow(clippy::missing_errors_doc)]
pub trait SegmentSource {
    /// The bytes of `slug`'s pinned release, if the store has them.
    fn segment(&self, slug: &str, digest: &str) -> Result<Option<Vec<u8>>, LinkError>;
}

/// A composed bundle as stored: the bytes and the digest they must hash to.
/// The digest travels beside the bundle so a hit can be verified in one hash
/// without parsing the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedArtifact {
    /// `sha256:` of [`Self::bundle`], as recorded at compose time.
    pub composition_digest: String,
    /// The bundle bytes, [`BUNDLE_MAGIC`] first.
    pub bundle: Vec<u8>,
}

/// Where composed bundles live, keyed by build key. Internal mutability, so
/// [`link`] borrows it immutably — a real store is shared infrastructure,
/// not a scratch pad the linker owns.
#[allow(clippy::missing_errors_doc)]
pub trait ComposedStore {
    /// The stored bundle for this key, if any.
    fn get(&self, build_key: &str) -> Result<Option<CachedArtifact>, LinkError>;
    /// Store the bundle under this key. Content-addressed storage is
    /// idempotent: putting the same key twice stores the same bytes twice.
    fn put(&self, build_key: &str, artifact: &CachedArtifact) -> Result<(), LinkError>;
}

/// An in-memory [`ComposedStore`] — the test and tooling implementation.
/// Where real bundles physically live is a control-plane decision.
#[allow(clippy::disallowed_types)] // test fixture, not request state
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, CachedArtifact>>,
}

impl ComposedStore for MemoryStore {
    fn get(&self, build_key: &str) -> Result<Option<CachedArtifact>, LinkError> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| LinkError::Store("memory store poisoned".to_owned()))?
            .get(build_key)
            .cloned())
    }

    fn put(&self, build_key: &str, artifact: &CachedArtifact) -> Result<(), LinkError> {
        self.entries
            .lock()
            .map_err(|_| LinkError::Store("memory store poisoned".to_owned()))?
            .insert(build_key.to_owned(), artifact.clone());
        Ok(())
    }
}

/// An in-memory [`SegmentSource`] — the test implementation. Real segments
/// come from a release store that does not exist yet; no module has a
/// published digest (the catalog pins are placeholders).
#[allow(clippy::disallowed_types)] // test fixture, not request state
#[derive(Default)]
pub struct MemorySegments {
    segments: Mutex<HashMap<(String, String), Vec<u8>>>,
    fetches: Mutex<usize>,
}

impl MemorySegments {
    /// Store a segment under `(slug, digest)`. The caller is responsible for
    /// the digest matching the bytes — production segments are pinned by the
    /// catalog, and [`link`] verifies regardless.
    ///
    /// # Panics
    ///
    /// Never on valid input; the fixture mutex is only poisoned if a closure
    /// already panicked while holding it.
    pub fn insert(&self, slug: &str, digest: &str, bytes: Vec<u8>) {
        self.segments
            .lock()
            .expect("memory segment store poisoned")
            .insert((slug.to_owned(), digest.to_owned()), bytes);
    }

    /// How many times any segment was fetched. The cache-hit assertion reads
    /// this: a hit path that consults the segment source is a miss in
    /// disguise.
    ///
    /// # Panics
    ///
    /// Never on valid input; the fixture mutex is only poisoned if a closure
    /// already panicked while holding it.
    #[must_use]
    pub fn fetches(&self) -> usize {
        *self.fetches.lock().expect("memory segment store poisoned")
    }
}

impl SegmentSource for MemorySegments {
    fn segment(&self, slug: &str, digest: &str) -> Result<Option<Vec<u8>>, LinkError> {
        *self.fetches.lock().expect("memory segment store poisoned") += 1;
        Ok(self
            .segments
            .lock()
            .map_err(|_| LinkError::Store("memory segment store poisoned".to_owned()))?
            .get(&(slug.to_owned(), digest.to_owned()))
            .cloned())
    }
}

// ---------------------------------------------------------------------------
// The bundle
// ---------------------------------------------------------------------------

/// The canonical header of a composed bundle: the build-key inputs and the
/// sorted pins, serialized in declaration order. Struct serialization (not
/// `serde_json::json!`) because the header is digested bytes — its byte form
/// must be stable for the same inputs.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct BundleHeader<'a> {
    linker: &'static str,
    harness_api: u32,
    rustc_version: &'a str,
    profile: &'a str,
    modules: Vec<HeaderModule<'a>>,
}

#[derive(Serialize)]
struct HeaderModule<'a> {
    slug: &'a str,
    version: &'a str,
    digest: &'a str,
}

/// What [`link`] produced: the bundle, its address, and which path produced
/// it. `source` is the honest answer to "did this go near a compiler or a
/// segment store" — a config-only deploy is a [`Outcome::CacheHit`] and the
/// record should say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedArtifact {
    /// The #59 build key the bundle is stored under. The same key the
    /// cache, provenance and `fz build-key` all speak.
    pub build_key: String,
    /// `sha256:` of [`Self::bytes`].
    pub composition_digest: String,
    /// The module slugs, sorted (the bundle's segment order).
    pub modules: Vec<String>,
    /// Whether this came from the cache or was composed now.
    pub source: Outcome,
    /// The bundle bytes: [`BUNDLE_MAGIC`], the canonical header, the
    /// separator, the segments concatenated in slug order.
    pub bytes: Vec<u8>,
}

/// Which path [`link`] took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The bundle was already stored under this build key; no segment was
    /// fetched and nothing was composed. This is the configuration-only
    /// change path.
    CacheHit,
    /// The segments were fetched, digest-verified and composed into a new
    /// bundle, which was stored.
    Composed,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2 + "sha256:".len());
    hex.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The set's releases, slug-sorted: the order segments are fetched, verified
/// and laid into the bundle, and the order the header lists them. A
/// composition must be byte-identical however the customer picked the
/// modules, so nothing here may depend on resolution order.
fn sorted_releases(set: &ModuleSet) -> Vec<&PinnedRelease> {
    let mut sorted: Vec<&PinnedRelease> = set.releases().iter().collect();
    sorted.sort_by(|a, b| a.slug.cmp(&b.slug));
    sorted
}

/// Link a venture: resolve every pinned release to its precompiled segment,
/// verify each against the catalog's digest, and compose the bundle — or
/// return the stored one when this build key is already cached.
///
/// The cache is consulted **first** and a hit touches nothing else: no
/// segment fetch, no hashing of segment bytes. That is the property a
/// configuration-only change needs — the key does not move, so the stored
/// bundle is still exactly the artifact this set names.
///
/// # Errors
///
/// [`LinkError::SegmentMissing`] when a pinned release has no precompiled
/// segment; [`LinkError::DigestMismatch`] when one's bytes fail its pin;
/// [`LinkError::CorruptCache`] when a hit's bytes fail their recorded
/// digest; [`LinkError::Key`] on a duplicate slug; [`LinkError::Store`] when
/// a store fails.
pub fn link(
    set: &ModuleSet,
    inputs: &LinkInputs,
    segments: &impl SegmentSource,
    store: &impl ComposedStore,
) -> Result<LinkedArtifact, LinkError> {
    let key = build_key(&BuildKeyInputs {
        releases: set.releases().to_vec(),
        harness_api: inputs.harness_api,
        rustc_version: inputs.rustc_version.clone(),
        profile: inputs.profile.clone(),
    })
    .map_err(LinkError::Key)?;

    if let Some(cached) = store.get(&key)? {
        let actual = sha256_hex(&cached.bundle);
        if actual != cached.composition_digest {
            return Err(LinkError::CorruptCache { build_key: key });
        }
        return Ok(LinkedArtifact {
            modules: sorted_releases(set)
                .iter()
                .map(|r| r.slug.clone())
                .collect(),
            build_key: key,
            composition_digest: cached.composition_digest,
            source: Outcome::CacheHit,
            bytes: cached.bundle,
        });
    }

    let releases = sorted_releases(set);
    let mut payload = Vec::new();
    for release in &releases {
        let bytes = segments
            .segment(&release.slug, &release.digest)?
            .ok_or_else(|| LinkError::SegmentMissing {
                slug: release.slug.clone(),
                digest: release.digest.clone(),
            })?;
        let actual = sha256_hex(&bytes);
        if actual != release.digest {
            return Err(LinkError::DigestMismatch {
                slug: release.slug.clone(),
                expected: release.digest.clone(),
                actual,
            });
        }
        payload.extend_from_slice(&bytes);
    }

    let header = serde_json::to_vec(&BundleHeader {
        linker: "cratefield-link-bundle-1",
        harness_api: inputs.harness_api,
        rustc_version: &inputs.rustc_version,
        profile: &inputs.profile,
        modules: releases
            .iter()
            .map(|r| HeaderModule {
                slug: &r.slug,
                version: &r.version,
                digest: &r.digest,
            })
            .collect(),
    })
    .map_err(|err| LinkError::Store(format!("bundle header failed to serialize: {err}")))?;

    let mut bundle =
        Vec::with_capacity(BUNDLE_MAGIC.len() + header.len() + SEPARATOR.len() + payload.len());
    bundle.extend_from_slice(BUNDLE_MAGIC);
    bundle.extend_from_slice(&header);
    bundle.extend_from_slice(SEPARATOR);
    bundle.extend_from_slice(&payload);

    let composition_digest = sha256_hex(&bundle);
    store.put(
        &key,
        &CachedArtifact {
            composition_digest: composition_digest.clone(),
            bundle: bundle.clone(),
        },
    )?;

    Ok(LinkedArtifact {
        modules: releases.iter().map(|r| r.slug.clone()).collect(),
        build_key: key,
        composition_digest,
        source: Outcome::Composed,
        bytes: bundle,
    })
}
