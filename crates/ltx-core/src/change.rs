//! Change identity (ADR-17 §3).
//!
//! A change id is opaque and machine-minted: 128 random bits rendered as 32
//! lowercase hex. It is *displayed* and *typed* as its shortest unique prefix,
//! never shorter than four characters, so the common path reads like `7f3a`
//! while the identity recorded in history stays globally distinct.
//!
//! Opaque rather than sequential or user-coined, and the reason is time rather
//! than collision. A change id is recorded in immutable history as the identity
//! of a unit of work and travels with a checkpoint over sync: `change 3` here
//! and `change 3` there would be two units of work under one identity, and a
//! name like `fix`, reused next month, would put two unrelated units of work
//! under one identity permanently, with no safe automatic repair. Nor is it
//! content-addressed, which would be self-defeating — the id would change on
//! every assign, the opposite of stable.
//!
//! Nothing here reaches for randomness. The 128 bits arrive as an argument from
//! the `Repo` boundary, so this module is pure and a test substitutes a counter
//! without there being a second code path for it to take.
//!
//! ```
//! use ltx_core::change::{self, Resolution};
//!
//! let live = [
//!     "7f3ac1d2e3f405162738495a6b7c8d9e",
//!     "7f3ac9000000000000000000000000ff",
//! ];
//! // Both begin `7f3ac`, so neither is distinguishable at the four-character
//! // floor: each abbreviation grows until it names one change and no other.
//! let short = change::abbreviate(live[0], &live);
//! assert_eq!(short, "7f3ac1");
//! assert_eq!(
//!     change::resolve(&short, &live),
//!     Resolution::One(live[0].to_string())
//! );
//! ```

/// Hex characters in a full change id: 128 bits, four bits to the character.
const ID_LEN: usize = 32;

/// The fewest characters a change id is ever shown or accepted as.
///
/// One floor serves both directions, which is what makes them inverses: since
/// `abbreviate` never returns fewer, a user is never shown a reference that
/// `resolve` would then refuse. It also keeps the empty string — and any near-
/// empty typo — from resolving to the only live change.
const MIN_PREFIX: usize = 4;

/// Render 128 bits as a change id.
pub fn mint(bits: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut id = String::with_capacity(ID_LEN);
    for byte in bits {
        id.push(HEX[(byte >> 4) as usize] as char);
        id.push(HEX[(byte & 0x0f) as usize] as char);
    }
    id
}

/// The shortest prefix of `id` that no other live change shares.
///
/// Never shorter than [`MIN_PREFIX`] characters, and never longer than `id`
/// itself — two ids differing only in their last character abbreviate to their
/// whole selves rather than to something that would resolve to both.
pub fn abbreviate(id: &str, live: &[&str]) -> String {
    // Cut only on character boundaries. An id this engine minted is 32 ASCII
    // hex and could be sliced at any byte, but the same call runs over ids read
    // back from a possibly-tampered op-log, where a fixed byte offset can fall
    // inside a multibyte codepoint and panic — in the path meant to report the
    // damage. Same doctrine as `short_id`.
    let cuts = id.char_indices().map(|(at, _)| at).chain([id.len()]);
    for end in cuts.skip(MIN_PREFIX) {
        let candidate = &id[..end];
        if !live
            .iter()
            .any(|other| *other != id && other.starts_with(candidate))
        {
            return candidate.to_string();
        }
    }
    id.to_string()
}

/// What a typed change reference names among the live changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one live change begins with it.
    One(String),
    /// No live change does.
    Unknown,
    /// Several do, in the order they were offered, so the caller can say which.
    Ambiguous(Vec<String>),
}

/// Resolve what a user typed against the changes live on their line.
///
/// The inverse of [`abbreviate`]: what that returns for an id, this returns the
/// id for.
pub fn resolve(typed: &str, live: &[&str]) -> Resolution {
    // Below the display floor nothing resolves at all. Without this the empty
    // string is a prefix of every id, and so would name the only live change.
    if typed.chars().count() < MIN_PREFIX {
        return Resolution::Unknown;
    }
    let mut matched: Vec<String> = live
        .iter()
        .filter(|id| id.starts_with(typed))
        .map(|id| id.to_string())
        .collect();
    match matched.len() {
        0 => Resolution::Unknown,
        1 => Resolution::One(matched.swap_remove(0)),
        _ => Resolution::Ambiguous(matched),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counter id source ADR-17 §3 names: the entropy a test substitutes
    /// for the real one. `at` places the counter, so a test can choose whether
    /// its ids diverge at the first character or the last.
    fn counted(at: usize, n: u8) -> String {
        let mut bits = [0u8; 16];
        bits[at] = n;
        mint(bits)
    }

    #[test]
    fn an_id_is_thirty_two_lowercase_hex_characters() {
        let id = mint([
            0x7f, 0x3a, 0xc1, 0xd2, 0xe3, 0xf4, 0x05, 0x16, 0x27, 0x38, 0x49, 0x5a, 0x6b, 0x7c,
            0x8d, 0x9e,
        ]);
        assert_eq!(id, "7f3ac1d2e3f405162738495a6b7c8d9e");
    }

    #[test]
    fn a_byte_below_sixteen_keeps_its_leading_zero() {
        // Rendering each byte in one character where it fits would produce ids
        // of varying length, and two different bit patterns could then render
        // the same id — a collision minted by the formatting, not the entropy.
        assert_eq!(mint([1u8; 16]), "01".repeat(16));
        assert_eq!(mint([0u8; 16]).len(), ID_LEN);
    }

    #[test]
    fn an_id_sharing_nothing_abbreviates_to_the_four_character_floor() {
        let a = "7f3ac1d2e3f405162738495a6b7c8d9e";
        let b = "0011223344556677889900aabbccddee";
        assert_eq!(abbreviate(a, &[a, b]), "7f3a");
    }

    #[test]
    fn an_abbreviation_grows_until_it_names_one_change() {
        let a = "7f3ac1d2e3f405162738495a6b7c8d9e";
        let b = "7f3ac9000000000000000000000000ff";
        assert_eq!(abbreviate(a, &[a, b]), "7f3ac1");
    }

    #[test]
    fn ids_differing_only_at_the_last_character_abbreviate_to_themselves() {
        let a = counted(15, 1);
        let b = counted(15, 2);
        assert_eq!(abbreviate(&a, &[&a, &b]), a);
        assert_eq!(
            a.len(),
            ID_LEN,
            "and that is the whole id, not a truncation"
        );
    }

    #[test]
    fn every_abbreviation_resolves_back_to_the_change_it_came_from() {
        // The property the whole scheme rests on: what a user is shown is what
        // a user can type. Both divergence points are drawn — ids that differ
        // at the first character and ids that differ only at the last — since
        // the late-divergence case is the one that forces the prefix to grow.
        let ids: Vec<String> = (1..=20u8)
            .flat_map(|n| [counted(0, n), counted(15, n)])
            .collect();
        let live: Vec<&str> = ids.iter().map(String::as_str).collect();

        for id in &ids {
            let shown = abbreviate(id, &live);
            assert!(
                shown.chars().count() >= MIN_PREFIX,
                "{id} was shown as `{shown}`, below the floor a user may type"
            );
            assert_eq!(
                resolve(&shown, &live),
                Resolution::One(id.clone()),
                "`{shown}` was shown for {id} but does not resolve back to it"
            );
        }
    }

    #[test]
    fn a_prefix_several_changes_share_resolves_to_all_of_them() {
        let a = "7f3ac1d2e3f405162738495a6b7c8d9e";
        let b = "7f3ac9000000000000000000000000ff";
        let c = "0011223344556677889900aabbccddee";
        assert_eq!(
            resolve("7f3a", &[a, b, c]),
            Resolution::Ambiguous(vec![a.to_string(), b.to_string()])
        );
    }

    #[test]
    fn a_reference_below_the_floor_names_nothing_even_when_only_one_change_exists() {
        // The empty string is a prefix of every id. Resolving it to the sole
        // live change would make `--to ""` a silent alias for whichever change
        // happened to be open.
        let only = "7f3ac1d2e3f405162738495a6b7c8d9e";
        assert_eq!(resolve("", &[only]), Resolution::Unknown);
        assert_eq!(resolve("7f3", &[only]), Resolution::Unknown);
    }

    #[test]
    fn a_reference_matching_no_live_change_is_unknown_rather_than_a_guess() {
        let only = "7f3ac1d2e3f405162738495a6b7c8d9e";
        assert_eq!(resolve("dead", &[only]), Resolution::Unknown);
        assert_eq!(resolve(&"f".repeat(ID_LEN), &[only]), Resolution::Unknown);
    }

    #[test]
    fn abbreviating_an_id_that_is_not_hex_cuts_on_a_character_rather_than_a_byte() {
        // Change ids are read back from the op-log, which G1.1 assumes can be
        // damaged. Here the fourth BYTE falls inside a two-byte character, so
        // a cut taken at the floor in bytes would panic — in the code that
        // would display the damage rather than in the damage itself. The floor
        // is four CHARACTERS, and this abbreviates to four of them.
        let damaged = "007é1d2e3f405162738495a6b7c8d9e";
        let other = "7f3ac1d2e3f405162738495a6b7c8d9e";
        assert_eq!(abbreviate(damaged, &[damaged, other]), "007é");
    }

    #[test]
    fn an_id_shorter_than_the_floor_abbreviates_to_itself() {
        // A truncated id cannot be padded up to the floor, and returning a
        // prefix of it would name a change that does not exist.
        let truncated = "7f3";
        assert_eq!(abbreviate(truncated, &[truncated]), "7f3");
    }
}
