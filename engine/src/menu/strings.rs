//! The prompt strings the menu shows, ported from `d_englsh.h` and the
//! `endmsg[]` table in `dstrings.c`. The wording is verbatim — the
//! original is GPL and so is this port; keep the strings as they are.

/// `DOSY` — the tail every quit prompt gets.
pub const DOSY: &str = "(press y to quit)";
/// `EMPTYSTRING` — shown in load/save slots that were never written.
pub const EMPTY_SLOT: &str = "empty slot";

/// `LOADNET`
pub const LOADNET: &str = "you can't do load while in a net game!\n\npress a key.";
/// `QLOADNET`
pub const QLOADNET: &str = "you can't quickload during a netgame!\n\npress a key.";
/// `QSAVESPOT`
pub const QSAVESPOT: &str = "you haven't picked a quicksave slot yet!\n\npress a key.";
/// `SAVEDEAD`
pub const SAVEDEAD: &str = "you can't save if you aren't playing!\n\npress a key.";
/// `NEWGAME`
pub const NEWGAME_NET: &str =
    "you can't start a new game\nwhile in a network game.\n\npress a key.";
/// `NIGHTMARE`
pub const NIGHTMARE: &str =
    "are you sure? this skill level\nisn't even remotely fair.\n\npress y or n.";
/// `SWSTRING`
pub const SWSTRING: &str = "this is the shareware version of doom.\n\n\
you need to order the entire trilogy.\n\npress a key.";
/// `NETEND`
pub const NETEND: &str = "you can't end a netgame!\n\npress a key.";
/// `ENDGAME` — the end-game (return to title) prompt.
pub const ENDGAME: &str = "are you sure you want to end the game?\n\npress y or n.";

/// `QSPROMPT` with the slot name spliced in (`sprintf(QSPROMPT, name)`).
pub fn quick_save_prompt(name: &str) -> String {
    format!("quicksave over your game named\n\n'{name}'?\n\npress y or n.")
}

/// `QLPROMPT` with the slot name spliced in.
pub fn quick_load_prompt(name: &str) -> String {
    format!("do you want to quickload the game named\n\n'{name}'?\n\npress y or n.")
}

/// `NUM_QUITMESSAGES` — the count the picker wraps around.
pub const NUM_QUITMESSAGES: usize = 22;

/// `endmsg[]` from `dstrings.c` — QUITMSG (the language-sensitive one,
/// used as-is for non-English) plus the DOOM 1, DOOM II and Final DOOM
/// gags, and the internal debug placeholder the picker never reaches.
pub const QUIT_MESSAGES: [&str; NUM_QUITMESSAGES + 1] = [
    // DOOM1
    "are you sure you want to\nquit this great game?", // 0 — QUITMSG
    "please don't leave, there's more\ndemons to toast!",
    "let's beat it -- this is turning\ninto a bloodbath!",
    "i wouldn't leave if i were you.\ndos is much worse.",
    "you're trying to say you like dos\nbetter than me, right?",
    "don't leave yet -- there's a\ndemon around that corner!",
    "ya know, next time you come in here\ni'm gonna toast ya.",
    "go ahead and leave. see if i care.",
    // QuitDOOM II messages
    "you want to quit?\nthen, thou hast lost an eighth!",
    "don't go now, there's a \ndimensional shambler waiting\nat the dos prompt!",
    "get outta here and go back\nto your boring programs.",
    "if i were your boss, i'd \n deathmatch ya in a minute!",
    "look, bud. you leave now\nand you forfeit your body count!",
    "just leave. when you come\nback, i'll be waiting with a bat.",
    "you're lucky i don't smack\nyou for thinking about leaving.",
    // FinalDOOM?
    "fuck you, pussy!\nget the fuck out!",
    "you quit and i'll jizz\nin your cystholes!",
    "if you leave, i'll make\nthe lord drink my jizz.",
    "hey, ron! can we say\n'fuck' in the game?",
    "i'd leave: this is just\nmore monsters and levels.\nwhat a load.",
    "suck it down, asshole!\nyou're a fucking wimp!",
    "don't quit now! we're \nstill spending your money!",
    // Internal debug. Different style, too.
    "THIS IS NO MESSAGE!\nPage intentionally left blank.",
];

/// `M_QuitDOOM`: the message shown when the player picks "quit game".
/// English gets one of the random gags (never the debug entry); other
/// languages get the plain QUITMSG.
pub fn quit_message(gametic: u64, english: bool) -> String {
    let text = if english {
        QUIT_MESSAGES[(gametic % (NUM_QUITMESSAGES as u64 - 2)) as usize + 1]
    } else {
        QUIT_MESSAGES[0]
    };
    format!("{text}\n\n{DOSY}")
}
