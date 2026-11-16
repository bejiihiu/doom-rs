//! The switch on/off texture pair table, ported from `p_switch.c`
//! (`alphSwitchList`). Doom texture names are at most 8 characters.
//!
//! The vanilla terminator entry (`{"\0", "\0", 0}`) is not ported - it only
//! existed to sentinel-terminate the C array. Mapthing `episode` marks the
//! minimum game episode a switch appears in: 1 = shareware, 2 = registered,
//! 3 = commercial (Doom II). Resolution of the texture names to texture
//! indices happens at game startup (`P_InitSwitchList`, p_switch.c).

/// Max number of wall switches in a level (vanilla `MAXSWITCHES`, p_spec.h).
pub const MAXSWITCHES: usize = 50;
/// Max number of buttons pressed at once (vanilla `MAXBUTTONS`, p_spec.h).
pub const MAXBUTTONS: usize = 16;
/// Button lifetime: 1 second in ticks (vanilla `BUTTONTIME`, p_spec.h).
pub const BUTTONTIME: i32 = 35;

/// One row of vanilla `alphSwitchList` (`switchlist_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchListEntry {
    /// Texture to show while the switch is off (vanilla `name1`).
    pub name1: &'static str,
    /// Texture to show after it is flipped (vanilla `name2`).
    pub name2: &'static str,
    /// Minimum game episode the switch appears in (vanilla `episode`).
    pub episode: u16,
}

/// The full vanilla `alphSwitchList` table.
pub static SWITCH_LIST: &[SwitchListEntry] = &[
    SwitchListEntry {
        name1: "SW1BRCOM",
        name2: "SW2BRCOM",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1BRN1",
        name2: "SW2BRN1",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1BRN2",
        name2: "SW2BRN2",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1BRNGN",
        name2: "SW2BRNGN",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1BROWN",
        name2: "SW2BROWN",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1COMM",
        name2: "SW2COMM",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1COMP",
        name2: "SW2COMP",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1DIRT",
        name2: "SW2DIRT",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1EXIT",
        name2: "SW2EXIT",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1GRAY",
        name2: "SW2GRAY",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1GRAY1",
        name2: "SW2GRAY1",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1METAL",
        name2: "SW2METAL",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1PIPE",
        name2: "SW2PIPE",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1SLAD",
        name2: "SW2SLAD",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1STARG",
        name2: "SW2STARG",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1STON1",
        name2: "SW2STON1",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1STON2",
        name2: "SW2STON2",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1STONE",
        name2: "SW2STONE",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1STRTN",
        name2: "SW2STRTN",
        episode: 1,
    },
    SwitchListEntry {
        name1: "SW1BLUE",
        name2: "SW2BLUE",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1CMT",
        name2: "SW2CMT",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1GARG",
        name2: "SW2GARG",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1GSTON",
        name2: "SW2GSTON",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1HOT",
        name2: "SW2HOT",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1LION",
        name2: "SW2LION",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1SATYR",
        name2: "SW2SATYR",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1SKIN",
        name2: "SW2SKIN",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1VINE",
        name2: "SW2VINE",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1WOOD",
        name2: "SW2WOOD",
        episode: 2,
    },
    SwitchListEntry {
        name1: "SW1PANEL",
        name2: "SW2PANEL",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1ROCK",
        name2: "SW2ROCK",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1MET2",
        name2: "SW2MET2",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1WDMET",
        name2: "SW2WDMET",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1BRIK",
        name2: "SW2BRIK",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1MOD1",
        name2: "SW2MOD1",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1ZIM",
        name2: "SW2ZIM",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1STON6",
        name2: "SW2STON6",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1TEK",
        name2: "SW2TEK",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1MARB",
        name2: "SW2MARB",
        episode: 3,
    },
    SwitchListEntry {
        name1: "SW1SKULL",
        name2: "SW2SKULL",
        episode: 3,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_list_matches_vanilla_count() {
        assert_eq!(SWITCH_LIST.len(), 40);
        assert!(SWITCH_LIST.len() <= MAXSWITCHES);
    }

    #[test]
    fn switch_entries_are_well_formed() {
        for entry in SWITCH_LIST {
            assert!(!entry.name1.is_empty());
            assert!(!entry.name2.is_empty());
            assert!(entry.name1.len() <= 8);
            assert!(entry.name2.len() <= 8);
            assert_ne!(entry.name1, entry.name2);
            assert!((1..=3).contains(&entry.episode));
            for name in [entry.name1, entry.name2] {
                assert!(
                    name.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
                    "non-alphanumeric texture name {name}"
                );
            }
        }
        // episode groups are ordered like vanilla (shareware, registered, Doom II)
        let episodes: Vec<u16> = SWITCH_LIST.iter().map(|e| e.episode).collect();
        let mut sorted = episodes.clone();
        sorted.sort_unstable();
        assert_eq!(episodes, sorted);
        assert_eq!(episodes[0], 1);
        assert_eq!(*episodes.iter().max().unwrap(), 3);
    }

    #[test]
    fn button_constants_match_p_spec_h() {
        assert_eq!(MAXSWITCHES, 50);
        assert_eq!(MAXBUTTONS, 16);
        assert_eq!(BUTTONTIME, 35);
    }
}
