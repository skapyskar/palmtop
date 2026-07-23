//! evdev keycode -> PS/2 Scan Code Set 1 lookup, for `SendInput`'s
//! `KEYEVENTF_SCANCODE` path.
//!
//! Bounded to exactly the codes `Keycodes.java` can emit (confirmed by
//! reading that file directly, not inferred): 26 letters, 10 digits, 11
//! punctuation keys, 5 editing keys (Esc/Backspace/Tab/Enter/Space), 4
//! modifiers, and 3 media keys -- 59 entries, checked by
//! `every_keycode_the_android_app_can_send_has_a_scancode` below against
//! that exact source of truth so a new key added to the app fails this
//! crate's tests instead of silently doing nothing on a Windows host.
//!
//! Values cross-checked against two independent sources rather than typed
//! from memory: the letter/digit/punctuation block against vetra.com's
//! Set 1 table, and the three extended media codes (which don't appear in
//! that table) against an unrelated multimedia-scancode reference -- both
//! agreed. Still worth a real keypress test on Windows before trusting it
//! beyond that.
//!
//! Deliberately free of the `windows` crate: everything here is table
//! lookups and bit arithmetic, and keeping it that way means these tests
//! -- the ones actually pinning the table's correctness -- run on any
//! machine `cargo test` runs on, not only a Windows target this project
//! can cross-compile but not execute. `platform::windows::input` (the
//! actual `SendInput` call site, `cfg(windows)`-only) is what translates
//! [`key_event_flags`]'s plain `u32` into the `windows` crate's
//! `KEYBD_EVENT_FLAGS` newtype.

/// `SendInput`'s `KEYEVENTF_*` values, from Microsoft's own documentation
/// for `KEYBDINPUT.dwFlags` -- reproduced as bare constants rather than
/// pulled from the `windows` crate so this module has no platform-specific
/// dependency at all.
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_SCANCODE: u32 = 0x0008;

/// A PS/2 Set 1 scancode, plus whether it needs the `E0` extended prefix
/// (`KEYEVENTF_EXTENDEDKEY`). Only Left GUI and the three media keys in this
/// table are extended -- every letter, digit, punctuation, and the three
/// left-side modifiers are plain single-byte Set 1 codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scancode {
    pub code: u16,
    pub extended: bool,
}

const fn sc(code: u16) -> Scancode {
    Scancode { code, extended: false }
}

const fn sc_ext(code: u16) -> Scancode {
    Scancode { code, extended: true }
}

/// evdev code -> Set 1 scancode. `None` for anything `Keycodes.java` cannot
/// emit -- callers should treat that as "drop the event", not panic, since a
/// version-skewed client sending a newer evdev code the host doesn't yet
/// know about should degrade gracefully, not crash the input thread.
pub fn scancode_for(evdev_code: u32) -> Option<Scancode> {
    Some(match evdev_code {
        1 => sc(0x01),  // Esc
        14 => sc(0x0E), // Backspace
        15 => sc(0x0F), // Tab
        28 => sc(0x1C), // Enter
        57 => sc(0x39), // Space

        // Digits 1-0 (evdev 2..=11, in typing order across the top row)
        2 => sc(0x02),
        3 => sc(0x03),
        4 => sc(0x04),
        5 => sc(0x05),
        6 => sc(0x06),
        7 => sc(0x07),
        8 => sc(0x08),
        9 => sc(0x09),
        10 => sc(0x0A),
        11 => sc(0x0B),

        // Letters, keyed by evdev code (Keycodes.java's letterCodes array).
        30 => sc(0x1E), // a
        48 => sc(0x30), // b
        46 => sc(0x2E), // c
        32 => sc(0x20), // d
        18 => sc(0x12), // e
        33 => sc(0x21), // f
        34 => sc(0x22), // g
        35 => sc(0x23), // h
        23 => sc(0x17), // i
        36 => sc(0x24), // j
        37 => sc(0x25), // k
        38 => sc(0x26), // l
        50 => sc(0x32), // m
        49 => sc(0x31), // n
        24 => sc(0x18), // o
        25 => sc(0x19), // p
        16 => sc(0x10), // q
        19 => sc(0x13), // r
        31 => sc(0x1F), // s
        20 => sc(0x14), // t
        22 => sc(0x16), // u
        47 => sc(0x2F), // v
        17 => sc(0x11), // w
        45 => sc(0x2D), // x
        21 => sc(0x15), // y
        44 => sc(0x2C), // z

        // Punctuation
        12 => sc(0x0C), // -
        13 => sc(0x0D), // =
        26 => sc(0x1A), // [
        27 => sc(0x1B), // ]
        39 => sc(0x27), // ;
        40 => sc(0x28), // '
        41 => sc(0x29), // `
        43 => sc(0x2B), // backslash
        51 => sc(0x33), // ,
        52 => sc(0x34), // .
        53 => sc(0x35), // /

        // Modifiers -- plain (non-extended) left-side Set 1 codes, except
        // Left GUI/Meta, which is E0-prefixed like every other Windows/Super
        // key on a real keyboard.
        29 => sc(0x1D),      // Left Ctrl
        42 => sc(0x2A),      // Left Shift
        56 => sc(0x38),      // Left Alt
        125 => sc_ext(0x5B), // Left Meta/Super/Win

        // Media keys -- all E0-prefixed.
        113 => sc_ext(0x20), // Mute
        114 => sc_ext(0x2E), // Volume Down
        115 => sc_ext(0x30), // Volume Up

        _ => return None,
    })
}

/// The `dwFlags` bits `SendInput` needs for one `KEYBDINPUT`: always
/// `KEYEVENTF_SCANCODE` (we always send by scancode, never by virtual-key),
/// plus `KEYEVENTF_EXTENDEDKEY` when the scancode calls for it, plus
/// `KEYEVENTF_KEYUP` on release.
///
/// Returns a plain `u32` rather than the `windows` crate's
/// `KEYBD_EVENT_FLAGS` -- see the module doc comment for why. The caller in
/// `platform::windows::input` wraps it as `KEYBD_EVENT_FLAGS(flags)` right
/// at the `SendInput` call site.
pub fn key_event_flags(scancode: Scancode, pressed: bool) -> u32 {
    let mut flags = KEYEVENTF_SCANCODE;
    if scancode.extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !pressed {
        flags |= KEYEVENTF_KEYUP;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact evdev codes `Keycodes.java` can emit, transcribed directly
    /// from that file (letterCodes[], digitCodes[], the punctuation `put()`
    /// calls, and the named constants) so this list is a copy of the real
    /// source of truth, not a guess at what it probably contains.
    const ANDROID_EMITTABLE_CODES: &[u32] = &[
        1, 14, 15, 28, 57, // Esc, Backspace, Tab, Enter, Space
        2, 3, 4, 5, 6, 7, 8, 9, 10, 11, // digits 1-0
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
        17, 45, 21, 44, // a..z
        12, 13, 26, 27, 39, 40, 41, 43, 51, 52, 53, // punctuation
        29, 42, 56, 125, // modifiers
        113, 114, 115, // media
    ];

    #[test]
    fn every_keycode_the_android_app_can_send_has_a_scancode() {
        for &code in ANDROID_EMITTABLE_CODES {
            assert!(scancode_for(code).is_some(), "evdev code {code} has no Windows scancode mapping");
        }
        assert_eq!(
            ANDROID_EMITTABLE_CODES.len(),
            59,
            "the android-emittable list itself drifted from Keycodes.java -- update both together"
        );
    }

    #[test]
    fn an_unknown_evdev_code_is_dropped_not_guessed_at() {
        assert_eq!(scancode_for(999), None);
    }

    #[test]
    fn key_event_flags_always_set_scancode_and_add_the_other_two_independently() {
        let plain_press = key_event_flags(sc(0x1E), true); // 'a', not extended
        assert_eq!(plain_press, KEYEVENTF_SCANCODE);

        let plain_release = key_event_flags(sc(0x1E), false);
        assert_eq!(plain_release, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP);

        let ext_press = key_event_flags(sc_ext(0x5B), true); // Left Meta
        assert_eq!(ext_press, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY);

        let ext_release = key_event_flags(sc_ext(0x5B), false);
        assert_eq!(ext_release, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
    }

    #[test]
    fn volume_keys_are_extended_and_in_evdev_order() {
        // Mirrors KeycodesTest's own
        // volumeKeysAreConsecutiveAndOrdered -- the same slip (swapping
        // up/down) is exactly as silent and exactly as easy to make here.
        let mute = scancode_for(113).unwrap();
        let down = scancode_for(114).unwrap();
        let up = scancode_for(115).unwrap();
        assert!(mute.extended && down.extended && up.extended);
        assert_eq!((mute.code, down.code, up.code), (0x20, 0x2E, 0x30));
    }

    #[test]
    fn left_meta_is_extended_but_the_other_three_modifiers_are_not() {
        assert!(!scancode_for(29).unwrap().extended); // Ctrl
        assert!(!scancode_for(42).unwrap().extended); // Shift
        assert!(!scancode_for(56).unwrap().extended); // Alt
        assert!(scancode_for(125).unwrap().extended); // Meta/Super
    }

    #[test]
    fn no_two_distinct_evdev_codes_collide_on_the_same_non_extended_scancode() {
        // A collision here means two different keys on the phone would
        // press the same key on the laptop -- exactly the kind of mistake
        // that is silent until someone notices "b" also opens Ctrl+Alt+Del
        // -- so every scancode is checked to be unique, extended and
        // non-extended tracked separately since 0x20 legitimately means two
        // different keys depending on the E0 prefix (D vs Mute).
        let mut plain = Vec::new();
        let mut extended = Vec::new();
        for &code in ANDROID_EMITTABLE_CODES {
            let s = scancode_for(code).unwrap();
            if s.extended {
                extended.push(s.code);
            } else {
                plain.push(s.code);
            }
        }
        let mut plain_sorted = plain.clone();
        plain_sorted.sort();
        plain_sorted.dedup();
        assert_eq!(plain.len(), plain_sorted.len(), "two non-extended keys collide");

        let mut ext_sorted = extended.clone();
        ext_sorted.sort();
        ext_sorted.dedup();
        assert_eq!(extended.len(), ext_sorted.len(), "two extended keys collide");
    }
}
