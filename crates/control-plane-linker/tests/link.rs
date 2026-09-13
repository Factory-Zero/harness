//! The linker's contract, end to end over the public API: what composes,
//! what is refused, and which path each case takes.
//!
//! Every test here guards a property that can silently rot: the hit path
//! fetching segments, a bad segment being composed in, a corrupt cache entry
//! shipping. The bundle is digested bytes, so the determinism assertions are
//! byte assertions, not equality-of-convenience.

use cratefield_linker::{
    CachedArtifact, ComposedStore, LinkError, LinkInputs, MemorySegments, MemoryStore, Outcome,
    SegmentSource, link,
};
use cratefield_manifest::{Catalog, CatalogModule, ModuleRelease, ReleaseReview, Tier};

// ---------------------------------------------------------------------------
// Fixtures: a catalog whose pins are real digests of synthetic segments.
// ---------------------------------------------------------------------------

fn pseudo_bytes(seed: u8, len: usize) -> Vec<u8> {
    // Deterministic, cheap, not all-same: xorshift over the seed.
    let mut state = u32::from(seed) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2 + "sha256:".len());
    hex.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn module(slug: &str, seed: u8, tier: Tier, depends_on: &[&str]) -> (CatalogModule, Vec<u8>) {
    let bytes = pseudo_bytes(seed, 4096);
    let entry = CatalogModule {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        summary: format!("the {slug} module"),
        tier,
        depends_on: depends_on.iter().map(|s| (*s).to_owned()).collect(),
        releases: vec![ModuleRelease {
            version: "0.1.0".to_owned(),
            digest: sha256_hex(&bytes),
            review: ReleaseReview::Approved {
                reviewer: "test".to_owned(),
                reviewed_at: "2026-09-13T00:00:00Z".to_owned(),
            },
        }],
    };
    (entry, bytes)
}

/// A catalog of six modules (`core` plus five optional), and the segments
/// matching its pins, exactly as a release store would hold them.
fn six_modules() -> (Catalog, MemorySegments) {
    let (core, core_bytes) = module("core", 1, Tier::Core, &[]);
    let (email_signup, email_bytes) = module("email-signup", 2, Tier::Optional, &[]);
    let (waitlist, waitlist_bytes) = module("waitlist", 3, Tier::Optional, &[]);
    let (cms, cms_bytes) = module("cms", 4, Tier::Optional, &[]);
    let (notifications, notifications_bytes) =
        module("notifications", 5, Tier::Optional, &["core"]);
    let (privacy, privacy_bytes) = module("privacy", 6, Tier::Optional, &["core"]);
    let catalog = Catalog {
        modules: vec![core, email_signup, waitlist, cms, notifications, privacy],
    };
    let segments = MemorySegments::default();
    for (slug, bytes) in [
        ("core", core_bytes),
        ("email-signup", email_bytes),
        ("waitlist", waitlist_bytes),
        ("cms", cms_bytes),
        ("notifications", notifications_bytes),
        ("privacy", privacy_bytes),
    ] {
        segments.insert(slug, &sha256_hex(&bytes), bytes);
    }
    (catalog, segments)
}

fn inputs() -> LinkInputs {
    LinkInputs::release(HARNESS_API, "rustc 1.98.1 (test 2026-09-13)")
}

fn resolve(catalog: &Catalog, selected: &[&str]) -> cratefield_manifest::ModuleSet {
    catalog
        .resolve(selected)
        .expect("fixture selection resolves")
}

// A store that lies: hands back a bundle with one flipped byte under the
// original digest. This is the shape a corrupted cache entry takes, and the
// only way to produce one through the public API is to interpose it here.
struct CorruptingStore(MemoryStore);

impl ComposedStore for CorruptingStore {
    fn get(&self, build_key: &str) -> Result<Option<CachedArtifact>, LinkError> {
        Ok(self.0.get(build_key)?.map(|mut cached| {
            let last = cached.bundle.len() - 1;
            cached.bundle[last] ^= 0xff;
            cached
        }))
    }

    fn put(&self, build_key: &str, artifact: &CachedArtifact) -> Result<(), LinkError> {
        self.0.put(build_key, artifact)
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

#[test]
fn composition_is_deterministic_and_pick_order_independent() {
    let (catalog, segments) = six_modules();
    // The same five optional modules, selected in two orders.
    let one = resolve(
        &catalog,
        &[
            "email-signup",
            "waitlist",
            "cms",
            "notifications",
            "privacy",
        ],
    );
    let two = resolve(
        &catalog,
        &[
            "privacy",
            "cms",
            "waitlist",
            "notifications",
            "email-signup",
        ],
    );
    // ModuleSet's own equality includes resolution order (dependencies
    // before dependants), which legitimately differs by pick order; the
    // *set* — what keys the artifact — is the sorted releases.
    let sorted = |set: &cratefield_manifest::ModuleSet| {
        let mut releases = set.releases().to_vec();
        releases.sort_by(|a, b| a.slug.cmp(&b.slug));
        releases
    };
    assert_eq!(
        sorted(&one),
        sorted(&two),
        "the same set resolved in either pick order"
    );

    let first = link(&one, &inputs(), &segments, &MemoryStore::default()).unwrap();
    let second = link(&two, &inputs(), &segments, &MemoryStore::default()).unwrap();

    assert_eq!(
        first.bytes, second.bytes,
        "identical sets compose byte-identical bundles"
    );
    assert_eq!(first.build_key, second.build_key);
    assert_eq!(first.composition_digest, second.composition_digest);
    // The bundle's segments are slug-sorted, whatever the pick order was.
    let mut sorted: Vec<&str> = one.slugs().iter().map(String::as_str).collect();
    sorted.sort_unstable();
    assert_eq!(
        first.modules,
        sorted.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()
    );
}

#[test]
fn the_bundle_shape_is_magic_header_separator_segments() {
    let (catalog, segments) = six_modules();
    let set = resolve(&catalog, &["email-signup"]);
    let artifact = link(&set, &inputs(), &segments, &MemoryStore::default()).unwrap();

    assert!(artifact.bytes.starts_with(cratefield_linker::BUNDLE_MAGIC));
    let rest = &artifact.bytes[cratefield_linker::BUNDLE_MAGIC.len()..];
    let sep = b"\n--segments--\n";
    let split = rest
        .windows(sep.len())
        .position(|w| w == sep)
        .expect("the header/segment separator is present");
    let header: serde_json::Value =
        serde_json::from_slice(&rest[..split]).expect("the header is canonical JSON");
    assert_eq!(header["linker"], "cratefield-link-bundle-1");
    assert_eq!(header["harness-api"], u64::from(HARNESS_API));
    let modules = header["modules"].as_array().expect("header lists modules");
    assert_eq!(modules.len(), 2, "email-signup plus the core module");
    assert_eq!(modules[0]["slug"], "core", "header modules are slug-sorted");

    // The payload is exactly the pinned segments, concatenated in slug order.
    let mut payload = Vec::new();
    for slug in ["core", "email-signup"] {
        let release = set.release(slug).unwrap();
        payload.extend_from_slice(&segments.segment(slug, &release.digest).unwrap().unwrap());
    }
    assert_eq!(
        &rest[split + sep.len()..],
        &payload[..],
        "segments concatenated in slug order"
    );
}

#[test]
fn a_segment_failing_its_pin_is_refused_by_name_and_nothing_is_stored() {
    let (catalog, segments) = six_modules();
    // Sabotage one module: its stored bytes no longer match the pinned
    // digest (the catalog's pin is right; the store's bytes are wrong).
    let wrong = pseudo_bytes(99, 4096);
    segments.insert("cms", &sha256_hex(&pseudo_bytes(4, 4096)), wrong);

    let set = resolve(&catalog, &["cms"]);
    let store = MemoryStore::default();
    let err = link(&set, &inputs(), &segments, &store).unwrap_err();

    match err {
        LinkError::DigestMismatch {
            slug,
            expected,
            actual,
        } => {
            assert_eq!(slug, "cms");
            assert_eq!(expected, set.release("cms").unwrap().digest);
            assert_eq!(actual, sha256_hex(&pseudo_bytes(99, 4096)));
        }
        other => panic!("expected a digest mismatch naming `cms`, got {other:?}"),
    }
    assert!(
        store
            .get(
                &cratefield_manifest::build_key(&cratefield_manifest::BuildKeyInputs {
                    releases: set.releases().to_vec(),
                    harness_api: inputs().harness_api,
                    rustc_version: inputs().rustc_version,
                    profile: inputs().profile,
                })
                .unwrap()
            )
            .unwrap()
            .is_none(),
        "refused composition stores nothing"
    );
}

#[test]
fn a_missing_segment_is_refused_and_names_the_release() {
    let (catalog, segments) = six_modules();
    let set = resolve(&catalog, &["notifications"]);
    // `notifications` pulls in `core`; drop only `notifications`' segment.
    let release = set.release("notifications").unwrap();
    segments
        .segment("notifications", &release.digest)
        .unwrap()
        .unwrap();
    // Empty the source of just that module by rebuilding the store without it.
    let fresh = MemorySegments::default();
    let core = set.release("core").unwrap();
    fresh.insert(
        "core",
        &core.digest,
        segments.segment("core", &core.digest).unwrap().unwrap(),
    );

    match link(&set, &inputs(), &fresh, &MemoryStore::default()).unwrap_err() {
        LinkError::SegmentMissing { slug, digest } => {
            assert_eq!(slug, "notifications");
            assert_eq!(digest, release.digest);
        }
        other => panic!("expected a missing-segment refusal, got {other:?}"),
    }
}

#[test]
fn a_cache_hit_fetches_no_segments_and_returns_the_same_bundle() {
    let (catalog, segments) = six_modules();
    let set = resolve(&catalog, &["email-signup", "waitlist"]);
    let store = MemoryStore::default();

    let first = link(&set, &inputs(), &segments, &store).unwrap();
    assert_eq!(first.source, Outcome::Composed);
    let misses = segments.fetches();
    // `core` is always included, so the set is three modules.
    assert_eq!(misses, 3, "compose fetched one segment per module");

    // The second link has never seen these segments.
    let fresh_source = MemorySegments::default();
    let second = link(&set, &inputs(), &fresh_source, &store).unwrap();
    assert_eq!(second.source, Outcome::CacheHit);
    assert_eq!(fresh_source.fetches(), 0, "a hit consults no segment store");
    assert_eq!(second.bytes, first.bytes);
    assert_eq!(second.composition_digest, first.composition_digest);
    assert_eq!(second.modules, first.modules);
}

#[test]
fn a_configuration_only_change_is_a_cache_hit() {
    // The amended #59 criterion, at the linker: nothing about a venture's
    // name, host, config, seed data or sidecar mounts enters the build key,
    // so the config-only path cannot even reach the segments. The inputs
    // here are deliberately identical to prove the linker needs nothing
    // more: if config ever leaked into the key, this test's premise breaks
    // and the golden wire-form test in cratefield-manifest fails first.
    let (catalog, segments) = six_modules();
    let set = resolve(&catalog, &["email-signup"]);
    let store = MemoryStore::default();

    let before = link(&set, &inputs(), &segments, &store).unwrap();
    // ...the customer renames the venture, changes the host, mounts a
    // sidecar: none of which is visible to `link` ...
    let after = link(&set, &inputs(), &segments, &store).unwrap();

    assert_eq!(after.source, Outcome::CacheHit);
    assert_eq!(after.build_key, before.build_key);
    assert_eq!(after.bytes, before.bytes);
}

#[test]
fn a_corrupt_cache_entry_is_refused_not_shipped() {
    let (catalog, segments) = six_modules();
    let set = resolve(&catalog, &["email-signup"]);

    let honest = MemoryStore::default();
    link(&set, &inputs(), &segments, &honest).unwrap();

    let err = link(&set, &inputs(), &segments, &CorruptingStore(honest)).unwrap_err();
    assert!(matches!(err, LinkError::CorruptCache { .. }), "got {err}");
    assert!(
        err.to_string().contains("corrupt"),
        "the message says what happened: {err}"
    );
}

#[test]
fn changing_a_pinned_version_composes_a_new_bundle_not_the_old_one() {
    let (mut catalog, segments) = six_modules();
    // Re-release `waitlist` at 0.1.1 with different bytes.
    let new_bytes = pseudo_bytes(77, 4096);
    let waitlist = catalog
        .modules
        .iter_mut()
        .find(|m| m.slug == "waitlist")
        .unwrap();
    waitlist.releases.insert(
        0,
        ModuleRelease {
            version: "0.1.1".to_owned(),
            digest: sha256_hex(&new_bytes),
            review: ReleaseReview::Approved {
                reviewer: "test".to_owned(),
                reviewed_at: "2026-09-13T00:00:00Z".to_owned(),
            },
        },
    );
    segments.insert("waitlist", &sha256_hex(&new_bytes), new_bytes);

    let old_set = resolve(&six_modules().0, &["waitlist"]);
    let new_set = resolve(&catalog, &["waitlist"]);
    let store = MemoryStore::default();

    let old = link(&old_set, &inputs(), &segments, &store).unwrap();
    let new = link(&new_set, &inputs(), &segments, &store).unwrap();

    assert_ne!(old.build_key, new.build_key, "the pin is in the key");
    assert_eq!(new.source, Outcome::Composed);
    assert_ne!(old.bytes, new.bytes);
}

/// The measurement behind `docs/control-plane/LINKER.md`: six synthetic
/// segments (24 KiB total), timed cold and on a hit. Prints, and asserts
/// only the structural facts the numbers depend on (hit fetches nothing) —
/// wall-clock assertions would flake CI; the doc carries the numbers.
#[test]
fn measured_cold_compose_and_cache_hit() {
    let (catalog, segments) = six_modules();
    let set = resolve(
        &catalog,
        &[
            "email-signup",
            "waitlist",
            "cms",
            "notifications",
            "privacy",
        ],
    );
    let store = MemoryStore::default();

    let start = std::time::Instant::now();
    let composed = link(&set, &inputs(), &segments, &store).unwrap();
    let cold = start.elapsed();

    let start = std::time::Instant::now();
    let hit = link(&set, &inputs(), &segments, &store).unwrap();
    let warm = start.elapsed();

    let payload: usize = set
        .releases()
        .iter()
        .map(|r| segments.segment(&r.slug, &r.digest).unwrap().unwrap().len())
        .sum();
    println!(
        "cold compose: {cold:?} ({} segments, {payload} bytes, bundle {} bytes)\nhit:         {warm:?}",
        set.releases().len(),
        composed.bytes.len()
    );
    assert_eq!(composed.source, Outcome::Composed);
    assert_eq!(hit.source, Outcome::CacheHit);
}

/// The harness API version the fixture segments were "compiled" against. The
// real value is `cratefield_core::HARNESS_API`; the linker takes it as an
// input, and the fixture pins a literal so the test does not need the
// harness itself.
const HARNESS_API: u32 = 1;

// A segment source is a port; a source that fails must fail the link, not
/// compose around the hole.
struct FailingSource;

impl SegmentSource for FailingSource {
    fn segment(&self, _slug: &str, _digest: &str) -> Result<Option<Vec<u8>>, LinkError> {
        Err(LinkError::Store("release store unreachable".to_owned()))
    }
}

#[test]
fn a_failing_segment_source_fails_the_link() {
    let (catalog, _) = six_modules();
    let set = resolve(&catalog, &["email-signup"]);
    let err = link(&set, &inputs(), &FailingSource, &MemoryStore::default()).unwrap_err();
    assert!(matches!(err, LinkError::Store(_)), "got {err}");
}
