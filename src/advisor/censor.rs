//! "Censor profanity": which letters of a profane word the advisor hides.
//!
//! The game censors a profane word by drawing the whole word strip through a mosaic
//! (`shaders/censor.vs`, set in `wordboard.lua:254-258` while `noProfanity` is on), which hides
//! every letter. That works there because the player built the word. The advisor has to tell the
//! player the word, so it hides only some letters (the user's design, 2026-11-08):
//!
//! - **The vowels** `a e i o u`, wherever they are, the first letter included. This is the
//!   newspaper convention, and the hidden letters are the commonest tiles, the easiest to find on
//!   the board.
//! - **`y`, when neither neighbour is a vowel**: where it sounds as a vowel (`gym`, `sexy`), not
//!   where it is a consonant beside one (`yes`, `bayou`).
//! - **If that hides nothing** (a word with no vowel), every other letter from the second.
//!
//! The binary draws each hidden letter's tile as the game's mosaic: see `mosaic` in
//! `src/bin/diggle_advisor.rs`.

/// The five letters that are always vowels.
fn plain_vowel(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u')
}

/// For each character of `word`, whether it is hidden. Case does not matter.
pub fn hidden(word: &str) -> Vec<bool> {
    let chars: Vec<char> = word.chars().collect();
    let vowel_beside = |i: usize| {
        (i > 0 && plain_vowel(chars[i - 1])) || chars.get(i + 1).is_some_and(|&c| plain_vowel(c))
    };
    let mask: Vec<bool> = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| plain_vowel(c) || (c.eq_ignore_ascii_case(&'y') && !vowel_beside(i)))
        .collect();
    if mask.contains(&true) {
        mask
    } else {
        (0..chars.len()).map(|i| i % 2 == 1).collect()
    }
}

/// `word` with its hidden letters as `*`, for the places the word is text rather than tiles.
pub fn masked(word: &str) -> String {
    word.chars().zip(hidden(word)).map(|(c, h)| if h { '*' } else { c }).collect()
}

/// For each tile of a word, whether it is drawn censored. `spans` is how many of the word's
/// letters each tile carries, in order: a ligature tile (`TH`, `ING`) carries more than one, and
/// is censored when any of its letters is hidden. If the spans do not add up to the word, every
/// tile is censored, since showing a letter that should be hidden is the worse mistake.
pub fn tiles_hidden(word: &str, spans: &[usize]) -> Vec<bool> {
    let mask = hidden(word);
    if spans.iter().sum::<usize>() != mask.len() {
        return vec![true; spans.len()];
    }
    let mut at = 0;
    spans
        .iter()
        .map(|&n| {
            let any = mask[at..at + n].contains(&true);
            at += n;
            any
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every word here is a clean one: the rule is about letters, not about what they spell.

    #[test]
    fn vowels_are_hidden_the_first_letter_included() {
        assert_eq!(masked("TABLE"), "T*BL*");
        assert_eq!(masked("ocean"), "*c**n");
        assert_eq!(masked("Apple"), "*ppl*");
    }

    #[test]
    fn y_is_hidden_only_when_no_neighbour_is_a_vowel() {
        assert_eq!(masked("GYM"), "G*M", "between consonants it is a vowel");
        assert_eq!(masked("LADY"), "L*D*", "after a consonant at the end, a vowel");
        assert_eq!(masked("YES"), "Y*S", "before a vowel it is a consonant");
        assert_eq!(masked("BAYOU"), "B*Y**", "between vowels, a consonant");
        assert_eq!(masked("TOY"), "T*Y", "after a vowel, a consonant");
        assert_eq!(masked("Y"), "*", "alone, nothing beside it is a vowel");
    }

    #[test]
    fn a_word_with_nothing_to_hide_falls_back_to_every_other_letter_from_the_second() {
        assert_eq!(masked("TSK"), "T*K");
        assert_eq!(masked("PFFT"), "P*F*");
        assert_eq!(masked("HM"), "H*");
    }

    #[test]
    fn something_is_always_hidden_once_a_word_has_two_letters() {
        for w in ["TSK", "SHH", "BRR", "GYP", "AYE", "OY", "YAY", "QUEUE", "RHYTHM"] {
            assert!(hidden(w).contains(&true), "{w} shows in full");
        }
    }

    #[test]
    fn a_ligature_tile_is_censored_when_any_of_its_letters_is_hidden() {
        // T H I N G: TH on one tile, ING on another, as the board can carry them.
        assert_eq!(tiles_hidden("THING", &[2, 3]), vec![false, true]);
        assert_eq!(tiles_hidden("THING", &[1, 1, 1, 1, 1]), vec![false, false, true, false, false]);
    }

    #[test]
    fn spans_that_do_not_fit_the_word_censor_every_tile() {
        assert_eq!(tiles_hidden("THING", &[2, 2]), vec![true, true]);
    }
}
