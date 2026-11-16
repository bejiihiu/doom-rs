//! Centralized resource management, re-imagined for Rust.
//!
//! Vanilla kept everything in zone memory and cached by hand: `W_CacheLumpNum`
//! with `PU_CACHE` tags, `Z_ChangeTag` when a block ran out of room, and a
//! hard-coded eviction order in `Z_FreeTags`. That machinery existed because
//! the zone allocator had a fixed size and reused blocks aggressively.
//!
//! The modern equivalent does not need any of that. The data is small enough
//! to keep in memory for the whole session, so this module offers what vanilla
//! never had: typed caches with lazy decoding and explicit statistics. Raw
//! lump bytes are always borrowed from the owning WADs — nothing is copied or
//! cached — and decoded values (palettes, headers, lookup tables, …) are
//! cached once per (type, lump) pair and shared as [`Arc<T>`](std::sync::Arc).
//!
//! Ownership: [`crate::resources::ResourceManager`] owns the [`crate::wad::WadSet`] outright. Renderer and
//! game code receive `&ResourceManager` and never see the underlying set;
//! everything the set can do (name lookup, stacking order, namespace
//! markers) is delegated here.
//!
//! Decode failures are **not** cached. A failed decoder returns its error
//! every time and the next call tries again from the raw bytes.
//!
//! The manager is single-threaded (the renderer runs on one thread); cache
//! stats are plain counters behind a `RefCell`, not atomics. Cached values
//! are shared as `Arc<T>` so decoding happens at most once per lump and type.
//!
//! ```no_run
//! use doom_engine::resources::ResourceManager;
//! use doom_engine::wad::WadFile;
//!
//! # fn example() -> doom_engine::errors::EngineResult<()> {
//! let iwad = WadFile::open("DOOM.WAD")?;
//! let manager = ResourceManager::new(iwad, Vec::new());
//! let playpal = manager.lump_data("PLAYPAL")?;
//! # Ok(())
//! # }
//! ```

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::errors::EngineResult;
use crate::wad::{WadFile, WadSet};

/// Snapshot of cache activity counters.
///
/// Returned by [`ResourceManager::stats`]; all fields are plain counters and
/// this struct is a cheap copy. `bytes_decoded` sums the lump payload fed
/// into decoders — a decode of a 4096-byte lump adds 4096, regardless of how
/// big the decoded value is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Cached value was reused; the decoder was not called.
    pub hits: u64,
    /// No cached value for the (type, lump) pair; a decode was attempted.
    pub misses: u64,
    /// Decodes that completed successfully.
    pub decodes: u64,
    /// Total lump payload bytes fed into successful decodes.
    pub bytes_decoded: u64,
}

/// A typed cache for decoded lump data, keyed by lump index.
///
/// The index is the *global* index into the combined namespace
/// ([`WadSet::entries`]), not an index into a single WAD — that is the same
/// number [`ResourceManager::find_lump`] returns.
///
/// Every element is stored once and handed out as an `Arc<T>`; there is no
/// per-call decode of a cached lump ("one request — one object", same idea
/// as vanilla's cached zone blocks, but typed).
#[derive(Debug)]
pub struct LumpCache<T> {
    map: HashMap<usize, Arc<T>>,
}

impl<T> LumpCache<T> {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// The cached value for `index`, if any, or `None`.
    pub fn get(&self, index: usize) -> Option<Arc<T>> {
        self.map.get(&index).map(Arc::clone)
    }

    /// Stores `value` for `index` and returns the shared handle.
    pub fn insert(&mut self, index: usize, value: T) -> Arc<T> {
        let arc = Arc::new(value);
        self.map.insert(index, Arc::clone(&arc));
        arc
    }

    /// Number of cached values.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// True when `index` holds a cached value.
    pub fn contains(&self, index: usize) -> bool {
        self.map.contains_key(&index)
    }

    /// Drops every cached value.
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

impl<T> Default for LumpCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Type-erased view of a [`LumpCache<T>`].
///
/// Lets the manager hold caches of every type behind one map; the only
/// operations the manager needs on the trait object are `len`/`clear` and
/// the round trip back to the concrete cache via `as_any_mut`.
trait ErasedLumpCache: Any {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn len(&self) -> usize;
    fn clear(&mut self);
}

impl<T: 'static> ErasedLumpCache for LumpCache<T> {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

/// The resource manager: owns the stacked WADs and every typed cache.
///
/// All access goes through `&self`; cached values and stats use interior
/// mutability because the engine is single-threaded. This is a deliberate
/// ownership choice: the manager is passed around as a plain reference
/// (`&ResourceManager`) instead of being shared through `Rc<RefCell<_>>`.
///
/// Build one with [`ResourceManager::new`] (an [`WadFile`] IWAD plus optional
/// patch WADs) or [`ResourceManager::from_paths`] (paths, in load order).
/// [`ResourceManager::default`] produces an empty manager whose every lookup
/// fails with a clear error — useful in tests, pointless for a running game.
pub struct ResourceManager {
    wads: WadSet,
    caches: RefCell<HashMap<TypeId, Box<dyn ErasedLumpCache>>>,
    stats: RefCell<CacheStats>,
}

impl fmt::Debug for ResourceManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceManager")
            .field("wads", &self.wads)
            .field("cache_entries", &self.cache_size())
            .field("stats", &*self.stats.borrow())
            .finish()
    }
}

impl ResourceManager {
    /// Builds a manager from an IWAD and any number of patch WADs.
    ///
    /// Load order is `iwad` first, then `pwads` — that order defines the
    /// stacking rule (a later lump shadows an earlier one of the same name).
    pub fn new(iwad: WadFile, pwads: Vec<WadFile>) -> Self {
        let mut set = WadSet::new();
        set.add_wad(iwad);
        for patch in pwads {
            set.add_wad(patch);
        }
        Self::from_wad_set(set)
    }

    /// Builds a manager from paths, in load order (IWAD first).
    ///
    /// Fails with [`crate::wad::WadError::NoWadsLoaded`] when `paths` is empty.
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> EngineResult<Self> {
        Ok(Self::from_wad_set(WadSet::from_paths(paths)?))
    }

    fn from_wad_set(wads: WadSet) -> Self {
        Self {
            wads,
            caches: RefCell::new(HashMap::new()),
            stats: RefCell::new(CacheStats::default()),
        }
    }

    /// The stacked WADs, in load order. Read-only; the manager owns them.
    pub fn wads(&self) -> &WadSet {
        &self.wads
    }

    /// Finds the lump with `name`, searching from the end of the directory
    /// (last file wins). Returns the global lump index, if any.
    pub fn find_lump(&self, name: &str) -> Option<usize> {
        self.wads.find_lump(name)
    }

    /// Raw bytes of the lump with `name`. Never copied, never cached —
    /// borrowed straight from the owning WAD.
    pub fn lump_data(&self, name: &str) -> EngineResult<&[u8]> {
        Ok(self.wads.lump_data_named(name)?)
    }

    /// Raw bytes of the lump at global index `index`. See
    /// [`lump_data`](Self::lump_data).
    pub fn lump_data_index(&self, index: usize) -> EngineResult<&[u8]> {
        Ok(self.wads.lump_data(index)?)
    }

    /// Global index of the first flat (`F_START` + 1), if the marker exists.
    pub fn first_flat(&self) -> Option<usize> {
        self.wads.first_flat()
    }

    /// Global index of the last flat (`F_END` - 1), if the marker exists.
    pub fn last_flat(&self) -> Option<usize> {
        self.wads.last_flat()
    }

    /// Global index of the first sprite (`S_START` + 1), if the marker exists.
    pub fn first_sprite(&self) -> Option<usize> {
        self.wads.first_sprite()
    }

    /// Global index of the last sprite (`S_END` - 1), if the marker exists.
    pub fn last_sprite(&self) -> Option<usize> {
        self.wads.last_sprite()
    }

    /// The cached value for `lump_index`, decoding it on first use.
    ///
    /// Decoding runs [`decoder`](Self) on the raw lump bytes, stores the
    /// result in the cache for `T` and returns a shared [`Arc<T>`](std::sync::Arc). Every
    /// later call for the same pair returns the stored `Arc` — the decoder
    /// is not called again. Raw-byte access through [`lump_data`](Self::lump_data)
    /// does not touch this cache.
    ///
    /// A miss is counted whenever the value is not cached, even if reading
    /// the lump or decoding then fails; a failed decode is not cached, so
    /// the next call retries from the raw bytes. Decodes and
    /// `bytes_decoded` are only counted on success.
    pub fn decode_cached<T: 'static>(
        &self,
        lump_index: usize,
        decoder: impl FnOnce(&[u8]) -> EngineResult<T>,
    ) -> EngineResult<Arc<T>> {
        let mut caches = self.caches.borrow_mut();
        let erased = caches
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(LumpCache::<T>::new()));
        // entries are only ever inserted under TypeId::of::<T>() below, so
        // the downcast is an invariant of this function, not a runtime risk
        let cache = erased
            .as_any_mut()
            .downcast_mut::<LumpCache<T>>()
            .expect("cache registered under TypeId::of::<T>() is always a LumpCache<T>");
        let mut stats = self.stats.borrow_mut();

        if let Some(hit) = cache.get(lump_index) {
            stats.hits += 1;
            return Ok(hit);
        }
        stats.misses += 1;

        let data = self.wads.lump_data(lump_index)?;
        let bytes = data.len() as u64;
        let decoded = decoder(data)?;
        stats.decodes += 1;
        stats.bytes_decoded += bytes;
        Ok(cache.insert(lump_index, decoded))
    }

    /// Total number of cached values across all types.
    pub fn cache_size(&self) -> usize {
        self.caches.borrow().values().map(|c| c.len()).sum()
    }

    /// Drops every cached value. Raw lump data is unaffected.
    pub fn clear_cache(&self) {
        for cache in self.caches.borrow_mut().values_mut() {
            cache.clear();
        }
    }

    /// Current cache counters.
    pub fn stats(&self) -> CacheStats {
        *self.stats.borrow()
    }

    /// Zeroes all cache counters.
    pub fn reset_stats(&self) {
        *self.stats.borrow_mut() = CacheStats::default();
    }
}

impl Default for ResourceManager {
    /// An empty manager with no WADs loaded; every lookup fails with a
    /// clear error. Meant for tests and tools, not for a running game.
    fn default() -> Self {
        Self::from_wad_set(WadSet::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wad::testutil::build_wad;

    fn wad_bytes(magic: &[u8; 4], lumps: &[(&str, &[u8])]) -> Vec<u8> {
        build_wad(magic, lumps)
    }

    fn manager_with(iwad_lumps: &[(&str, &[u8])], pwads: &[&[(&str, &[u8])]]) -> ResourceManager {
        let iwad = WadFile::from_bytes(wad_bytes(b"IWAD", iwad_lumps)).unwrap();
        let patches = pwads
            .iter()
            .map(|lumps| WadFile::from_bytes(wad_bytes(b"PWAD", lumps)).unwrap())
            .collect();
        ResourceManager::new(iwad, patches)
    }

    #[test]
    fn decoder_runs_once_and_repeat_requests_share_the_arc() {
        let mgr = manager_with(&[("PLAYPAL", b"palette-bytes")], &[]);
        let index = mgr.find_lump("PLAYPAL").unwrap();
        let calls = std::cell::Cell::new(0u64);

        let first = mgr
            .decode_cached(index, |bytes| {
                calls.set(calls.get() + 1);
                Ok(bytes.to_vec())
            })
            .unwrap();
        let second = mgr
            .decode_cached(index, |bytes| {
                calls.set(calls.get() + 1);
                Ok(bytes.to_vec())
            })
            .unwrap();

        assert_eq!(calls.get(), 1, "decoder must not run for a cache hit");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(&first[..], b"palette-bytes");
    }

    #[test]
    fn distinct_lumps_decode_independently() {
        let mgr = manager_with(&[("A", b"aaa"), ("B", b"bbbb")], &[]);
        let a = mgr.find_lump("A").unwrap();
        let b = mgr.find_lump("B").unwrap();

        let first = mgr.decode_cached(a, |bytes| Ok(bytes.len())).unwrap();
        let second = mgr.decode_cached(b, |bytes| Ok(bytes.len())).unwrap();

        assert_eq!(*first, 3);
        assert_eq!(*second, 4);
        assert_eq!(mgr.stats().decodes, 2);
    }

    #[test]
    fn decoder_error_is_propagated_and_not_cached() {
        let mgr = manager_with(&[("BAD", b"corrupt")], &[]);
        let index = mgr.find_lump("BAD").unwrap();
        let calls = std::cell::Cell::new(0u64);

        let err = mgr
            .decode_cached::<usize>(index, |_| {
                calls.set(calls.get() + 1);
                Err(crate::errors::EngineError::Resource("bad magic".into()))
            })
            .unwrap_err();
        assert!(err.to_string().contains("bad magic"));
        assert_eq!(mgr.cache_size(), 0, "failed decode must not be cached");

        // the very next call must try decoding again, not replay the error
        let ok = mgr
            .decode_cached(index, |bytes| {
                calls.set(calls.get() + 1);
                Ok(bytes.len())
            })
            .unwrap();
        assert_eq!(*ok, 7);
        assert_eq!(calls.get(), 2);

        let stats = mgr.stats();
        assert_eq!(stats.misses, 2, "each attempt counts as a miss");
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.decodes, 1, "only the successful decode counts");
        assert_eq!(stats.bytes_decoded, 7);
    }

    #[test]
    fn out_of_range_lump_index_is_an_error_not_a_panic() {
        let mgr = manager_with(&[("A", b"a")], &[]);
        assert!(mgr.lump_data_index(1).is_err());
        assert!(mgr.decode_cached(1, |_| Ok(())).is_err());
    }

    #[test]
    fn patch_wad_overrides_iwad_through_the_manager() {
        let mgr = manager_with(
            &[("PLAYPAL", b"iwad-palette"), ("ONLYIWAD", b"iwad-only")],
            &[&[("PLAYPAL", b"pwad-palette")]],
        );

        assert_eq!(mgr.lump_data("PLAYPAL").unwrap(), b"pwad-palette");
        assert_eq!(mgr.lump_data("ONLYIWAD").unwrap(), b"iwad-only");
        let index = mgr.find_lump("PLAYPAL").unwrap();
        assert_eq!(index, 2, "the PWAD's lump wins the lookup");

        // the decode path sees the same override
        let decoded = mgr
            .decode_cached(index, |bytes| Ok(bytes.to_vec()))
            .unwrap();
        assert_eq!(&decoded[..], b"pwad-palette");
    }

    #[test]
    fn namespace_markers_delegate_to_the_stacked_set() {
        let mgr = manager_with(
            &[
                ("F_START", b""),
                ("FLAT1", b"f1"),
                ("F_END", b""),
                ("S_START", b""),
                ("SPRT1", b"s1"),
                ("S_END", b""),
            ],
            &[],
        );

        assert_eq!(mgr.first_flat(), Some(1));
        assert_eq!(mgr.last_flat(), Some(1));
        assert_eq!(mgr.first_sprite(), Some(4));
        assert_eq!(mgr.last_sprite(), Some(4));
    }

    #[test]
    fn stats_track_hits_misses_decodes_and_bytes_until_reset() {
        let mgr = manager_with(&[("A", b"12345")], &[]);
        let index = mgr.find_lump("A").unwrap();

        mgr.decode_cached(index, |bytes| Ok(bytes.len())).unwrap();
        mgr.decode_cached(index, |bytes| Ok(bytes.len())).unwrap();
        mgr.decode_cached(index, |bytes| Ok(bytes.len())).unwrap();

        let stats = mgr.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.decodes, 1);
        assert_eq!(stats.bytes_decoded, 5);

        mgr.reset_stats();
        assert_eq!(mgr.stats(), CacheStats::default());
    }

    #[test]
    fn cache_size_counts_across_types_and_clear_cache_forces_re_decode() {
        let mgr = manager_with(&[("A", b"data"), ("B", b"other")], &[]);
        let a = mgr.find_lump("A").unwrap();
        let b = mgr.find_lump("B").unwrap();

        mgr.decode_cached(a, |bytes| Ok(bytes.len())).unwrap();
        mgr.decode_cached(a, |bytes| Ok(bytes.to_vec())).unwrap();
        mgr.decode_cached(b, |bytes| Ok(bytes.len())).unwrap();
        assert_eq!(mgr.cache_size(), 3, "one entry per (type, lump) pair");

        mgr.clear_cache();
        assert_eq!(mgr.cache_size(), 0);

        mgr.decode_cached(a, |bytes| Ok(bytes.len())).unwrap();
        assert_eq!(mgr.stats().decodes, 4, "re-decoded after the clear");
        assert_eq!(mgr.cache_size(), 1);
    }

    #[test]
    fn empty_manager_fails_with_clear_errors() {
        let mgr = ResourceManager::default();
        let err = mgr.lump_data("PLAYPAL").unwrap_err();
        assert!(err.to_string().contains("PLAYPAL"), "got: {err}");
        assert!(mgr.lump_data_index(0).is_err());
        assert!(mgr.find_lump("PLAYPAL").is_none());
        assert_eq!(mgr.cache_size(), 0);
        assert_eq!(mgr.stats(), CacheStats::default());
        assert_eq!(mgr.first_flat(), None);
        assert_eq!(mgr.first_sprite(), None);

        // and decoding through the empty manager reports the same way
        assert!(mgr.decode_cached(0, |_| Ok(())).is_err());
    }

    #[test]
    fn from_paths_with_no_files_is_an_error() {
        let err = ResourceManager::from_paths(std::iter::empty::<PathBuf>()).unwrap_err();
        assert!(matches!(
            err,
            crate::errors::EngineError::Wad(crate::wad::WadError::NoWadsLoaded)
        ));
    }

    #[test]
    fn manager_debug_output_mentions_the_caches() {
        let mgr = manager_with(&[("A", b"data")], &[]);
        mgr.decode_cached(mgr.find_lump("A").unwrap(), |bytes| Ok(bytes.len()))
            .unwrap();
        let debug = format!("{mgr:?}");
        assert!(debug.contains("cache_entries: 1"), "got: {debug}");
    }
}
