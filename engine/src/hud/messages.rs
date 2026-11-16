//! The message and level-name strings from `d_englsh.h`, ported 1:1.
//!
//! The original engine assigns plain `char*`s to `players[n].message`;
//! there is no enum there. Rust gets an enum so the compiler guarantees
//! every message the game layer can produce has a displayable string.
//! [`HudMessage::ALL`] lists every variant and [`HudMessage::as_str`] is an
//! exhaustive `match`, so a variant without a string cannot compile.
//!
//! The map-name tables back the level title the automap shows
//! (`HU_TITLE*` in `hu_stuff.c`). The raw `#define` names from the
//! original are kept as comments so the port can be cross-checked
//! string by string.

/// Every static string that can end up in `players[n].message` in
/// linuxdoom-1.10, plus the level-name tables and the chat strings.
/// Strings assigned to `plr->message` but formatted at runtime (the
/// `mypos` cheat, `"Marked Spot %d"`) are *not* enum variants; they are
/// produced with [`marked_spot_message`] and friends by the game layer.
macro_rules! hud_messages {
    ($($variant:ident => $text:literal),* $(,)?) => {
        /// A player message that can be shown in the heads-up display.
        ///
        /// `None` maps to the vanilla `plr->message == NULL` state; it
        /// renders as an empty string. Ordering of the variants is stable
        /// (the discriminants are only meaningful as indices into
        /// [`HudMessage::ALL`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum HudMessage {
            /// No message pending (vanilla `plr->message == NULL`).
            None,
            $($variant,)*
        }

        impl HudMessage {
            /// Every variant, in declaration order. Useful for
            /// exhaustiveness tests.
            pub const ALL: &'static [HudMessage] = &[HudMessage::None, $(HudMessage::$variant),*];

            /// The English text for this message (`d_englsh.h`).
            pub fn as_str(self) -> &'static str {
                match self {
                    HudMessage::None => "",
                    $(HudMessage::$variant => $text,)*
                }
            }
        }
    };
}

hud_messages! {
    // P_inter.C
    GotArmor => "Picked up the armor.", // GOTARMOR
    GotMega => "Picked up the MegaArmor!", // GOTMEGA
    GotHtHBonus => "Picked up a health bonus.", // GOTHTHBONUS
    GotArmBonus => "Picked up an armor bonus.", // GOTARMBONUS
    GotStim => "Picked up a stimpack.", // GOTSTIM
    GotMediNeed => "Picked up a medikit that you REALLY need!", // GOTMEDINEED
    GotMedikit => "Picked up a medikit.", // GOTMEDIKIT
    GotSuper => "Supercharge!", // GOTSUPER
    GotMegaSphere => "MegaSphere!", // GOTMSPHERE
    GotBlueCard => "Picked up a blue keycard.", // GOTBLUECARD
    GotYellowCard => "Picked up a yellow keycard.", // GOTYELWCARD
    GotRedCard => "Picked up a red keycard.", // GOTREDCARD
    GotBlueSkull => "Picked up a blue skull key.", // GOTBLUESKUL
    GotYellowSkull => "Picked up a yellow skull key.", // GOTYELWSKUL
    GotRedSkull => "Picked up a red skull key.", // GOTREDSKULL
    GotInvul => "Invulnerability!", // GOTINVUL
    GotBerserk => "Berserk!", // GOTBERSERK
    GotInvis => "Partial Invisibility", // GOTINVIS
    GotSuit => "Radiation Shielding Suit", // GOTSUIT
    GotMap => "Computer Area Map", // GOTMAP
    GotVisor => "Light Amplification Visor", // GOTVISOR
    GotClip => "Picked up a clip.", // GOTCLIP
    GotClipBox => "Picked up a box of bullets.", // GOTCLIPBOX
    GotRocket => "Picked up a rocket.", // GOTROCKET
    GotRockBox => "Picked up a box of rockets.", // GOTROCKBOX
    GotCell => "Picked up an energy cell.", // GOTCELL
    GotCellBox => "Picked up an energy cell pack.", // GOTCELLBOX
    GotShells => "Picked up 4 shotgun shells.", // GOTSHELLS
    GotShellBox => "Picked up a box of shotgun shells.", // GOTSHELLBOX
    GotBackpack => "Picked up a backpack full of ammo!", // GOTBACKPACK
    GotBfg9000 => "You got the BFG9000!  Oh, yes.", // GOTBFG9000
    GotChaingun => "You got the chaingun!", // GOTCHAINGUN
    GotChainsaw => "A chainsaw!  Find some meat!", // GOTCHAINSAW
    GotLauncher => "You got the rocket launcher!", // GOTLAUNCHER
    GotPlasma => "You got the plasma gun!", // GOTPLASMA
    GotShotgun => "You got the shotgun!", // GOTSHOTGUN
    GotShotgun2 => "You got the super shotgun!", // GOTSHOTGUN2
    // P_Doors.C
    PdBlueO => "You need a blue key to activate this object", // PD_BLUEO
    PdRedO => "You need a red key to activate this object", // PD_REDO
    PdYellowO => "You need a yellow key to activate this object", // PD_YELLOWO
    PdBlueK => "You need a blue key to open this door", // PD_BLUEK
    PdRedK => "You need a red key to open this door", // PD_REDK
    PdYellowK => "You need a yellow key to open this door", // PD_YELLOWK
    // G_game.C
    GgSaved => "game saved.", // GGSAVED
    // HU_stuff.C
    MsgUnsent => "[Message unsent]", // HUSTR_MSGU
    TalkToSelf1 => "You mumble to yourself", // HUSTR_TALKTOSELF1
    TalkToSelf2 => "Who's there?", // HUSTR_TALKTOSELF2
    TalkToSelf3 => "You scare yourself", // HUSTR_TALKTOSELF3
    TalkToSelf4 => "You start to rave", // HUSTR_TALKTOSELF4
    TalkToSelf5 => "You've lost it...", // HUSTR_TALKTOSELF5
    MsgSent => "[Message Sent]", // HUSTR_MESSAGESENT
    // M_Menu.C (the messages-on/off toggle forces these through)
    MsgOff => "Messages OFF", // MSGOFF
    MsgOn => "Messages ON", // MSGON
    // AM_map.C
    FollowOn => "Follow Mode ON", // AMSTR_FOLLOWON
    FollowOff => "Follow Mode OFF", // AMSTR_FOLLOWOFF
    GridOn => "Grid ON", // AMSTR_GRIDON
    GridOff => "Grid OFF", // AMSTR_GRIDOFF
    MarkedSpot => "Marked Spot", // AMSTR_MARKEDSPOT
    MarksCleared => "All Marks Cleared", // AMSTR_MARKSCLEARED
    // ST_stuff.C
    StMusicChange => "Music Change", // STSTR_MUS
    StNoMusic => "IMPOSSIBLE SELECTION", // STSTR_NOMUS
    StDqModeOn => "Degreelessness Mode On", // STSTR_DQDON
    StDqModeOff => "Degreelessness Mode Off", // STSTR_DQDOFF
    StKfaAdded => "Very Happy Ammo Added", // STSTR_KFAADDED
    StFaAdded => "Ammo (no keys) Added", // STSTR_FAADDED
    StNoClipOn => "No Clipping Mode ON", // STSTR_NCON
    StNoClipOff => "No Clipping Mode OFF", // STSTR_NCOFF
    StBehold => "inVuln, Str, Inviso, Rad, Allmap, or Lite-amp", // STSTR_BEHOLD
    StBeholdX => "Power-up Toggled", // STSTR_BEHOLDX
    StChoppers => "... doesn't suck - GM", // STSTR_CHOPPERS
    StClev => "Changing Level...", // STSTR_CLEV
}

/// Formats the automap's "Marked Spot %d" message (`AMSTR_MARKEDSPOT`,
/// `sprintf(buffer, "%s %d", AMSTR_MARKEDSPOT, markpointnum)` in
/// `am_map.c`).
pub fn marked_spot_message(mark_point_num: i32) -> String {
    format!("{} {}", HudMessage::MarkedSpot.as_str(), mark_point_num)
}

/// Episode-pack map names (`HUSTR_E1M1..E4M9`). The last nine entries are
/// the vanilla `NEWLEVEL` fillers that pad `mapnames[]` to 45 slots.
pub const MAPNAMES_EPISODE: [&str; 45] = [
    "E1M1: Hangar",
    "E1M2: Nuclear Plant",
    "E1M3: Toxin Refinery",
    "E1M4: Command Control",
    "E1M5: Phobos Lab",
    "E1M6: Central Processing",
    "E1M7: Computer Station",
    "E1M8: Phobos Anomaly",
    "E1M9: Military Base",
    "E2M1: Deimos Anomaly",
    "E2M2: Containment Area",
    "E2M3: Refinery",
    "E2M4: Deimos Lab",
    "E2M5: Command Center",
    "E2M6: Halls of the Damned",
    "E2M7: Spawning Vats",
    "E2M8: Tower of Babel",
    "E2M9: Fortress of Mystery",
    "E3M1: Hell Keep",
    "E3M2: Slough of Despair",
    "E3M3: Pandemonium",
    "E3M4: House of Pain",
    "E3M5: Unholy Cathedral",
    "E3M6: Mt. Erebus",
    "E3M7: Limbo",
    "E3M8: Dis",
    "E3M9: Warrens",
    "E4M1: Hell Beneath",
    "E4M2: Perfect Hatred",
    "E4M3: Sever The Wicked",
    "E4M4: Unruly Evil",
    "E4M5: They Will Repent",
    "E4M6: Against Thee Wickedly",
    "E4M7: And Hell Followed",
    "E4M8: Unto The Cruel",
    "E4M9: Fear",
    "NEWLEVEL",
    "NEWLEVEL",
    "NEWLEVEL",
    "NEWLEVEL",
    "NEWLEVEL",
    "NEWLEVEL",
    "NEWLEVEL",
    "NEWLEVEL",
    "NEWLEVEL",
];

/// Doom 2 map names (`HUSTR_1..HUSTR_32`, the `mapnames2[]` table).
pub const MAPNAMES_DOOM2: [&str; 32] = [
    "level 1: entryway",
    "level 2: underhalls",
    "level 3: the gantlet",
    "level 4: the focus",
    "level 5: the waste tunnels",
    "level 6: the crusher",
    "level 7: dead simple",
    "level 8: tricks and traps",
    "level 9: the pit",
    "level 10: refueling base",
    "level 11: 'o' of destruction!",
    "level 12: the factory",
    "level 13: downtown",
    "level 14: the inmost dens",
    "level 15: industrial zone",
    "level 16: suburbs",
    "level 17: tenements",
    "level 18: the courtyard",
    "level 19: the citadel",
    "level 20: gotcha!",
    "level 21: nirvana",
    "level 22: the catacombs",
    "level 23: barrels o' fun",
    "level 24: the chasm",
    "level 25: bloodfalls",
    "level 26: the abandoned mines",
    "level 27: monster condo",
    "level 28: the spirit world",
    "level 29: the living end",
    "level 30: icon of sin",
    "level 31: wolfenstein",
    "level 32: grosse",
];

/// Plutonia map names (`PHUSTR_1..PHUSTR_32`, `mapnamesp[]`). The vanilla
/// code path for these is `#if 0`-style dead in 1.10; the table is
/// ported because it is part of the shipped strings.
pub const MAPNAMES_PLUTONIA: [&str; 32] = [
    "level 1: congo",
    "level 2: well of souls",
    "level 3: aztec",
    "level 4: caged",
    "level 5: ghost town",
    "level 6: baron's lair",
    "level 7: caughtyard",
    "level 8: realm",
    "level 9: abattoire",
    "level 10: onslaught",
    "level 11: hunted",
    "level 12: speed",
    "level 13: the crypt",
    "level 14: genesis",
    "level 15: the twilight",
    "level 16: the omen",
    "level 17: compound",
    "level 18: neurosphere",
    "level 19: nme",
    "level 20: the death domain",
    "level 21: slayer",
    "level 22: impossible mission",
    "level 23: tombstone",
    "level 24: the final frontier",
    "level 25: the temple of darkness",
    "level 26: bunker",
    "level 27: anti-christ",
    "level 28: the sewers",
    "level 29: odyssey of noises",
    "level 30: the gateway of hell",
    "level 31: cyberden",
    "level 32: go 2 it",
];

/// TNT: Evilution map names (`THUSTR_1..THUSTR_32`, `mapnamest[]`). Also
/// unselected in 1.10; shipped strings, ported.
pub const MAPNAMES_TNT: [&str; 32] = [
    "level 1: system control",
    "level 2: human bbq",
    "level 3: power control",
    "level 4: wormhole",
    "level 5: hanger",
    "level 6: open season",
    "level 7: prison",
    "level 8: metal",
    "level 9: stronghold",
    "level 10: redemption",
    "level 11: storage facility",
    "level 12: crater",
    "level 13: nukage processing",
    "level 14: steel works",
    "level 15: dead zone",
    "level 16: deepest reaches",
    "level 17: processing area",
    "level 18: mill",
    "level 19: shipping/respawning",
    "level 20: central processing",
    "level 21: administration center",
    "level 22: habitat",
    "level 23: lunar mining project",
    "level 24: quarry",
    "level 25: baron's den",
    "level 26: ballistyx",
    "level 27: mount pain",
    "level 28: heck",
    "level 29: river styx",
    "level 30: last call",
    "level 31: pharaoh",
    "level 32: caribbean",
];

/// Which name table feeds the level title (`HU_TITLE*` in the original).
///
/// Vanilla selects `MAPNAMES_EPISODE` for the shareware/registered/retail
/// game modes and `MAPNAMES_DOOM2` for commercial; the Plutonia/TNT
/// selection paths are dead code there but the tables are kept so a
/// future episode pack can wire them up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MapNameSet {
    /// The Ultimate DOOM episode tables (`mapnames[]`).
    #[default]
    Episode,
    /// The DOOM II table (`mapnames2[]`).
    Doom2,
    /// The Plutonia table (`mapnamesp[]`).
    Plutonia,
    /// The TNT: Evilution table (`mapnamest[]`).
    Tnt,
}

impl MapNameSet {
    /// Looks up the title for `(episode, map)`. Both are 1-based;
    /// out-of-range values are clamped instead of reading out of the
    /// tables (the C code indexed blindly).
    pub fn title(self, episode: u8, map: u8) -> &'static str {
        match self {
            MapNameSet::Episode => {
                let episode = i64::from(episode.saturating_sub(1));
                let map = i64::from(map.saturating_sub(1));
                let index = (episode * 9 + map).clamp(0, MAPNAMES_EPISODE.len() as i64 - 1);
                MAPNAMES_EPISODE[index as usize]
            }
            MapNameSet::Doom2 => MAPNAMES_DOOM2[map.clamp(1, 32) as usize - 1],
            MapNameSet::Plutonia => MAPNAMES_PLUTONIA[map.clamp(1, 32) as usize - 1],
            MapNameSet::Tnt => MAPNAMES_TNT[map.clamp(1, 32) as usize - 1],
        }
    }
}

/// Chat macros (`chat_macros[]` in `hu_stuff.c`, indexed by the
/// Alt+0..9 hotkeys; `CHATMACRO0` is "No").
pub const CHAT_MACROS: [&str; 10] = [
    "No",
    "I'm ready to kick butt!",
    "I'm OK.",
    "I'm not looking too good!",
    "Help!",
    "You suck!",
    "Next time, scumbag...",
    "Come here!",
    "I'll take care of it.",
    "Yes",
];

/// Net-game player names, indexed by player number (`player_names[]`,
/// `HUSTR_PLRGREEN` etc.), for "`<name>: <chat text>`" messages.
pub const PLAYER_NAMES: [&str; 4] = [
    "Green: ",  // HUSTR_PLRGREEN
    "Indigo: ", // HUSTR_PLRINDIGO
    "Brown: ",  // HUSTR_PLRBROWN
    "Red: ",    // HUSTR_PLRRED
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_variant_exactly_once() {
        // uniqueness of the texts (except the shared "" of None) proves
        // ALL does not repeat a variant and the strings were not
        // accidentally duplicated
        let mut texts = std::collections::HashSet::new();
        for &msg in HudMessage::ALL {
            texts.insert(msg.as_str());
        }
        assert_eq!(texts.len(), HudMessage::ALL.len());
    }

    #[test]
    fn every_variant_has_a_nonempty_string_except_none() {
        for &msg in HudMessage::ALL {
            if msg == HudMessage::None {
                assert_eq!(msg.as_str(), "");
            } else {
                assert!(!msg.as_str().is_empty(), "{msg:?} has no string");
            }
        }
    }

    #[test]
    fn variant_count_matches_the_english_strings() {
        // 71 real messages + None
        assert_eq!(HudMessage::ALL.len(), 72);
    }

    #[test]
    fn spot_check_against_denglesh_h() {
        assert_eq!(HudMessage::GotVisor.as_str(), "Light Amplification Visor");
        assert_eq!(HudMessage::GotChaingun.as_str(), "You got the chaingun!");
        assert_eq!(
            HudMessage::GotMediNeed.as_str(),
            "Picked up a medikit that you REALLY need!"
        );
        assert_eq!(
            HudMessage::PdBlueK.as_str(),
            "You need a blue key to open this door"
        );
        assert_eq!(HudMessage::MsgOff.as_str(), "Messages OFF");
        assert_eq!(HudMessage::MsgUnsent.as_str(), "[Message unsent]");
        assert_eq!(
            HudMessage::StBehold.as_str(),
            "inVuln, Str, Inviso, Rad, Allmap, or Lite-amp"
        );
        assert_eq!(
            HudMessage::GotBfg9000.as_str(),
            "You got the BFG9000!  Oh, yes."
        );
        assert_eq!(HudMessage::GgSaved.as_str(), "game saved.");
        assert_eq!(HudMessage::MarkedSpot.as_str(), "Marked Spot");
        assert_eq!(marked_spot_message(3), "Marked Spot 3");
    }

    #[test]
    fn episode_titles_match_vanilla_table() {
        assert_eq!(MapNameSet::Episode.title(1, 1), "E1M1: Hangar");
        assert_eq!(MapNameSet::Episode.title(1, 9), "E1M9: Military Base");
        assert_eq!(MapNameSet::Episode.title(2, 6), "E2M6: Halls of the Damned");
        assert_eq!(MapNameSet::Episode.title(3, 8), "E3M8: Dis");
        assert_eq!(MapNameSet::Episode.title(4, 9), "E4M9: Fear");
        assert_eq!(MapNameSet::Episode.title(4, 4), "E4M4: Unruly Evil");
        // out-of-range clamps into the NEWLEVEL filler instead of OOB
        assert_eq!(MapNameSet::Episode.title(9, 9), "NEWLEVEL");
        assert_eq!(MapNameSet::Episode.title(0, 0), "E1M1: Hangar");
    }

    #[test]
    fn doom2_titles_match_vanilla_table() {
        assert_eq!(MapNameSet::Doom2.title(0, 1), "level 1: entryway");
        assert_eq!(
            MapNameSet::Doom2.title(0, 11),
            "level 11: 'o' of destruction!"
        );
        assert_eq!(MapNameSet::Doom2.title(0, 23), "level 23: barrels o' fun");
        assert_eq!(MapNameSet::Doom2.title(0, 32), "level 32: grosse");
        // clamped, never OOB
        assert_eq!(MapNameSet::Doom2.title(0, 0), "level 1: entryway");
        assert_eq!(MapNameSet::Doom2.title(0, 99), "level 32: grosse");
    }

    #[test]
    fn plutonia_and_tnt_tables_match_vanilla() {
        assert_eq!(MapNameSet::Plutonia.title(0, 32), "level 32: go 2 it");
        assert_eq!(MapNameSet::Plutonia.title(0, 9), "level 9: abattoire");
        assert_eq!(MapNameSet::Plutonia.title(0, 1), "level 1: congo");
        assert_eq!(MapNameSet::Tnt.title(0, 32), "level 32: caribbean");
        assert_eq!(MapNameSet::Tnt.title(0, 26), "level 26: ballistyx");
        assert_eq!(MapNameSet::Tnt.title(0, 1), "level 1: system control");
        assert_eq!(MapNameSet::Tnt.title(0, 99), "level 32: caribbean");
    }

    #[test]
    fn episode_table_shape() {
        assert_eq!(MAPNAMES_EPISODE.len(), 45);
        assert!(MAPNAMES_EPISODE[36..].iter().all(|s| *s == "NEWLEVEL"));
        assert_eq!(MAPNAMES_EPISODE[0], "E1M1: Hangar");
        assert_eq!(MAPNAMES_EPISODE[35], "E4M9: Fear");
    }

    #[test]
    fn chat_macros_match_vanilla_order() {
        assert_eq!(CHAT_MACROS[0], "No");
        assert_eq!(CHAT_MACROS[1], "I'm ready to kick butt!");
        assert_eq!(CHAT_MACROS[4], "Help!");
        assert_eq!(CHAT_MACROS[9], "Yes");
    }

    #[test]
    fn player_names_match_vanilla_order() {
        assert_eq!(PLAYER_NAMES, ["Green: ", "Indigo: ", "Brown: ", "Red: "]);
    }
}
