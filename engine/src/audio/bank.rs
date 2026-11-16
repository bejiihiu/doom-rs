//! Sound lump loading and caching.
//!
//! Vanilla cached every `S_sfx` entry at `I_InitSound()` time (skipping
//! linked variants, which share their parent's data) and fell back to
//! `DSPISTOL` when a lump was missing. [`SoundBank`] keeps the same shape:
//! [`SoundBank::preload`] decodes every non-linked sfx up front, resolves
//! linked variants to their parent's decoded data and applies the same
//! fallback rule. Decoding goes through the resource manager's typed
//! cache, so a given lump is decoded at most once per manager.
//!
//! The bank deliberately does not own the WAD: it borrows the
//! [`ResourceManager`] at load time and hands out `Arc<DmxSound>` shares
//! afterwards, so the engine layer stays free of resource plumbing.

use std::collections::HashMap;
use std::sync::Arc;

use crate::errors::{EngineError, EngineResult};
use crate::info::{SFX, SFX_COUNT, SfxId};
use crate::resources::ResourceManager;

use super::dmx::{DmxSound, decode_dmx_sound};
use super::error::{AudioError, AudioResult};

/// Upper bound on link-chain hops; vanilla linked at most once (chgun →
/// pistol), we iterate defensively against corrupt tables.
const MAX_LINK_DEPTH: usize = 8;

/// Lump name used as the fallback when a sound is missing from the WAD
/// (vanilla `getsfx()` substituted `dspistol` for any absent `ds*` lump).
const FALLBACK_LUMP: &str = "DSPISTOL";

/// Report returned by [`SoundBank::preload`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreloadReport {
    /// Number of distinct sfx entries loaded (non-linked).
    pub loaded: usize,
    /// Sfx whose own lump was missing and which fell back to
    /// [`FALLBACK_LUMP`].
    pub fell_back: Vec<SfxId>,
}

/// Decoded sound data for the vanilla `S_sfx` table.
#[derive(Default)]
pub struct SoundBank {
    sounds: HashMap<SfxId, Arc<DmxSound>>,
}

impl std::fmt::Debug for SoundBank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundBank")
            .field("loaded", &self.sounds.len())
            .finish()
    }
}

impl SoundBank {
    /// An empty bank; [`SoundBank::preload`] (or [`SoundBank::load`] for a
    /// single sfx) fills it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of sfx entries currently loaded.
    pub fn len(&self) -> usize {
        self.sounds.len()
    }

    /// True when nothing has been loaded yet.
    pub fn is_empty(&self) -> bool {
        self.sounds.is_empty()
    }

    /// Resolves an sfx id to the id whose lump actually carries the data
    /// (following `S_sfx.link`, e.g. `sfx_chgun` -> `sfx_pistol`).
    ///
    /// Returns the input id unchanged when it has no link.
    pub fn resolve_link(&self, sfx_id: SfxId) -> AudioResult<SfxId> {
        let mut current = sfx_id;
        for _ in 0..MAX_LINK_DEPTH {
            match SFX[current as usize].link {
                Some(next) => current = next,
                None => return Ok(current),
            }
        }
        Err(AudioError::LinkCycle(sfx_id.name()))
    }

    /// Looks up the decoded data for an sfx, resolving links.
    ///
    /// Does not touch the WAD; the sound must have been loaded first.
    pub fn get(&self, sfx_id: SfxId) -> Option<Arc<DmxSound>> {
        let target = resolve_link_pure(sfx_id)?;
        self.sounds.get(&target).cloned()
    }

    /// Loads (or fetches from the manager's cache) one sfx entry.
    ///
    /// Linked variants decode through their parent's lump; a missing lump
    /// falls back to `DSPISTOL` like vanilla. Decoding errors abort the
    /// load with the engine error unchanged.
    pub fn load(
        &mut self,
        resources: &ResourceManager,
        sfx_id: SfxId,
    ) -> EngineResult<Arc<DmxSound>> {
        let target = self.resolve_link(sfx_id).map_err(EngineError::from)?;
        if let Some(sound) = self.sounds.get(&target) {
            return Ok(sound.clone());
        }

        let lump_name = sfx_lump_name(target);
        let lump_index = resources
            .find_lump(&lump_name)
            .or_else(|| resources.find_lump(FALLBACK_LUMP))
            .ok_or_else(|| EngineError::from(AudioError::MissingSound(lump_name.clone())))?;

        let sound = resources.decode_cached::<DmxSound>(lump_index, |bytes| {
            decode_dmx_sound(bytes).map_err(EngineError::from)
        })?;
        self.sounds.insert(target, Arc::clone(&sound));
        Ok(sound)
    }

    /// Loads every non-linked sfx entry from the WAD, vanilla-style.
    ///
    /// Lump-missing entries fall back to `DSPISTOL`; a missing `DSPISTOL`
    /// aborts with an error (vanilla raised a fatal error in the same
    /// case). Decoding failures abort loudly - a silent missing sound is
    /// worse than a dead engine at startup.
    pub fn preload(&mut self, resources: &ResourceManager) -> EngineResult<PreloadReport> {
        let mut report = PreloadReport::default();
        for (index, row) in SFX.iter().enumerate().take(SFX_COUNT).skip(1) {
            let sfx_id = sfx_id_from_index(index);
            // linked variants share their parent's data; skip like vanilla
            if row.link.is_some() {
                continue;
            }
            let lump_name = sfx_lump_name(sfx_id);
            let had_own_lump = resources.find_lump(&lump_name).is_some();
            self.load(resources, sfx_id)?;
            if !had_own_lump {
                report.fell_back.push(sfx_id);
            }
            report.loaded += 1;
        }
        Ok(report)
    }
}

/// All `SfxId` variants in table order (`id as usize == position`).
///
/// The `S_sfx` table is dense and in enum order; this array pins that
/// mapping down without `unsafe` enum punning. The roundtrip test in this
/// module fails loudly if the tables ever drift apart.
const ALL_SFX_IDS: [SfxId; SFX_COUNT] = [
    SfxId::sfx_None,
    SfxId::sfx_pistol,
    SfxId::sfx_shotgn,
    SfxId::sfx_sgcock,
    SfxId::sfx_dshtgn,
    SfxId::sfx_dbopn,
    SfxId::sfx_dbcls,
    SfxId::sfx_dbload,
    SfxId::sfx_plasma,
    SfxId::sfx_bfg,
    SfxId::sfx_sawup,
    SfxId::sfx_sawidl,
    SfxId::sfx_sawful,
    SfxId::sfx_sawhit,
    SfxId::sfx_rlaunc,
    SfxId::sfx_rxplod,
    SfxId::sfx_firsht,
    SfxId::sfx_firxpl,
    SfxId::sfx_pstart,
    SfxId::sfx_pstop,
    SfxId::sfx_doropn,
    SfxId::sfx_dorcls,
    SfxId::sfx_stnmov,
    SfxId::sfx_swtchn,
    SfxId::sfx_swtchx,
    SfxId::sfx_plpain,
    SfxId::sfx_dmpain,
    SfxId::sfx_popain,
    SfxId::sfx_vipain,
    SfxId::sfx_mnpain,
    SfxId::sfx_pepain,
    SfxId::sfx_slop,
    SfxId::sfx_itemup,
    SfxId::sfx_wpnup,
    SfxId::sfx_oof,
    SfxId::sfx_telept,
    SfxId::sfx_posit1,
    SfxId::sfx_posit2,
    SfxId::sfx_posit3,
    SfxId::sfx_bgsit1,
    SfxId::sfx_bgsit2,
    SfxId::sfx_sgtsit,
    SfxId::sfx_cacsit,
    SfxId::sfx_brssit,
    SfxId::sfx_cybsit,
    SfxId::sfx_spisit,
    SfxId::sfx_bspsit,
    SfxId::sfx_kntsit,
    SfxId::sfx_vilsit,
    SfxId::sfx_mansit,
    SfxId::sfx_pesit,
    SfxId::sfx_sklatk,
    SfxId::sfx_sgtatk,
    SfxId::sfx_skepch,
    SfxId::sfx_vilatk,
    SfxId::sfx_claw,
    SfxId::sfx_skeswg,
    SfxId::sfx_pldeth,
    SfxId::sfx_pdiehi,
    SfxId::sfx_podth1,
    SfxId::sfx_podth2,
    SfxId::sfx_podth3,
    SfxId::sfx_bgdth1,
    SfxId::sfx_bgdth2,
    SfxId::sfx_sgtdth,
    SfxId::sfx_cacdth,
    SfxId::sfx_skldth,
    SfxId::sfx_brsdth,
    SfxId::sfx_cybdth,
    SfxId::sfx_spidth,
    SfxId::sfx_bspdth,
    SfxId::sfx_vildth,
    SfxId::sfx_kntdth,
    SfxId::sfx_pedth,
    SfxId::sfx_skedth,
    SfxId::sfx_posact,
    SfxId::sfx_bgact,
    SfxId::sfx_dmact,
    SfxId::sfx_bspact,
    SfxId::sfx_bspwlk,
    SfxId::sfx_vilact,
    SfxId::sfx_noway,
    SfxId::sfx_barexp,
    SfxId::sfx_punch,
    SfxId::sfx_hoof,
    SfxId::sfx_metal,
    SfxId::sfx_chgun,
    SfxId::sfx_tink,
    SfxId::sfx_bdopn,
    SfxId::sfx_bdcls,
    SfxId::sfx_itmbk,
    SfxId::sfx_flame,
    SfxId::sfx_flamst,
    SfxId::sfx_getpow,
    SfxId::sfx_bospit,
    SfxId::sfx_boscub,
    SfxId::sfx_bossit,
    SfxId::sfx_bospn,
    SfxId::sfx_bosdth,
    SfxId::sfx_manatk,
    SfxId::sfx_mandth,
    SfxId::sfx_sssit,
    SfxId::sfx_ssdth,
    SfxId::sfx_keenpn,
    SfxId::sfx_keendt,
    SfxId::sfx_skeact,
    SfxId::sfx_skesit,
    SfxId::sfx_skeatk,
    SfxId::sfx_radio,
];

/// `SfxId` from a raw table index, used by [`SoundBank::preload`].
fn sfx_id_from_index(index: usize) -> SfxId {
    ALL_SFX_IDS[index]
}

/// Pure link resolution without the depth-cap error plumbing.
fn resolve_link_pure(sfx_id: SfxId) -> Option<SfxId> {
    let mut current = sfx_id;
    for _ in 0..MAX_LINK_DEPTH {
        match SFX[current as usize].link {
            Some(next) => current = next,
            None => return Some(current),
        }
    }
    None
}

/// `DS` + uppercased name, the WAD lump a sfx resolves to (vanilla
/// `I_GetSfxLumpNum`).
fn sfx_lump_name(sfx_id: SfxId) -> String {
    let bytes = sfx_id.lump_name();
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    fn lump_bytes(tag: u16, rate: u16, samples: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&tag.to_le_bytes());
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        data.extend_from_slice(samples);
        data
    }

    fn manager_with(lumps: &[(&str, &[u8])]) -> ResourceManager {
        let wad = WadFile::from_bytes(build_wad(b"IWAD", lumps)).unwrap();
        ResourceManager::new(wad, Vec::new())
    }

    #[test]
    fn resolves_link_to_parent_id() {
        let bank = SoundBank::new();
        assert_eq!(
            bank.resolve_link(SfxId::sfx_chgun).unwrap(),
            SfxId::sfx_pistol
        );
        assert_eq!(
            bank.resolve_link(SfxId::sfx_pistol).unwrap(),
            SfxId::sfx_pistol
        );
    }

    #[test]
    fn linked_sfx_decodes_through_parent_lump() {
        let manager = manager_with(&[("DSPISTOL", &lump_bytes(3, 11025, &[10, 20, 30]))]);
        let mut bank = SoundBank::new();
        let pistol = bank.load(&manager, SfxId::sfx_pistol).unwrap();
        let chgun = bank.load(&manager, SfxId::sfx_chgun).unwrap();
        assert!(
            Arc::ptr_eq(&pistol, &chgun),
            "linked sfx shares the parent's data"
        );
    }

    #[test]
    fn missing_lump_falls_back_to_dspistol() {
        let manager = manager_with(&[("DSPISTOL", &lump_bytes(3, 11025, &[1, 2, 3]))]);
        let mut bank = SoundBank::new();
        // no DSBFG in the WAD: falls back to DSPISTOL like vanilla
        let sound = bank.load(&manager, SfxId::sfx_bfg).unwrap();
        assert_eq!(
            sound.samples,
            lump_bytes(3, 11025, &[1, 2, 3])[8..]
                .iter()
                .map(|&b| ((i32::from(b) - 128) << 8) as i16)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn preload_reports_fallbacks_and_loads_everything() {
        // pistol is the only real lump; everything else falls back
        let manager = manager_with(&[("DSPISTOL", &lump_bytes(3, 11025, &[9, 8, 7]))]);
        let mut bank = SoundBank::new();
        // ids 1..=108 = 108 sfx, minus chgun (linked, shares pistol)
        let report = bank.preload(&manager).unwrap();
        assert_eq!(report.loaded, SFX_COUNT - 2);
        assert!(report.fell_back.contains(&SfxId::sfx_bfg));
        assert!(!report.fell_back.contains(&SfxId::sfx_pistol));
        assert!(bank.get(SfxId::sfx_chgun).is_some());
    }

    #[test]
    fn preload_without_any_lumps_fails_loudly() {
        let manager = manager_with(&[("DUMMY", b"not audio")]);
        let mut bank = SoundBank::new();
        let err = bank.preload(&manager).unwrap_err();
        assert!(err.to_string().contains("DSPISTOL"));
    }

    #[test]
    fn lump_names_are_vanilla_format() {
        assert_eq!(sfx_lump_name(SfxId::sfx_pistol), "DSPISTOL");
        assert_eq!(sfx_lump_name(SfxId::sfx_chgun), "DSCHGUN");
        assert_eq!(sfx_lump_name(SfxId::sfx_None), "DSNONE");
    }

    #[test]
    fn sfx_id_from_index_covers_the_whole_table() {
        for index in 1..SFX_COUNT {
            let id = sfx_id_from_index(index);
            assert_eq!(id as usize, index);
        }
    }
}
