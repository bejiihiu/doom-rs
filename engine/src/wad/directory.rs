//! Stacking WADs into one global lump namespace.
//!
//! Vanilla loads the IWAD first, then any PWADs, into a single flat
//! namespace; the name lookup scans backwards so later files override
//! earlier ones. This module keeps the same rules, plus one difference:
//! nothing here is a global. [`WadSet`] owns the files and the combined
//! directory, so lookups are methods on an explicit value and the whole
//! thing is testable without global state.
//!
//! Lump names stay unique-looking but are not deduplicated: every lump from
//! every WAD gets an entry, and the *last* matching entry wins on lookup,
//! exactly like vanilla.

use std::path::{Path, PathBuf};

use super::errors::{WadError, WadResult};
use super::reader::{LumpName, WadFile};

/// A lump in the combined namespace of a [`WadSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LumpEntry {
    /// Normalized lump name.
    pub name: LumpName,
    /// Index of the WAD this lump came from, in add order.
    pub wad_index: usize,
    /// Index of the lump within that WAD's own directory.
    pub lump_index: usize,
    /// Offset of the lump's data within the WAD file it came from.
    pub file_pos: usize,
    /// Size of the lump in bytes.
    pub size: usize,
}

/// Several WADs combined into a single global namespace.
///
/// The order of [`WadSet::add_wad`] calls is the load order: an IWAD first,
/// then any patches. When two WADs (or two lumps inside one WAD) define the
/// same lump name, the last one added wins — that is vanilla behavior.
///
/// Namespace markers (`F_START`/`F_END`, `S_START`/`S_END`) are located
/// after each add and recorded as global indices into the combined
/// namespace, mirroring how vanilla derives `firstflat`/`lastflat` and
/// `firstspritelump`/`lastspritelump`. They are `None` when the markers are
/// missing or nonsensical (e.g. `F_END` at index 0).
#[derive(Debug)]
pub struct WadSet {
    wads: Vec<WadFile>,
    entries: Vec<LumpEntry>,
    first_flat: Option<usize>,
    last_flat: Option<usize>,
    first_sprite: Option<usize>,
    last_sprite: Option<usize>,
}

impl Default for WadSet {
    fn default() -> Self {
        Self::new()
    }
}

impl WadSet {
    /// An empty set. Use [`WadSet::add_wad`] to stack files into it, or
    /// [`WadSet::from_paths`] to load a list of files in one shot.
    pub fn new() -> Self {
        Self {
            wads: Vec::new(),
            entries: Vec::new(),
            first_flat: None,
            last_flat: None,
            first_sprite: None,
            last_sprite: None,
        }
    }

    /// Adds a parsed WAD to the set (after the ones already present).
    ///
    /// Returns the index of the WAD in [`wads`](Self::wads). The combined
    /// directory is rebuilt incrementally and namespace markers are
    /// refreshed, so the set is always consistent after this call.
    pub fn add_wad(&mut self, wad: WadFile) -> usize {
        let wad_index = self.wads.len();
        self.entries.reserve(wad.lump_count());
        for (lump_index, entry) in wad.entries().iter().enumerate() {
            self.entries.push(LumpEntry {
                name: entry.name,
                wad_index,
                lump_index,
                file_pos: entry.file_pos,
                size: entry.size,
            });
        }
        self.wads.push(wad);
        self.refresh_namespaces();
        wad_index
    }

    /// Parses `bytes` as a WAD and adds it to the set. Convenience for
    /// [`WadFile::from_bytes`] + [`WadSet::add_wad`].
    pub fn add_wad_bytes(&mut self, bytes: Vec<u8>) -> WadResult<usize> {
        Ok(self.add_wad(WadFile::from_bytes(bytes)?))
    }

    /// Parses `path` as a WAD and adds it to the set. Convenience for
    /// [`WadFile::open`] + [`WadSet::add_wad`].
    pub fn add_wad_file(&mut self, path: impl AsRef<Path>) -> WadResult<usize> {
        Ok(self.add_wad(WadFile::open(path)?))
    }

    /// Loads a series of WADs in order into a new set.
    ///
    /// Fails with [`WadError::NoWadsLoaded`] if `paths` is empty, matching
    /// vanilla's refusal to start with no files at all.
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> WadResult<Self> {
        let mut set = Self::new();
        let mut loaded = 0usize;
        for path in paths {
            set.add_wad_file(path)?;
            loaded += 1;
        }
        if loaded == 0 {
            return Err(WadError::NoWadsLoaded);
        }
        Ok(set)
    }

    /// The stacked WADs, in load order.
    pub fn wads(&self) -> &[WadFile] {
        &self.wads
    }

    /// Number of WADs in the set.
    pub fn wad_count(&self) -> usize {
        self.wads.len()
    }

    /// The combined directory: every lump from every WAD, in load order.
    pub fn entries(&self) -> &[LumpEntry] {
        &self.entries
    }

    /// Total number of lumps across all WADs.
    pub fn lump_count(&self) -> usize {
        self.entries.len()
    }

    /// True when no WADs have been added yet.
    pub fn is_empty(&self) -> bool {
        self.wads.is_empty()
    }

    /// Finds the lump with `name`, searching from the end of the directory.
    ///
    /// This is vanilla `W_CheckNumForName`: later files (and later lumps
    /// within a file) override earlier ones, so a patch WAD's `PLAYPAL`
    /// beats the IWAD's. Returns the global index into
    /// [`entries`](Self::entries).
    pub fn find_lump(&self, name: &str) -> Option<usize> {
        let name = LumpName::from(name);
        self.entries.iter().rposition(|e| e.name == name)
    }

    /// Like [`find_lump`](Self::find_lump), but fails when the lump does not
    /// exist. Vanilla `W_GetNumForName` aborts the program in that case;
    /// here it is a regular error.
    pub fn get_lump(&self, name: &str) -> WadResult<usize> {
        self.find_lump(name).ok_or_else(|| WadError::LumpNotFound {
            name: name.to_string(),
        })
    }

    /// Raw bytes of the lump at global index `index`.
    pub fn lump_data(&self, index: usize) -> WadResult<&[u8]> {
        let entry = self
            .entries
            .get(index)
            .ok_or(WadError::LumpIndexOutOfBounds {
                index,
                len: self.entries.len(),
            })?;
        self.wads[entry.wad_index].lump(entry.lump_index)
    }

    /// Size of the lump at global index `index`.
    pub fn lump_size(&self, index: usize) -> WadResult<usize> {
        self.entries
            .get(index)
            .map(|e| e.size)
            .ok_or(WadError::LumpIndexOutOfBounds {
                index,
                len: self.entries.len(),
            })
    }

    /// Raw bytes of the lump with `name`; see [`find_lump`](Self::find_lump)
    /// for the override rule and [`get_lump`](Self::get_lump) for the error.
    pub fn lump_data_named(&self, name: &str) -> WadResult<&[u8]> {
        let index = self.get_lump(name)?;
        self.lump_data(index)
    }

    /// Global index of the first flat (`F_START` + 1), if `F_START` exists.
    pub fn first_flat(&self) -> Option<usize> {
        self.first_flat
    }

    /// Global index of the last flat (`F_END` - 1), if `F_END` exists.
    pub fn last_flat(&self) -> Option<usize> {
        self.last_flat
    }

    /// Global index of the first sprite (`S_START` + 1), if `S_START` exists.
    pub fn first_sprite(&self) -> Option<usize> {
        self.first_sprite
    }

    /// Global index of the last sprite (`S_END` - 1), if `S_END` exists.
    pub fn last_sprite(&self) -> Option<usize> {
        self.last_sprite
    }

    fn refresh_namespaces(&mut self) {
        // checked_sub because a marker at index 0 leaves no room for a
        // namespace before it; treat that as "no marker" rather than
        // wrapping around to usize::MAX.
        self.first_flat = self.find_lump("F_START").map(|i| i + 1);
        self.last_flat = self.find_lump("F_END").and_then(|i| i.checked_sub(1));
        self.first_sprite = self.find_lump("S_START").map(|i| i + 1);
        self.last_sprite = self.find_lump("S_END").and_then(|i| i.checked_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wad::testutil::build_wad;

    fn add_bytes(set: &mut WadSet, magic: &[u8; 4], lumps: &[(&str, &[u8])]) -> usize {
        set.add_wad_bytes(build_wad(magic, lumps)).unwrap()
    }

    #[test]
    fn stacking_two_wads_lets_the_later_one_win() {
        let mut set = WadSet::new();
        add_bytes(&mut set, b"IWAD", &[("PLAYPAL", b"iwad-palette")]);
        add_bytes(&mut set, b"PWAD", &[("PLAYPAL", b"pwad-palette")]);

        assert_eq!(set.lump_count(), 2);
        assert_eq!(set.lump_data_named("PLAYPAL").unwrap(), b"pwad-palette");

        // the combined directory keeps both, in load order
        assert_eq!(set.entries()[0].wad_index, 0);
        assert_eq!(set.entries()[1].wad_index, 1);
        assert_eq!(set.entries()[0].name, LumpName::from("PLAYPAL"));
        assert_eq!(set.entries()[1].name, LumpName::from("PLAYPAL"));
    }

    #[test]
    fn find_lump_scans_from_the_end_across_wads() {
        let mut set = WadSet::new();
        add_bytes(
            &mut set,
            b"IWAD",
            &[("MAP01", b"iwad map"), ("COMMON", b"c0")],
        );
        add_bytes(&mut set, b"PWAD", &[("COMMON", b"c1")]);

        // global indices: 0..=1 from the IWAD, 2 from the PWAD
        assert_eq!(set.find_lump("MAP01"), Some(0));
        assert_eq!(set.find_lump("COMMON"), Some(2));
        assert_eq!(set.lump_data(2).unwrap(), b"c1");
        assert_eq!(set.lump_data(1).unwrap(), b"c0");
    }

    #[test]
    fn stacking_preserves_load_order_and_local_indices() {
        let mut set = WadSet::new();
        add_bytes(&mut set, b"IWAD", &[("A", b"a"), ("B", b"b")]);
        add_bytes(&mut set, b"PWAD", &[("C", b"c")]);

        let entries = set.entries();
        assert_eq!(entries[0].wad_index, 0);
        assert_eq!(entries[0].lump_index, 0);
        assert_eq!(entries[1].wad_index, 0);
        assert_eq!(entries[1].lump_index, 1);
        assert_eq!(entries[2].wad_index, 1);
        assert_eq!(entries[2].lump_index, 0);

        assert_eq!(set.wad_count(), 2);
        assert_eq!(set.wads()[0].kind(), crate::wad::WadKind::Iwad);
        assert_eq!(set.wads()[1].kind(), crate::wad::WadKind::Pwad);
    }

    #[test]
    fn namespace_markers_point_inside_the_combined_directory() {
        let mut set = WadSet::new();
        add_bytes(
            &mut set,
            b"IWAD",
            &[
                ("F_START", b""),
                ("FLAT1", b"f1"),
                ("FLAT2", b"f2"),
                ("F_END", b""),
                ("S_START", b""),
                ("SPRT1", b"s1"),
                ("S_END", b""),
            ],
        );

        assert_eq!(set.first_flat(), Some(1));
        assert_eq!(set.last_flat(), Some(2));
        assert_eq!(set.first_sprite(), Some(5));
        assert_eq!(set.last_sprite(), Some(5));
    }

    #[test]
    fn namespace_markers_are_none_when_missing() {
        let mut set = WadSet::new();
        add_bytes(&mut set, b"IWAD", &[("FLAT1", b"f1")]);
        assert_eq!(set.first_flat(), None);
        assert_eq!(set.last_flat(), None);
        assert_eq!(set.first_sprite(), None);
        assert_eq!(set.last_sprite(), None);
    }

    #[test]
    fn markers_in_a_later_wad_override_the_iwad() {
        // backward search means the PWAD's F_START/F_END win, like vanilla
        let mut set = WadSet::new();
        add_bytes(
            &mut set,
            b"IWAD",
            &[("F_START", b""), ("FLAT1", b"f1"), ("F_END", b"")],
        );
        add_bytes(
            &mut set,
            b"PWAD",
            &[("F_START", b""), ("FLATPWAD", b"fp"), ("F_END", b"")],
        );

        assert_eq!(set.first_flat(), Some(4)); // PWAD's F_START + 1
        assert_eq!(set.last_flat(), Some(4)); // PWAD's F_END - 1
    }

    #[test]
    fn empty_set_reports_absence_instead_of_panicking() {
        let set = WadSet::new();
        assert!(set.is_empty());
        assert_eq!(set.lump_count(), 0);
        assert_eq!(set.find_lump("PLAYPAL"), None);
        assert!(matches!(
            set.get_lump("PLAYPAL").unwrap_err(),
            WadError::LumpNotFound { .. }
        ));
        assert!(matches!(
            set.lump_data(0).unwrap_err(),
            WadError::LumpIndexOutOfBounds { index: 0, len: 0 }
        ));
        assert!(matches!(
            set.lump_size(0).unwrap_err(),
            WadError::LumpIndexOutOfBounds { .. }
        ));
        assert_eq!(set.first_flat(), None);
    }

    #[test]
    fn from_paths_with_no_files_is_an_error() {
        let err = WadSet::from_paths(Vec::new()).unwrap_err();
        assert!(matches!(err, WadError::NoWadsLoaded));
    }

    #[test]
    fn lump_size_matches_the_payload() {
        let mut set = WadSet::new();
        add_bytes(&mut set, b"IWAD", &[("A", b"12345")]);
        assert_eq!(set.lump_size(0).unwrap(), 5);
    }

    #[test]
    fn lump_data_comes_straight_from_the_owning_wad() {
        let mut set = WadSet::new();
        add_bytes(&mut set, b"IWAD", &[("A", b"iwad-a")]);
        add_bytes(&mut set, b"PWAD", &[("B", b"pwad-b")]);

        // indices 0 (IWAD) and 1 (PWAD) map into different backing buffers
        assert_eq!(set.lump_data(0).unwrap(), b"iwad-a");
        assert_eq!(set.lump_data(1).unwrap(), b"pwad-b");
        assert!(std::ptr::eq(
            set.lump_data(0).unwrap().as_ptr(),
            set.wads()[0].lump(0).unwrap().as_ptr()
        ));
        assert!(std::ptr::eq(
            set.lump_data(1).unwrap().as_ptr(),
            set.wads()[1].lump(0).unwrap().as_ptr()
        ));
    }

    #[test]
    fn get_lump_reports_missing_names() {
        let mut set = WadSet::new();
        add_bytes(&mut set, b"IWAD", &[("A", b"a")]);
        let err = set.get_lump("NOPE").unwrap_err();
        assert_eq!(err.to_string(), "lump 'NOPE' not found");
    }

    #[test]
    fn default_set_is_empty_and_usable() {
        let set = WadSet::default();
        assert!(set.is_empty());
        assert_eq!(set.wads().len(), 0);
    }
}
