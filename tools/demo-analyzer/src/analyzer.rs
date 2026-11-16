//! Command implementations for `doom-demo-analyzer`.
//!
//! Everything here works on a [`ParsedDemo`]; reading and decoding the
//! wire format lives in [`crate::demo`]. Output is plain text aimed at
//! humans, except `dump-tics --format jsonl`, which targets scripts.

use std::io::Write;
use std::path::Path;

use crate::demo::{
    self, BT_ATTACK, BT_CHANGE, BT_SPECIAL, BT_SPECIALMASK, BT_UNDEFINED_0X40, BT_USE,
    BT_WEAPONMASK, BT_WEAPONSHIFT, BTS_PAUSE, BTS_SAVEGAME, BTS_SAVEMASK, BTS_SAVESHIFT,
    MAX_DEMO_SIZE, ParsedDemo, StreamEnd, TicRecord, is_known_version,
};

const TICRATE: f64 = 35.0;

/// Rendered `buttons` byte, decoded the vanilla way (`d_event.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonDecode {
    pub attack: bool,
    pub use_button: bool,
    pub weapon_change: bool,
    /// Weapon number when `BT_CHANGE` is set, else `None`.
    pub weapon: Option<u8>,
    pub special: bool,
    pub pause: bool,
    pub save: bool,
    /// Save slot when this is a `BTS_SAVEGAME` special, else `None`.
    pub save_slot: Option<u8>,
    pub undefined_0x40: bool,
}

/// Decode a raw `buttons` byte.
pub fn decode_buttons(buttons: u8) -> ButtonDecode {
    let special = buttons & BT_SPECIAL != 0;
    let special_cmd = buttons & BT_SPECIALMASK;
    let weapon_change = buttons & BT_CHANGE != 0;
    ButtonDecode {
        attack: buttons & BT_ATTACK != 0,
        use_button: buttons & BT_USE != 0,
        weapon_change,
        weapon: weapon_change.then_some((buttons & BT_WEAPONMASK) >> BT_WEAPONSHIFT),
        special,
        pause: special && special_cmd == BTS_PAUSE,
        save: special && special_cmd == BTS_SAVEGAME,
        save_slot: (special && special_cmd == BTS_SAVEGAME)
            .then_some((buttons & BTS_SAVEMASK) >> BTS_SAVESHIFT),
        undefined_0x40: buttons & BT_UNDEFINED_0X40 != 0,
    }
}

/// Vanilla weapon names for the weapon bits, in `weapontype_t` order.
pub const WEAPON_NAMES: [&str; 8] = [
    "fist",
    "pistol",
    "shotgun",
    "chaingun",
    "rocket launcher",
    "plasma rifle",
    "BFG 9000",
    "super shotgun",
];

fn pct(part: u64, total: u64) -> String {
    if total == 0 {
        return "0.0%".to_string();
    }
    format!("{:.1}%", part as f64 * 100.0 / total as f64)
}

/// `angle_turn` (BAM) to signed degrees.
fn bam_to_degrees_f64(turn: f64) -> f64 {
    turn / 65536.0 * 360.0
}

fn active_player_flags(demo: &ParsedDemo) -> String {
    let flags: Vec<String> = demo
        .header
        .playeringame
        .iter()
        .map(|p| p.to_string())
        .collect();
    format!("[{}]", flags.join(","))
}

/// Describe how the tic stream terminated, in one line.
fn end_summary(demo: &ParsedDemo) -> String {
    match demo.end {
        StreamEnd::Marker { offset } => {
            if demo.trailing_bytes.is_empty() {
                format!("ends with DEMOMARKER (0x80) at offset {offset}")
            } else {
                format!(
                    "ends with DEMOMARKER (0x80) at offset {offset} (+ {} trailing byte(s))",
                    demo.trailing_bytes.len()
                )
            }
        }
        StreamEnd::Truncated { offset, leftover } => {
            format!("truncated tic at offset {offset} ({leftover} byte(s) left)")
        }
        StreamEnd::EndOfFile => "no DEMOMARKER found (stream just stopped)".to_string(),
    }
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

/// `doom-demo-analyzer info <file>`
pub fn cmd_info(w: &mut dyn Write, file: &Path, demo: &ParsedDemo) -> Result<u8, String> {
    let header = &demo.header;
    writeln!(w, "File: {}", file.display()).map_err(|e| e.to_string())?;
    writeln!(w, "Size: {} bytes", demo.total_len).map_err(|e| e.to_string())?;
    writeln!(w).map_err(|e| e.to_string())?;
    writeln!(w, "Header:").map_err(|e| e.to_string())?;
    let version_note = match header.version {
        doom_engine::game::DEMO_VERSION => "DOOM 1.10 (linuxdoom-1.10)",
        demo::DEMO_VERSION_109 => "DOOM 1.9",
        _ => "unknown version",
    };
    writeln!(w, "  Version (magic): {} ({version_note})", header.version)
        .map_err(|e| e.to_string())?;
    writeln!(w, "  Skill: {} ({})", header.skill, header.skill_name())
        .map_err(|e| e.to_string())?;
    writeln!(w, "  Episode: {}", header.episode).map_err(|e| e.to_string())?;
    writeln!(w, "  Map: {}", header.map).map_err(|e| e.to_string())?;
    let (primary, commercial) = header.level_labels();
    match commercial {
        Some(alt) => writeln!(w, "  Level: {primary}; commercial reading: {alt}")
            .map_err(|e| e.to_string())?,
        None => writeln!(w, "  Level: {primary}").map_err(|e| e.to_string())?,
    }
    writeln!(
        w,
        "  Deathmatch: {} ({})",
        header.deathmatch_name(),
        header.deathmatch
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  Respawn monsters: {} (-respawn)",
        yes_no(header.respawn_monsters)
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  Fast monsters: {} (-fast)",
        yes_no(header.fast_monsters)
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  No monsters: {} (-nomonsters)",
        yes_no(header.no_monsters)
    )
    .map_err(|e| e.to_string())?;
    writeln!(w, "  Console player: {}", header.console_player).map_err(|e| e.to_string())?;
    let active = header.active_players();
    let active_list = if active.is_empty() {
        "none".to_string()
    } else {
        active
            .iter()
            .map(|i| format!("player {}", i + 1))
            .collect::<Vec<_>>()
            .join(", ")
    };
    writeln!(
        w,
        "  Players: {} in game ({}; {})",
        header.player_count(),
        active_player_flags(demo),
        active_list
    )
    .map_err(|e| e.to_string())?;
    writeln!(w).map_err(|e| e.to_string())?;
    writeln!(w, "Stream:").map_err(|e| e.to_string())?;
    writeln!(w, "  Tics: {}", demo.tics.len()).map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  Duration: {:.1} s at 35 tics/s",
        demo.tics.len() as f64 / TICRATE
    )
    .map_err(|e| e.to_string())?;
    writeln!(w, "  {}", end_summary(demo)).map_err(|e| e.to_string())?;
    Ok(0)
}

fn yes_no(value: u8) -> &'static str {
    if value == 0 { "no" } else { "yes" }
}

// ---------------------------------------------------------------------------
// stats
// ---------------------------------------------------------------------------

/// Axis accumulator for the raw `-128..=127` command bytes.
struct Axis {
    positive: u64,
    negative: u64,
    zero: u64,
    sum: i64,
    min: i32,
    max: i32,
    /// Per raw byte value, indexed by `v as u8 as usize`.
    dist: [u64; 256],
}

impl Default for Axis {
    fn default() -> Self {
        Self {
            positive: 0,
            negative: 0,
            zero: 0,
            sum: 0,
            min: 0,
            max: 0,
            dist: [0; 256],
        }
    }
}

impl Axis {
    fn add(&mut self, v: i8) {
        let n = i32::from(v);
        self.sum += n as i64;
        if self.positive + self.negative + self.zero == 0 {
            self.min = n;
            self.max = n;
        } else {
            self.min = self.min.min(n);
            self.max = self.max.max(n);
        }
        self.dist[v as u8 as usize] += 1;
        if v > 0 {
            self.positive += 1;
        } else if v < 0 {
            self.negative += 1;
        } else {
            self.zero += 1;
        }
    }

    fn mean(&self) -> f64 {
        let total = self.positive + self.negative + self.zero;
        if total == 0 {
            0.0
        } else {
            self.sum as f64 / total as f64
        }
    }

    fn dist_line(&self) -> String {
        let mut parts = Vec::new();
        for v in i8::MIN..=i8::MAX {
            if v == 0 {
                continue;
            }
            let count = self.dist[v as u8 as usize];
            if count > 0 {
                parts.push(format!("{v:+}:{count}"));
            }
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join("  ")
        }
    }
}

#[derive(Default)]
struct TurnStats {
    count: u64,
    nonzero: u64,
    sum: i64,
    sum_abs: i64,
    sum_nonzero: i64,
    min: i32,
    max: i32,
    min_nonzero: i32,
    max_nonzero: i32,
}

impl TurnStats {
    fn add(&mut self, turn: i16) {
        let n = i32::from(turn);
        if self.count == 0 {
            self.min = n;
            self.max = n;
        } else {
            self.min = self.min.min(n);
            self.max = self.max.max(n);
        }
        self.count += 1;
        self.sum += n as i64;
        self.sum_abs += n.abs() as i64;
        if turn != 0 {
            if self.nonzero == 0 {
                self.min_nonzero = n;
                self.max_nonzero = n;
            } else {
                self.min_nonzero = self.min_nonzero.min(n);
                self.max_nonzero = self.max_nonzero.max(n);
            }
            self.sum_nonzero += n as i64;
            self.nonzero += 1;
        }
    }
}

#[derive(Default)]
struct ButtonStats {
    attack: u64,
    use_button: u64,
    weapon_change: u64,
    weapons: [u64; 8],
    special_pause: u64,
    special_save: u64,
    special_other: u64,
    undefined_0x40: u64,
}

impl ButtonStats {
    fn add(&mut self, buttons: u8) {
        let decoded = decode_buttons(buttons);
        if decoded.attack {
            self.attack += 1;
        }
        if decoded.use_button {
            self.use_button += 1;
        }
        if decoded.weapon_change {
            self.weapon_change += 1;
            if let Some(weapon) = decoded.weapon {
                self.weapons[weapon as usize & 7] += 1;
            }
        }
        if decoded.special {
            if decoded.pause {
                self.special_pause += 1;
            } else if decoded.save {
                self.special_save += 1;
            } else {
                self.special_other += 1;
            }
        }
        if decoded.undefined_0x40 {
            self.undefined_0x40 += 1;
        }
    }
}

/// One interrupted activity run, with its start tic and length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    start: usize,
    len: usize,
}

/// Track the longest run for which `active` held.
struct RunTracker {
    current: Option<(usize, usize)>,
    longest: Option<Run>,
}

impl RunTracker {
    fn new() -> Self {
        Self {
            current: None,
            longest: None,
        }
    }

    fn feed(&mut self, tic: usize, active: bool) {
        match (self.current, active) {
            (Some((start, len)), true) => self.current = Some((start, len + 1)),
            (None, true) => self.current = Some((tic, 1)),
            (Some(_), false) => {
                self.finish();
            }
            (None, false) => {}
        }
    }

    fn finish(&mut self) {
        if let Some((start, len)) = self.current.take() {
            let run = Run { start, len };
            let better = match &self.longest {
                None => true,
                Some(best) => run.len > best.len,
            };
            if better {
                self.longest = Some(run);
            }
        }
    }
}

/// `doom-demo-analyzer stats <file>`
pub fn cmd_stats(w: &mut dyn Write, file: &Path, demo: &ParsedDemo) -> Result<u8, String> {
    let tics = &demo.tics;
    let total = tics.len() as u64;

    let mut forward = Axis::default();
    let mut side = Axis::default();
    let mut turns = TurnStats::default();
    let mut buttons = ButtonStats::default();
    let mut button_bytes = [0u64; 256];

    let mut idle = RunTracker::new();
    let mut attack = RunTracker::new();

    // fixed 35-tic windows for burst detection
    let mut window_attacks: Vec<u64> = Vec::new();

    for (i, record) in tics.iter().enumerate() {
        let cmd = &record.cmd;
        forward.add(cmd.forward_move);
        side.add(cmd.side_move);
        turns.add(cmd.angle_turn);
        buttons.add(cmd.buttons);
        button_bytes[cmd.buttons as usize] += 1;

        let decoded = decode_buttons(cmd.buttons);
        let is_idle =
            cmd.forward_move == 0 && cmd.side_move == 0 && cmd.angle_turn == 0 && cmd.buttons == 0;
        idle.feed(i, is_idle);
        attack.feed(i, decoded.attack);

        if i % 35 == 0 {
            window_attacks.push(0);
        }
        if decoded.attack {
            if let Some(last) = window_attacks.last_mut() {
                *last += 1;
            }
        }
    }
    idle.finish();
    attack.finish();

    writeln!(w, "File: {}", file.display()).map_err(|e| e.to_string())?;
    let (primary, _) = demo.header.level_labels();
    writeln!(
        w,
        "Version: {} ({}); Level: {primary}; Skill: {} ({})",
        demo.header.version,
        if is_known_version(demo.header.version) {
            "known"
        } else {
            "UNKNOWN"
        },
        demo.header.skill,
        demo.header.skill_name()
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "Tics: {} ({:.1} s at 35 tics/s)",
        tics.len(),
        tics.len() as f64 / TICRATE
    )
    .map_err(|e| e.to_string())?;
    writeln!(w, "Tic commands: {}", tics.len()).map_err(|e| e.to_string())?;
    writeln!(w, "{}", end_summary(demo)).map_err(|e| e.to_string())?;
    writeln!(w).map_err(|e| e.to_string())?;

    writeln!(w, "Movement:").map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  forward: {} forward ({}), {} backward ({}), {} neutral ({})",
        forward.positive,
        pct(forward.positive, total),
        forward.negative,
        pct(forward.negative, total),
        forward.zero,
        pct(forward.zero, total)
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  RUNNING (forward > 0): {} tics ({})",
        forward.positive,
        pct(forward.positive, total)
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  forward mean: {:+.1}, min {}, max {}",
        forward.mean(),
        forward.min,
        forward.max
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  forward distribution (nonzero): {}",
        forward.dist_line()
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  side (right > 0): {} right ({}), {} left ({}), {} neutral ({})",
        side.positive,
        pct(side.positive, total),
        side.negative,
        pct(side.negative, total),
        side.zero,
        pct(side.zero, total)
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  side mean: {:+.1}, min {}, max {}",
        side.mean(),
        side.min,
        side.max
    )
    .map_err(|e| e.to_string())?;
    writeln!(w, "  side distribution (nonzero): {}", side.dist_line())
        .map_err(|e| e.to_string())?;
    writeln!(w).map_err(|e| e.to_string())?;

    writeln!(w, "Turns:").map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  nonzero turns: {} tics ({})",
        turns.nonzero,
        pct(turns.nonzero, total)
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  angle_turn BAM mean: {:+.1} (all tics)",
        turns.sum as f64 / total.max(1) as f64
    )
    .map_err(|e| e.to_string())?;
    writeln!(w, "  angle_turn BAM min/max: {} / {}", turns.min, turns.max)
        .map_err(|e| e.to_string())?;
    if turns.nonzero > 0 {
        writeln!(
            w,
            "  angle_turn BAM mean (nonzero): {:+.1}, min {}, max {}",
            turns.sum_nonzero as f64 / turns.nonzero as f64,
            turns.min_nonzero,
            turns.max_nonzero
        )
        .map_err(|e| e.to_string())?;
    }
    writeln!(
        w,
        "  per-tic degrees: mean {:+.2}, min {:+.2}, max {:+.2}",
        bam_to_degrees_f64(turns.sum as f64 / total.max(1) as f64),
        bam_to_degrees_f64(f64::from(turns.min)),
        bam_to_degrees_f64(f64::from(turns.max))
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  total absolute rotation: {:.1} degrees",
        turns.sum_abs as f64 / 65536.0 * 360.0
    )
    .map_err(|e| e.to_string())?;
    writeln!(w).map_err(|e| e.to_string())?;

    writeln!(w, "Buttons (per tic):").map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  ATTACK: {} tics ({})",
        buttons.attack,
        pct(buttons.attack, total)
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  USE: {} tics ({})",
        buttons.use_button,
        pct(buttons.use_button, total)
    )
    .map_err(|e| e.to_string())?;
    let weapons: Vec<String> = buttons
        .weapons
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(idx, count)| format!("{} ({})x{}", WEAPON_NAMES[idx], idx, count))
        .collect();
    writeln!(
        w,
        "  WEAPON CHANGE: {} tics ({}){}",
        buttons.weapon_change,
        pct(buttons.weapon_change, total),
        if weapons.is_empty() {
            String::new()
        } else {
            format!("  [{}]", weapons.join(", "))
        }
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  SPECIAL: {} (pause: {}, save: {}, other: {})",
        buttons.special_pause + buttons.special_save + buttons.special_other,
        buttons.special_pause,
        buttons.special_save,
        buttons.special_other
    )
    .map_err(|e| e.to_string())?;
    writeln!(w, "  undefined bit 0x40: {}", buttons.undefined_0x40).map_err(|e| e.to_string())?;
    let distinct_button_bytes = button_bytes.iter().filter(|&&count| count > 0).count();
    writeln!(w, "  distinct button byte values: {distinct_button_bytes}")
        .map_err(|e| e.to_string())?;
    writeln!(w).map_err(|e| e.to_string())?;

    writeln!(w, "Activity:").map_err(|e| e.to_string())?;
    match idle.longest {
        Some(run) => writeln!(
            w,
            "  longest idle run: {} tics (tics {}..{})",
            run.len,
            run.start,
            run.start + run.len - 1
        )
        .map_err(|e| e.to_string())?,
        None => writeln!(w, "  longest idle run: none (demo is never full-stop)")
            .map_err(|e| e.to_string())?,
    }
    match attack.longest {
        Some(run) => writeln!(
            w,
            "  longest attack run: {} tics (tics {}..{})",
            run.len,
            run.start,
            run.start + run.len - 1
        )
        .map_err(|e| e.to_string())?,
        None => {
            writeln!(w, "  longest attack run: none (never fired)").map_err(|e| e.to_string())?
        }
    }
    if window_attacks.is_empty() {
        writeln!(w, "  bursts: no full 35-tic windows in demo").map_err(|e| e.to_string())?;
    } else {
        let num_windows = window_attacks.len();
        let mean_attacks = buttons.attack as f64 / num_windows as f64;
        let peak = window_attacks
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| **count)
            .map(|(idx, count)| (idx, *count));
        if let Some((idx, count)) = peak {
            writeln!(
                w,
                "  busiest 35-tic window: tics {}..{} with {} attack(s)",
                idx * 35,
                (idx * 35 + 34).min(tics.len() - 1),
                count
            )
            .map_err(|e| e.to_string())?;
        }
        let bursts: Vec<String> = window_attacks
            .iter()
            .enumerate()
            .filter(|(_, count)| **count >= 8 && **count as f64 >= mean_attacks * 2.0)
            .take(5)
            .map(|(idx, count)| {
                format!(
                    "tics {}..{} ({} attack(s))",
                    idx * 35,
                    (idx * 35 + 34).min(tics.len() - 1),
                    count
                )
            })
            .collect();
        if bursts.is_empty() {
            writeln!(
                w,
                "  unusual bursts (>=8 attacks in a 35-tic window, >=2x the average): none"
            )
            .map_err(|e| e.to_string())?;
        } else {
            writeln!(
                w,
                "  unusual bursts (>=8 attacks in a 35-tic window, >=2x the average): {}",
                bursts.join(", ")
            )
            .map_err(|e| e.to_string())?;
        }
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// dump-tics
// ---------------------------------------------------------------------------

/// Output format for `dump-tics`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicFormat {
    /// Aligned plain-text columns (one tic per line).
    Plain,
    /// JSON Lines - one object per tic, for scripting.
    Jsonl,
}

/// `doom-demo-analyzer dump-tics <file> [--limit N] [--format plain|jsonl]`
pub fn cmd_dump_tics(
    w: &mut dyn Write,
    demo: &ParsedDemo,
    limit: Option<usize>,
    format: TicFormat,
) -> Result<u8, String> {
    let limit = limit.unwrap_or(demo.tics.len());
    match format {
        TicFormat::Plain => {
            writeln!(w, "# tic forward_move side_move buttons angle_turn")
                .map_err(|e| e.to_string())?;
            for record in demo.tics.iter().take(limit) {
                writeln!(
                    w,
                    "{} {} {} {} {}",
                    record.index,
                    record.cmd.forward_move,
                    record.cmd.side_move,
                    record.cmd.buttons,
                    record.cmd.angle_turn
                )
                .map_err(|e| e.to_string())?;
            }
        }
        TicFormat::Jsonl => {
            for record in demo.tics.iter().take(limit) {
                writeln!(w, "{}", tic_jsonl(record)).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(0)
}

fn tic_jsonl(record: &TicRecord) -> String {
    let cmd = &record.cmd;
    let decoded = decode_buttons(cmd.buttons);
    let weapon = match decoded.weapon {
        Some(weapon) => weapon.to_string(),
        None => "null".to_string(),
    };
    let save_slot = match decoded.save_slot {
        Some(slot) => slot.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"tic\":{},\"forward_move\":{},\"side_move\":{},\"buttons\":{},\"angle_turn\":{},\"angle_turn_byte\":{},\"attack\":{},\"use\":{},\"weapon_change\":{},\"weapon\":{},\"special\":{},\"pause\":{},\"save\":{},\"save_slot\":{}}}",
        record.index,
        cmd.forward_move,
        cmd.side_move,
        cmd.buttons,
        cmd.angle_turn,
        record.raw[2],
        decoded.attack,
        decoded.use_button,
        decoded.weapon_change,
        weapon,
        decoded.special,
        decoded.pause,
        decoded.save,
        save_slot
    )
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

/// Header field names and decoded values, for the comparison report.
fn header_fields(demo: &ParsedDemo) -> Vec<(String, String)> {
    let h = &demo.header;
    let mut fields = vec![
        ("version".to_string(), h.version.to_string()),
        ("skill".to_string(), h.skill.to_string()),
        ("episode".to_string(), h.episode.to_string()),
        ("map".to_string(), h.map.to_string()),
        ("deathmatch".to_string(), h.deathmatch.to_string()),
        (
            "respawn_monsters".to_string(),
            h.respawn_monsters.to_string(),
        ),
        ("fast_monsters".to_string(), h.fast_monsters.to_string()),
        ("no_monsters".to_string(), h.no_monsters.to_string()),
        ("console_player".to_string(), h.console_player.to_string()),
    ];
    for (i, player) in h.playeringame.iter().enumerate() {
        fields.push((format!("playeringame[{i}]"), player.to_string()));
    }
    fields
}

fn header_diffs(a: &ParsedDemo, b: &ParsedDemo) -> Vec<String> {
    header_fields(a)
        .into_iter()
        .zip(header_fields(b))
        .filter(|((name_a, value_a), (name_b, value_b))| {
            debug_assert_eq!(name_a, name_b);
            value_a != value_b
        })
        .map(|((name, value_a), (_, value_b))| format!("  - {name}: {value_a} vs {value_b}"))
        .collect()
}

/// Field-level difference strings for the first differing tic.
fn first_diff_fields(a: &TicRecord, b: &TicRecord) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |name: &str, va: i64, vb: i64| {
        if va != vb {
            out.push(format!("    {name}: {va} vs {vb} (delta {:+})", vb - va));
        }
    };
    push(
        "forward_move",
        i64::from(a.cmd.forward_move),
        i64::from(b.cmd.forward_move),
    );
    push(
        "side_move",
        i64::from(a.cmd.side_move),
        i64::from(b.cmd.side_move),
    );
    push(
        "angle_turn",
        i64::from(a.cmd.angle_turn),
        i64::from(b.cmd.angle_turn),
    );
    push(
        "buttons",
        i64::from(a.cmd.buttons),
        i64::from(b.cmd.buttons),
    );
    out
}

/// `doom-demo-analyzer compare <file1> <file2>`
pub fn cmd_compare(
    w: &mut dyn Write,
    path_a: &Path,
    a: &ParsedDemo,
    path_b: &Path,
    b: &ParsedDemo,
) -> Result<u8, String> {
    writeln!(w, "Comparing demos").map_err(|e| e.to_string())?;
    let (label_a, _) = a.header.level_labels();
    let (label_b, _) = b.header.level_labels();
    writeln!(
        w,
        "  a: {} (version {}, level {label_a}, {} tics)",
        path_a.display(),
        a.header.version,
        a.tics.len()
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  b: {} (version {}, level {label_b}, {} tics)",
        path_b.display(),
        b.header.version,
        b.tics.len()
    )
    .map_err(|e| e.to_string())?;
    writeln!(w).map_err(|e| e.to_string())?;

    // header
    let diffs = header_diffs(a, b);
    if diffs.is_empty() {
        writeln!(w, "Header: identical").map_err(|e| e.to_string())?;
    } else {
        writeln!(w, "Header: DIFFERENT").map_err(|e| e.to_string())?;
        for line in &diffs {
            writeln!(w, "{line}").map_err(|e| e.to_string())?;
        }
    }

    // ending
    let (ending_equal, ending_line) = ending_cmp(a, b);
    writeln!(w, "Ending: {ending_line}").map_err(|e| e.to_string())?;

    // tics
    let common = a.tics.len().min(b.tics.len());
    let mut diff_count = 0u64;
    let mut first: Option<usize> = None;
    let mut first_fields: Vec<String> = Vec::new();
    let mut first_ten: Vec<usize> = Vec::new();
    for i in 0..common {
        if a.tics[i].raw != b.tics[i].raw {
            diff_count += 1;
            if first.is_none() {
                first = Some(i);
                first_fields = first_diff_fields(&a.tics[i], &b.tics[i]);
            }
            if first_ten.len() < 10 {
                first_ten.push(i);
            }
        }
    }

    if let Some(index) = first {
        writeln!(
            w,
            "Tics: comparing {common} tic(s) (a: {}, b: {})",
            a.tics.len(),
            b.tics.len()
        )
        .map_err(|e| e.to_string())?;
        writeln!(w, "  first difference at tic {index}:").map_err(|e| e.to_string())?;
        for line in &first_fields {
            writeln!(w, "{line}").map_err(|e| e.to_string())?;
        }
        writeln!(w, "  differing tics: {diff_count} of {common} compared")
            .map_err(|e| e.to_string())?;
        writeln!(
            w,
            "  first 10 differing tics: {}",
            first_ten
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .map_err(|e| e.to_string())?;
    } else if a.tics.len() == b.tics.len() {
        writeln!(w, "Tics: all {common} tic(s) identical").map_err(|e| e.to_string())?;
    } else {
        writeln!(
            w,
            "Tics: all {common} tic(s) of the common prefix identical"
        )
        .map_err(|e| e.to_string())?;
    }

    if a.tics.len() != b.tics.len() {
        let (longer, longer_len, shorter_len) = if a.tics.len() > b.tics.len() {
            ("a", a.tics.len(), b.tics.len())
        } else {
            ("b", b.tics.len(), a.tics.len())
        };
        writeln!(
            w,
            "  {longer} has {} extra tic(s) ({longer_len} vs {shorter_len})",
            longer_len - shorter_len
        )
        .map_err(|e| e.to_string())?;
    }

    let identical =
        diffs.is_empty() && diff_count == 0 && a.tics.len() == b.tics.len() && ending_equal;
    writeln!(w).map_err(|e| e.to_string())?;
    if identical {
        writeln!(w, "Result: IDENTICAL").map_err(|e| e.to_string())?;
        Ok(0)
    } else {
        writeln!(w, "Result: DIFFERENT").map_err(|e| e.to_string())?;
        Ok(1)
    }
}

fn ending_cmp(a: &ParsedDemo, b: &ParsedDemo) -> (bool, String) {
    match (&a.end, &b.end) {
        (StreamEnd::Marker { .. }, StreamEnd::Marker { .. }) => {
            if a.trailing_bytes == b.trailing_bytes {
                (true, "both end with DEMOMARKER (0x80)".to_string())
            } else {
                (
                    false,
                    format!(
                        "DIFFERENT: a has {} trailing byte(s) after DEMOMARKER, b has {}",
                        a.trailing_bytes.len(),
                        b.trailing_bytes.len()
                    ),
                )
            }
        }
        _ => (
            false,
            format!(
                "DIFFERENT: a {} / b {}",
                describe_end(&a.end),
                describe_end(&b.end)
            ),
        ),
    }
}

fn describe_end(end: &StreamEnd) -> &'static str {
    match end {
        StreamEnd::Marker { .. } => "ends with DEMOMARKER",
        StreamEnd::Truncated { .. } => "is truncated mid-tic",
        StreamEnd::EndOfFile => "has no DEMOMARKER",
    }
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ok,
    Warn,
    Fatal,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Ok => "ok",
            Severity::Warn => "warn",
            Severity::Fatal => "fatal",
        }
    }
}

struct Finding {
    severity: Severity,
    message: String,
}

fn ok(findings: &mut Vec<Finding>, message: String) {
    findings.push(Finding {
        severity: Severity::Ok,
        message,
    });
}

fn warn(findings: &mut Vec<Finding>, message: String) {
    findings.push(Finding {
        severity: Severity::Warn,
        message,
    });
}

fn fatal(findings: &mut Vec<Finding>, message: String) {
    findings.push(Finding {
        severity: Severity::Fatal,
        message,
    });
}

/// `doom-demo-analyzer validate <file>`
pub fn cmd_validate(w: &mut dyn Write, file: &Path, demo: &ParsedDemo) -> Result<u8, String> {
    let mut findings: Vec<Finding> = Vec::new();
    let header = &demo.header;

    // file size
    if demo.total_len > MAX_DEMO_SIZE {
        warn(
            &mut findings,
            format!(
                "file size {} exceeds the vanilla 0x20000 ({MAX_DEMO_SIZE}) record buffer; playback is unaffected, but this demo could not have been recorded by an unmodified vanilla game",
                demo.total_len
            ),
        );
    } else {
        ok(
            &mut findings,
            format!(
                "file size {} bytes (vanilla cap {MAX_DEMO_SIZE})",
                demo.total_len
            ),
        );
    }

    // version
    if is_known_version(header.version) {
        let note = if header.version == demo::DEMO_VERSION_109 {
            "DOOM 1.9"
        } else {
            "DOOM 1.10 (linuxdoom-1.10)"
        };
        ok(
            &mut findings,
            format!("version byte {} ({note})", header.version),
        );
    } else {
        fatal(
            &mut findings,
            format!(
                "unknown demo version {} (expected 109 or 110)",
                header.version
            ),
        );
    }

    // skill
    if header.skill <= 4 {
        ok(
            &mut findings,
            format!("skill {} ({})", header.skill, header.skill_name()),
        );
    } else {
        fatal(
            &mut findings,
            format!("skill {} out of range (vanilla has 0..=4)", header.skill),
        );
    }

    // episode/map ranges; commercial games ignore the episode byte
    match (header.episode, header.map) {
        (_, 0) => fatal(
            &mut findings,
            "map 0 is not a valid map index (maps are 1-based)".to_string(),
        ),
        (_, map) if map > 32 => fatal(
            &mut findings,
            format!(
                "map {} out of range (vanilla games have at most 32 maps)",
                header.map
            ),
        ),
        (episode, map) if episode > 4 => warn(
            &mut findings,
            format!(
                "episode {episode} out of range (Ultima Doom has 1..=4); commercial games ignore the episode byte, map {map} treated as MAP{map:02}"
            ),
        ),
        (episode, map) if episode >= 1 && map > 9 => warn(
            &mut findings,
            format!(
                "map {map} with episode {episode} set: commercial demos use MAPxx (map 1..=32) and ignore the episode byte; an episode game tops out at MAP09"
            ),
        ),
        (episode, map) => ok(
            &mut findings,
            format!("episode {episode}, map {map} (E{episode}M{map})"),
        ),
    }

    // console player and players
    if header.console_player <= 3 {
        ok(
            &mut findings,
            format!("console player {}", header.console_player),
        );
    } else {
        fatal(
            &mut findings,
            format!(
                "console player {} out of range (MAXPLAYERS is 4)",
                header.console_player
            ),
        );
    }
    let mut bad_flags = false;
    for (i, &flag) in header.playeringame.iter().enumerate() {
        if flag > 1 {
            bad_flags = true;
            warn(
                &mut findings,
                format!("playeringame[{i}] = {flag} (vanilla writes only 0/1)"),
            );
        }
    }
    let count = header.player_count();
    if count == 0 {
        fatal(
            &mut findings,
            "no players in game (playeringame is all zero)".to_string(),
        );
    } else {
        ok(
            &mut findings,
            format!("players in game: {count} ({})", active_player_flags(demo)),
        );
        if !bad_flags
            && header.console_player <= 3
            && header.playeringame[header.console_player as usize] == 0
        {
            warn(
                &mut findings,
                format!(
                    "console player {} is not flagged in playeringame",
                    header.console_player
                ),
            );
        }
    }

    // deathmatch mode
    if header.deathmatch <= 2 {
        ok(
            &mut findings,
            format!(
                "deathmatch mode {} ({})",
                header.deathmatch,
                header.deathmatch_name()
            ),
        );
    } else {
        warn(
            &mut findings,
            format!(
                "deathmatch mode {} is not a vanilla value (0=off, 1=duel, 2=altdeath)",
                header.deathmatch
            ),
        );
    }

    // boolean flags
    for (name, value) in [
        ("respawn", header.respawn_monsters),
        ("fast", header.fast_monsters),
        ("nomonsters", header.no_monsters),
    ] {
        if value <= 1 {
            ok(&mut findings, format!("{name} flag: {}", yes_no(value)));
        } else {
            warn(
                &mut findings,
                format!("{name} flag = {value} (expected 0/1)"),
            );
        }
    }

    // tic stream
    let num_tics = demo.tics.len();
    if num_tics == 0 {
        warn(&mut findings, "demo contains no tic commands".to_string());
    } else {
        ok(
            &mut findings,
            format!("{num_tics} tic command(s), {num_tics} game tic(s)"),
        );
    }
    match demo.end {
        StreamEnd::Marker { offset } => {
            ok(
                &mut findings,
                format!("demo ends with DEMOMARKER (0x80) at offset {offset}"),
            );
            if demo.trailing_bytes.is_empty() {
                ok(
                    &mut findings,
                    "no trailing data after DEMOMARKER".to_string(),
                );
            } else {
                warn(
                    &mut findings,
                    format!(
                        "{} trailing byte(s) after DEMOMARKER (well-formed demos end exactly at the marker)",
                        demo.trailing_bytes.len()
                    ),
                );
            }
        }
        StreamEnd::Truncated { offset, leftover } => fatal(
            &mut findings,
            format!(
                "truncated tic at offset {offset}: {leftover} byte(s) left (stream length is not a multiple of 4)"
            ),
        ),
        StreamEnd::EndOfFile => fatal(
            &mut findings,
            "demo does not end with DEMOMARKER (0x80); vanilla stops playback at the marker"
                .to_string(),
        ),
    }

    // undefined button bit
    let undefined = demo
        .tics
        .iter()
        .filter(|record| record.cmd.buttons & BT_UNDEFINED_0X40 != 0)
        .count();
    if undefined > 0 {
        warn(
            &mut findings,
            format!("{undefined} tic(s) set button bit 0x40, which vanilla never uses"),
        );
    }

    // report
    let mut warnings = 0u32;
    let mut fatals = 0u32;
    writeln!(w, "File: {}", file.display()).map_err(|e| e.to_string())?;
    for finding in &findings {
        match finding.severity {
            Severity::Warn => warnings += 1,
            Severity::Fatal => fatals += 1,
            Severity::Ok => {}
        }
        writeln!(w, "[{}] {}", finding.severity.label(), finding.message)
            .map_err(|e| e.to_string())?;
    }
    writeln!(w).map_err(|e| e.to_string())?;
    if fatals > 0 {
        writeln!(
            w,
            "Result: FATAL ({fatals} fatal error(s), {warnings} warning(s))"
        )
        .map_err(|e| e.to_string())?;
        Ok(2)
    } else if warnings > 0 {
        writeln!(w, "Result: WARNINGS ({warnings} warning(s))").map_err(|e| e.to_string())?;
        Ok(1)
    } else {
        writeln!(w, "Result: OK").map_err(|e| e.to_string())?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::parse;

    #[test]
    fn decode_buttons_variants() {
        // plain attack
        let d = decode_buttons(BT_ATTACK);
        assert!(d.attack && !d.use_button && !d.weapon_change && !d.special);

        // weapon change to weapon 6 (BFG): BT_CHANGE | (6 << 3)
        let d = decode_buttons(BT_CHANGE | (6 << BT_WEAPONSHIFT));
        assert!(d.weapon_change && !d.attack);
        assert_eq!(d.weapon, Some(6));

        // BT_CHANGE with no weapon bits -> weapon 0 (fist), still a change
        let d = decode_buttons(BT_CHANGE);
        assert!(d.weapon_change);
        assert_eq!(d.weapon, Some(0));

        // pause special
        let d = decode_buttons(BT_SPECIAL | BTS_PAUSE);
        assert!(d.special && d.pause && !d.save);
        assert_eq!(d.save_slot, None);

        // save game special, slot 3
        let d = decode_buttons(BT_SPECIAL | BTS_SAVEGAME | (3 << BTS_SAVESHIFT));
        assert!(d.special && d.save && !d.pause);
        assert_eq!(d.save_slot, Some(3));

        // undefined bit 0x40 passes through
        let d = decode_buttons(BT_UNDEFINED_0X40);
        assert!(d.undefined_0x40);

        // vanilla never sets bits beyond 0xff, so every bit is accounted for
        let d = decode_buttons(0xff);
        assert!(d.attack && d.use_button && d.weapon_change && d.special);
    }

    #[test]
    fn header_diffs_detects_field_changes() {
        let base = [110, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0];
        let a = parse(&base).unwrap();
        let mut b_bytes = base;
        b_bytes[0] = 109;
        b_bytes[9] = 2;
        let b = parse(&b_bytes).unwrap();
        let diffs = header_diffs(&a, &b);
        assert!(
            diffs.iter().any(|d| d.contains("version: 110 vs 109")),
            "{diffs:?}"
        );
        assert!(
            diffs.iter().any(|d| d.contains("playeringame[0]: 1 vs 2")),
            "{diffs:?}"
        );
        assert_eq!(diffs.len(), 2);
    }

    #[test]
    fn header_diffs_empty_for_identical_demos() {
        let a = parse(&[110, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0]).unwrap();
        let b = parse(&[110, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0]).unwrap();
        assert!(header_diffs(&a, &b).is_empty());
    }

    #[test]
    fn axis_distribution_and_mean() {
        let mut axis = Axis::default();
        for v in [50, 50, 25, -25, 0] {
            axis.add(v);
        }
        assert_eq!(axis.positive, 3);
        assert_eq!(axis.negative, 1);
        assert_eq!(axis.zero, 1);
        assert_eq!(axis.min, -25);
        assert_eq!(axis.max, 50);
        assert_eq!(axis.dist[50u8 as usize], 2);
        let dist = axis.dist_line();
        assert!(dist.contains("+25:1"), "{dist}");
        assert!(dist.contains("+50:2"), "{dist}");
        assert!(dist.contains("-25:1"), "{dist}");
        assert_eq!(dist.split_whitespace().count(), 3, "no zero entry: {dist}");
    }

    #[test]
    fn turn_stats_tracks_nonzero_and_absolute_rotation() {
        let mut turns = TurnStats::default();
        for turn in [16384i16, -16384, 0] {
            turns.add(turn);
        }
        assert_eq!(turns.nonzero, 2);
        assert_eq!(turns.sum, 0);
        assert_eq!(turns.sum_abs, 32768);
        assert_eq!(turns.min, -16384);
        assert_eq!(turns.max, 16384);
        // zero turn does not count as nonzero but does count for min/max
        turns.add(0);
        assert_eq!(turns.nonzero, 2);
    }

    #[test]
    fn run_tracker_finds_the_longest_run() {
        let mut tracker = RunTracker::new();
        for (i, active) in [true, true, false, true, true, true, false]
            .iter()
            .enumerate()
        {
            tracker.feed(i, *active);
        }
        tracker.finish();
        assert_eq!(tracker.longest, Some(Run { start: 3, len: 3 }));
    }

    #[test]
    fn run_tracker_handles_a_run_still_open_at_the_end() {
        let mut tracker = RunTracker::new();
        tracker.feed(0, true);
        tracker.feed(1, true);
        tracker.finish();
        assert_eq!(tracker.longest, Some(Run { start: 0, len: 2 }));
    }

    #[test]
    fn ending_comparison_marks_mismatch() {
        // one ends with a marker, the other does not: endings differ
        let mut with_marker = [110, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0x80].to_vec();
        let truncated = [110, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0].to_vec();
        let a = parse(&with_marker).unwrap();
        let b = parse(&truncated).unwrap();
        let (equal, _) = ending_cmp(&a, &b);
        assert!(!equal);

        // both end with a marker and no trailing garbage: same ending
        with_marker.extend_from_slice(&[0x80]); // this is trailing, not a tic
        let a = parse(&with_marker).unwrap();
        let b2 = parse(&[110, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0x80]).unwrap();
        let (equal, _) = ending_cmp(&a, &b2);
        assert!(!equal); // a has trailing bytes, b2 does not
    }
}
