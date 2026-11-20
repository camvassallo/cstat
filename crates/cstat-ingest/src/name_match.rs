//! Shared pieces of the name linkers that bind a third-party player row
//! (a Torvik season row, a 247 portal entry) to a cstat `players` row once
//! exact-name matching has run out.
//!
//! The pairing rule lives here so the linkers cannot drift apart: a fix to how
//! ambiguity is refused reaches every caller at once.
//!
//! Also the player identity rules the CollegeBasketballData path leans on
//! (`map-espn-ids`, `gap-fill`, the NatStat adoption of a CBBD-created
//! player): one name key ([`name_key`]), Torvik's spelling of a player as a
//! second name for him ([`TORVIK_ALIASES_SQL`]), spelling variants
//! ([`names_compatible`]) and the pairing that accepts a match only when it is
//! the sole one on both sides ([`pair_players`]). None of them needs NatStat:
//! NatStat's spelling is one name among others, never the required one.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Pair source rows to candidates that share a grouping key, accepting a group
/// only when it holds exactly one row AND exactly one candidate and `guard`
/// agrees with that pairing. Returns `(row index, candidate index)` pairs,
/// sorted by row index.
///
/// Requiring uniqueness on the source side as well as the candidate side is
/// what makes the pass order-independent: with at most one claimant and at
/// most one candidate per key, no two rows can ever contend for the same
/// player, so the result doesn't depend on which row is visited first.
///
/// Callers pass only rows still unmatched and candidates still unclaimed, and
/// apply the claims themselves.
pub fn pair_unambiguous<K: Eq + Hash>(
    rows: &[K],
    candidates: &[K],
    guard: impl Fn(usize, usize) -> bool,
) -> Vec<(usize, usize)> {
    let mut groups: HashMap<&K, Vec<usize>> = HashMap::new();
    for (i, k) in rows.iter().enumerate() {
        groups.entry(k).or_default().push(i);
    }
    let mut pool: HashMap<&K, Vec<usize>> = HashMap::new();
    for (i, k) in candidates.iter().enumerate() {
        pool.entry(k).or_default().push(i);
    }
    let mut out: Vec<(usize, usize)> = groups
        .into_iter()
        .filter_map(
            |(k, group)| match (group.as_slice(), pool.get(k)?.as_slice()) {
                ([row], [cand]) if guard(*row, *cand) => Some((*row, *cand)),
                _ => None,
            },
        )
        .collect();
    out.sort_unstable();
    out
}

/// SQL for the other spellings of player `p` (a `players` row aliased `p`):
/// the names of the Torvik season rows linked to him, where they differ from
/// his. Torvik spells a player independently of NatStat and often as ESPN
/// does (`Carlos Varela`, `Xabi Lopez`, `R.J. Newton` for NatStat's `Carlos
/// Varela Romero`, `Xabier Lopez Rodriguez`, `Roderick Newton`), so his
/// Torvik name is a second key for him that no NatStat call is needed for.
pub const TORVIK_ALIASES_SQL: &str = "ARRAY(SELECT DISTINCT tps.player_name \
     FROM torvik_player_stats tps \
     WHERE tps.player_id = p.id AND tps.season = p.season AND tps.player_name <> p.name)";

/// Name tokens that are a generational suffix, not a name: dropped wherever
/// they stand except first (`Nicholas Sangermano II`, `Cello Jackson Jr.`).
pub(crate) const SUFFIXES: [&str; 7] = ["jr", "sr", "ii", "iii", "iv", "v", "lll"];

/// A name as identity tokens: diacritics folded ([`fold_diacritics`]),
/// punctuation dropped (`C.J.` is `cj`), a hyphen splitting the parts it joins
/// (`Perez-Rouzan` is `perez rouzan`), and generational suffixes dropped.
pub fn name_tokens(name: &str) -> Vec<String> {
    let spaced: String = name
        .chars()
        .map(|c| {
            if matches!(c, '-' | '\u{2010}' | '\u{2011}' | '\u{2013}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    fold_diacritics(&spaced)
        .split_whitespace()
        .enumerate()
        .filter(|(i, t)| *i == 0 || !SUFFIXES.contains(t))
        .map(|(_, t)| t.to_string())
        .collect()
}

/// A name's token lists: the name itself, and for a name carrying a quoted
/// nickname (`Lawrence "LJ" Katz`) the name without it (`Lawrence Katz`) and
/// the nickname in place of the given name (`LJ Katz`). The first is always
/// the name as written.
fn name_forms(name: &str) -> Vec<Vec<String>> {
    let mut forms = vec![name_tokens(name)];
    let quotes = |c: char| matches!(c, '"' | '\u{201C}' | '\u{201D}');
    let mut parts = name.split(quotes);
    if let (Some(before), Some(nick), Some(after), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    {
        let surname: Vec<&str> = after.split_whitespace().collect();
        let given: Vec<&str> = before.split_whitespace().collect();
        if !nick.trim().is_empty() && !surname.is_empty() && !given.is_empty() {
            let rest = surname.join(" ");
            forms.push(name_tokens(&format!("{} {rest}", given.join(" "))));
            forms.push(name_tokens(&format!("{} {rest}", nick.trim())));
        }
    }
    forms
}

/// The exact-match key for a player name: [`name_tokens`] run together, so
/// `Jalen St Clair` meets `Jalen StClair`, `Ylan Esso-Essis` meets `Ylan Esso
/// Essis`, `C.J. Smith` meets `CJ Smith` and `Roko Prkačin` meets `Roko
/// Prkacin`. Empty for a name with no letters, which never matches.
pub fn name_key(name: &str) -> String {
    name_tokens(name).concat()
}

/// Shortest given name that may stand for a longer one it begins
/// (`Cam` / `Cameron`, `Xabi` / `Xabier`).
const GIVEN_PREFIX_MIN: usize = 3;
/// Shortest surname that may stand for a compound one it begins or ends
/// (`Varela` / `Varela Romero`, `Rouzan` / `Perez-Rouzan`).
const SURNAME_PART_MIN: usize = 4;
/// Shortest name part a one-letter misspelling is accepted on
/// (`Skytt` / `Skytta`, `Terrance` / `Terrence`).
const ONE_EDIT_MIN: usize = 5;

/// Whether two spellings may be one person's: one half of the name (given
/// name, or surname) identical, the other a variant of it. Never both halves
/// at once, so `Jay Smyth` is not `Jaylen Smith`.
///
/// - **Given-name variants**, with the surname identical once run together:
///   one given name begins the other (`Cam` / `Cameron`, `Matt` / `Matthew`,
///   at least [`GIVEN_PREFIX_MIN`] letters), one letter apart on names of
///   [`ONE_EDIT_MIN`] or more (`Javantae` / `Javante`), or one nickname
///   group ([`NICKNAMES`]: `Mike` / `Michael`, `Ike` / `Isaac`).
/// - **Surname variants**, with the given name identical: one surname is the
///   other's leading or trailing parts (`Varela` / `Varela Romero`, `Rouzan` /
///   `Perez-Rouzan`, `Nedal` / `Nedal Abdelrahman`, at least
///   [`SURNAME_PART_MIN`] letters), or one letter apart (`Guimares` /
///   `Guimaraes`, `Krianauskas` / `Krizanauskas`).
///
/// A one-word name (no surname) is never a variant. This is a statement about
/// two names, not about two people: callers accept a variant only when it is
/// the sole one on both sides ([`pair_players`]) and say what else vouches
/// for it.
pub fn names_compatible(a: &str, b: &str) -> bool {
    tokens_compatible(&name_tokens(a), &name_tokens(b))
}

fn tokens_compatible(a: &[String], b: &[String]) -> bool {
    let (Some((ga, sa)), Some((gb, sb))) = (a.split_first(), b.split_first()) else {
        return false;
    };
    if sa.is_empty() || sb.is_empty() {
        return false;
    }
    let given_same = ga == gb;
    let surname_same = sa.concat() == sb.concat();
    match (given_same, surname_same) {
        (true, true) => true,
        (false, true) => given_variant(ga, gb),
        (true, false) => surname_variant(sa, sb),
        (false, false) => false,
    }
}

fn given_variant(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    (short.chars().count() >= GIVEN_PREFIX_MIN && long.starts_with(short))
        || one_edit(a, b)
        || NICKNAMES
            .iter()
            .any(|group| group.contains(&a) && group.contains(&b))
}

/// Given names that are one name, where neither begins the other (those need
/// no entry: `Matt` / `Matthew`). Kept to the forms a box score uses.
const NICKNAMES: &[&[&str]] = &[
    &["michael", "mike", "mikey"],
    &["jacob", "jake"],
    &["nathan", "nathaniel", "nate"],
    &["charles", "charlie", "chuck"],
    &["isaac", "ike"],
    &["frederick", "fredrick", "freddy", "freddie"],
    &["nicholas", "nicolas", "nick", "nicky"],
    &["william", "bill", "billy", "will", "willie"],
    &["robert", "rob", "bob", "bobby", "robbie"],
    &["james", "jim", "jimmy"],
    &["joseph", "joe", "joey"],
    &["anthony", "tony"],
    &["richard", "rich", "rick", "ricky"],
    &["andrew", "andy", "drew"],
    &["thomas", "tom", "tommy"],
    &["edward", "eddie", "ed"],
    &["daniel", "danny", "dan"],
    &["gabriel", "gabe"],
    &["ezekiel", "zeke"],
    &["lawrence", "larry"],
    &["emmanuel", "manny"],
    &["christopher", "kristopher", "chris", "kris"],
];

fn surname_variant(a: &[String], b: &[String]) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if short.len() < long.len() && short.concat().chars().count() >= SURNAME_PART_MIN {
        let n = short.len();
        if long[..n] == *short || long[long.len() - n..] == *short {
            return true;
        }
    }
    one_edit(&a.concat(), &b.concat())
}

/// Exactly one insertion, deletion or substitution apart, on names of at
/// least [`ONE_EDIT_MIN`] letters.
fn one_edit(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b || a.len().min(b.len()) < ONE_EDIT_MIN || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let prefix = short
        .iter()
        .zip(long.iter())
        .take_while(|(x, y)| x == y)
        .count();
    if short.len() == long.len() {
        short[prefix + 1..] == long[prefix + 1..]
    } else {
        short[prefix..] == long[prefix + 1..]
    }
}

/// How [`pair_players`] paired a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PairedBy {
    /// The row's own name, exactly ([`name_key`]).
    Name,
    /// Another name the row is known by, exactly: an alias passed in
    /// (Torvik's spelling) or a form of a quoted nickname (`Lawrence "LJ"
    /// Katz` is also `LJ Katz`).
    Alias,
    /// A last name unique on both sides among the rows left.
    LastName,
    /// A spelling variant ([`names_compatible`]) unique on both sides.
    Variant,
}

/// Which pairing passes [`pair_players`] runs after the exact one.
#[derive(Debug, Clone, Copy)]
pub struct Passes {
    /// A last name unique on both sides among the rows left. Safe only between
    /// two lists of the same people (two box scores of one game).
    pub last_name: bool,
    /// A spelling variant unique on both sides.
    pub variants: bool,
}

/// Pair our players to another source's lines. Each of `ours` is a player's
/// names: his own first, then any aliases (Torvik's spelling of him). Passes,
/// each over the rows the earlier ones left:
///
/// 1. **Exact**, on [`name_key`] over every name a row has.
/// 2. **Last name** ([`Passes::last_name`]), as `pair_box_names` always has.
/// 3. **Variant** ([`Passes::variants`], [`names_compatible`]).
///
/// Every pass pairs a row and a line only when each is the other's sole
/// match. In the variant pass that uniqueness is counted against every row and
/// line, paired or not, so a line that could be an already-paired player as
/// well as an open one pairs with neither. `guard(our row, their line)` can
/// veto a last-name or variant pair (`map-espn-ids` requires the two lines'
/// points to agree); the exact pass is never vetoed. Returns `(ours, theirs,
/// how)`, sorted.
pub fn pair_players<A: AsRef<str>, B: AsRef<str>>(
    ours: &[Vec<A>],
    theirs: &[B],
    passes: Passes,
    guard: impl Fn(usize, usize) -> bool,
) -> Vec<(usize, usize, PairedBy)> {
    let toks_o: Vec<Vec<Vec<String>>> = ours
        .iter()
        .map(|names| names.iter().flat_map(|n| name_forms(n.as_ref())).collect())
        .collect();
    let toks_t: Vec<Vec<String>> = theirs.iter().map(|n| name_tokens(n.as_ref())).collect();
    let key_t: Vec<String> = toks_t.iter().map(|t| t.concat()).collect();
    let mut pairs: Vec<(usize, usize, PairedBy)> = Vec::new();

    // 1. Exact, over every name of ours.
    let exact = |o: usize, t: usize| -> Option<PairedBy> {
        if key_t[t].is_empty() {
            return None;
        }
        let pos = toks_o[o].iter().position(|n| n.concat() == key_t[t])?;
        Some(if pos == 0 {
            PairedBy::Name
        } else {
            PairedBy::Alias
        })
    };
    let all_o: Vec<usize> = (0..ours.len()).collect();
    let all_t: Vec<usize> = (0..theirs.len()).collect();
    for (o, t) in mutual_unique(&all_o, &all_t, &all_o, &all_t, |o, t| exact(o, t).is_some()) {
        let how = exact(o, t).unwrap_or(PairedBy::Name);
        pairs.push((o, t, how));
    }

    // 2. Last name, among the rows left.
    if passes.last_name {
        let (left_o, left_t) = left(&pairs, ours.len(), theirs.len());
        let last = |t: &Vec<String>| t.last().cloned().unwrap_or_default();
        let found = mutual_unique(&left_o, &left_t, &left_o, &left_t, |o, t| {
            let lt = last(&toks_t[t]);
            !lt.is_empty() && toks_o[o].iter().any(|n| last(n) == lt)
        });
        pairs.extend(
            found
                .into_iter()
                .filter(|&(o, t)| guard(o, t))
                .map(|(o, t)| (o, t, PairedBy::LastName)),
        );
    }

    // 3. Variants: open rows only, but unique against everyone.
    if passes.variants {
        let (left_o, left_t) = left(&pairs, ours.len(), theirs.len());
        let found = mutual_unique(&left_o, &left_t, &all_o, &all_t, |o, t| {
            toks_o[o].iter().any(|n| tokens_compatible(n, &toks_t[t]))
        });
        pairs.extend(
            found
                .into_iter()
                .filter(|&(o, t)| guard(o, t))
                .map(|(o, t)| (o, t, PairedBy::Variant)),
        );
    }
    pairs.sort_unstable_by_key(|&(o, t, _)| (o, t));
    pairs
}

/// Rows and lines no pair holds yet.
fn left(pairs: &[(usize, usize, PairedBy)], n_o: usize, n_t: usize) -> (Vec<usize>, Vec<usize>) {
    let taken_o: HashSet<usize> = pairs.iter().map(|p| p.0).collect();
    let taken_t: HashSet<usize> = pairs.iter().map(|p| p.1).collect();
    (
        (0..n_o).filter(|i| !taken_o.contains(i)).collect(),
        (0..n_t).filter(|i| !taken_t.contains(i)).collect(),
    )
}

/// `(o, t)` from `open_o` x `open_t` where `matches(o, t)`, `t` is the only
/// one of `pool_t` that `o` matches and `o` the only one of `pool_o` that `t`
/// matches. The open lists are subsets of the pools.
fn mutual_unique(
    open_o: &[usize],
    open_t: &[usize],
    pool_o: &[usize],
    pool_t: &[usize],
    matches: impl Fn(usize, usize) -> bool,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for &o in open_o {
        let mut hits = pool_t.iter().copied().filter(|&t| matches(o, t));
        let (Some(t), None) = (hits.next(), hits.next()) else {
            continue;
        };
        if !open_t.contains(&t) {
            continue;
        }
        let mut back = pool_o.iter().copied().filter(|&o2| matches(o2, t));
        if let (Some(o2), None) = (back.next(), back.next())
            && o2 == o
        {
            out.push((o, t));
        }
    }
    out
}

/// Pair two box scores' players for one game side: exact name ([`name_key`])
/// first, then a last name unique on both sides among those left. Returns
/// `(ours, theirs)` index pairs, sorted. An ambiguous name pairs nothing.
///
/// `source-parity`'s pairing. `map-espn-ids` pairs with [`pair_players`],
/// which adds Torvik's spelling of each player and spelling variants to these
/// two passes.
pub fn pair_box_names<A: AsRef<str>, B: AsRef<str>>(
    ours: &[A],
    theirs: &[B],
) -> Vec<(usize, usize)> {
    let ours: Vec<Vec<&str>> = ours.iter().map(|n| vec![n.as_ref()]).collect();
    pair_players(
        &ours,
        theirs,
        Passes {
            last_name: true,
            variants: false,
        },
        |_, _| true,
    )
    .into_iter()
    .map(|(o, t, _)| (o, t))
    .collect()
}

/// Lowercase a name and fold its diacritics to ASCII, dropping punctuation
/// (periods, apostrophes — including the curly `’` U+2019 and the Windows-1252
/// mojibake control char U+0092 we see in a few DB rows). German umlauts and
/// ß expand to digraphs; all other Latin diacritics fold to their base letter.
/// Alphabetic characters with no mapping pass through lowercased; everything
/// else (punctuation, control chars) is dropped.
///
/// Shared by the Torvik linker (`ingest::torvik`) and [`name_tokens`], so a
/// diacritic the one folds the other folds the same way.
pub fn fold_diacritics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            // German umlauts / eszett → digraph romanization
            'ä' | 'Ä' | 'æ' | 'Æ' => out.push_str("ae"),
            'ö' | 'Ö' | 'œ' | 'Œ' => out.push_str("oe"),
            'ü' | 'Ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            'þ' | 'Þ' => out.push_str("th"),
            // Latin diacritics → base letter
            'á' | 'à' | 'â' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' | 'Á' | 'À' | 'Â' | 'Ã' | 'Å' | 'Ā'
            | 'Ă' | 'Ą' => out.push('a'),
            'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' | 'Ç' | 'Ć' | 'Č' | 'Ĉ' | 'Ċ' => out.push('c'),
            'đ' | 'ď' | 'ð' | 'Đ' | 'Ď' | 'Ð' => out.push('d'),
            'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'É' | 'È' | 'Ê' | 'Ë' | 'Ē'
            | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => out.push('e'),
            'ğ' | 'ĝ' | 'ġ' | 'ģ' | 'Ğ' | 'Ĝ' | 'Ġ' | 'Ģ' => out.push('g'),
            'í' | 'ì' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' | 'Í' | 'Ì' | 'Î' | 'Ï' | 'Ī' | 'Ĭ'
            | 'Į' | 'İ' => out.push('i'),
            'ł' | 'ĺ' | 'ļ' | 'ľ' | 'Ł' | 'Ĺ' | 'Ļ' | 'Ľ' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' | 'Ñ' | 'Ń' | 'Ņ' | 'Ň' => out.push('n'),
            'ó' | 'ò' | 'ô' | 'õ' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ø' | 'Ō'
            | 'Ŏ' | 'Ő' => out.push('o'),
            'ŕ' | 'ř' | 'Ŕ' | 'Ř' => out.push('r'),
            'ś' | 'š' | 'ş' | 'ŝ' | 'ș' | 'Ś' | 'Š' | 'Ş' | 'Ŝ' | 'Ș' => out.push('s'),
            'ţ' | 'ť' | 'ț' | 'Ţ' | 'Ť' | 'Ț' => out.push('t'),
            'ú' | 'ù' | 'û' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'Ú' | 'Ù' | 'Û' | 'Ū' | 'Ŭ' | 'Ů'
            | 'Ű' | 'Ų' => out.push('u'),
            'ý' | 'ÿ' | 'Ý' | 'Ÿ' => out.push('y'),
            'ź' | 'ž' | 'ż' | 'Ź' | 'Ž' | 'Ż' => out.push('z'),
            _ if c.is_alphabetic() || c.is_whitespace() => {
                out.extend(c.to_lowercase());
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_names_pair_by_name_then_unique_last_name() {
        // NatStat drops the suffix ("Abdi Bashir"); CBBD keeps it. Two Smiths
        // on one side make "smith" ambiguous, so neither pairs by last name.
        let ours = ["Abdi Bashir", "DK Thorn", "Al Smith", "Bo Smith"];
        let theirs = [
            "Abdi Bashir Jr.",
            "Dekedran Thorn",
            "Alan Smith",
            "Bob Smith",
        ];
        assert_eq!(pair_box_names(&ours, &theirs), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn box_names_refuse_a_duplicated_full_name() {
        // Two identical names on one side: neither pairs, by name or last name.
        let ours = ["Jake Davis", "Jake Davis"];
        let theirs = ["Jake Davis"];
        assert!(pair_box_names(&ours, &theirs).is_empty());
    }

    #[test]
    fn pairs_only_groups_unique_on_both_sides() {
        let rows = ["a", "b", "b", "c"];
        let cands = ["a", "b", "c", "c"];
        // a: 1 row / 1 cand → paired. b: 2 rows → refused. c: 2 cands → refused.
        assert_eq!(pair_unambiguous(&rows, &cands, |_, _| true), vec![(0, 0)]);
    }

    #[test]
    fn guard_can_veto_a_unique_pair() {
        let rows = ["a"];
        let cands = ["a"];
        assert!(pair_unambiguous(&rows, &cands, |_, _| false).is_empty());
    }

    #[test]
    fn name_keys_fold_spelling_noise() {
        for (a, b) in [
            ("C.J. Smith", "CJ Smith"),
            ("Roko Prkačin", "Roko Prkacin"),
            ("Aleksej Kostić", "Aleksej Kostic"),
            ("Jalen St Clair", "Jalen StClair"),
            ("Ylan Esso-Essis", "Ylan Esso Essis"),
            ("Da'Shawn Hall-Johnson", "Dashawn Hall Johnson"),
            ("Nicholas Sangermano II", "Nicholas Sangermano"),
            ("Cello Jackson Jr.", "Cello Jackson Jr"),
        ] {
            assert_eq!(name_key(a), name_key(b), "{a} / {b}");
        }
        assert_eq!(
            name_tokens("Daniel Perez-Rouzan"),
            ["daniel", "perez", "rouzan"]
        );
        assert_eq!(name_key("123"), "");
    }

    #[test]
    fn spelling_variants_vary_one_half_of_the_name() {
        // The players the CBBD gap-fill left unresolved in November 2026.
        for (a, b) in [
            ("Carlos Varela", "Carlos Varela Romero"),
            ("Daniel Perez-Rouzan", "Daniel Rouzan"),
            ("Ahmed Nedal", "Ahmed Nedal Abdelrahman"),
            ("Andy Ramirez", "Andy Ramirez Novas"),
            ("Gustavo Guimares", "Gustavo Guimarães"),
            ("Emil Skytt", "Emil Skytta"),
            ("Gantas Krianauskas", "Gantas Krizanauskas"),
            ("Mekhi Conner", "Mekhi Connor"),
            ("Cam Hiatt", "Cameron Hiatt"),
            ("Matt Goodwin", "Matthew Goodwin"),
            ("Tayelor Moore", "Taye Moore"),
            ("Javantae Edwards", "Javante Edwards"),
            ("Terrance Hampton", "Terrence Hampton Jr."),
            ("Aleksandar Zecevic", "Aleksander Zečević"),
            ("Nate Ahner", "Nathan Ahner"),
            ("Mike Collins", "Michael Collins"),
            ("Nicholas Sangermano II", "Nick Sangermano"),
        ] {
            assert!(names_compatible(a, b), "{a} / {b}");
            assert!(names_compatible(b, a), "{b} / {a}");
        }
        for (a, b) in [
            // Different people with one half in common.
            ("Miles White", "Miles Elliott"),
            ("Keeper Jackson", "Jaden Jackson"),
            ("Chris Jones", "Chris Walker"),
            // Both halves varied at once.
            ("Jay Smyth", "Jaylen Smith"),
            ("Xabi Lopez", "Xabier Lopez Rodriguez"),
            // Nicknames no table lists, and initials: left to aliases.
            ("Obie Bronston", "Owen Bronston"),
            ("Bear Cherry", "Jeremiah Cherry"),
            ("BJ Roy", "Brandon Roy"),
            // A nickname whose surname differs.
            ("Mike Collins", "Michael Collier"),
            // Too short to stand for more.
            ("Al Smith", "Alan Smith"),
            ("Jo Le", "Jo Le Blanc"),
            ("Ivan Kim", "Ivan Kin"),
            // One-word names have no surname to compare.
            ("Neymar", "Neymar Jr"),
        ] {
            assert!(!names_compatible(a, b), "{a} / {b}");
        }
    }

    fn names(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    const ALL: Passes = Passes {
        last_name: true,
        variants: true,
    };

    #[test]
    fn players_pair_by_name_alias_last_name_then_variant() {
        let ours = names(&[
            &["Abdi Bashir"],
            &["Xabier Lopez Rodriguez", "Xabi Lopez"],
            &["Roderick Thorn"],
            &["Carlos Varela Romero"],
        ]);
        let theirs = [
            "Abdi Bashir Jr.",
            "Xabi Lopez",
            "Dekedran Thorn",
            "Carlos Varela",
        ];
        assert_eq!(
            pair_players(&ours, &theirs, ALL, |_, _| true),
            vec![
                (0, 0, PairedBy::Name),
                (1, 1, PairedBy::Alias),
                (2, 2, PairedBy::LastName),
                (3, 3, PairedBy::Variant),
            ]
        );
        // The guard vetoes the last-name and variant passes, never the exact.
        assert_eq!(
            pair_players(&ours, &theirs, ALL, |_, _| false),
            vec![(0, 0, PairedBy::Name), (1, 1, PairedBy::Alias)]
        );
        // Without the optional passes only the exact ones pair.
        let exact_only = Passes {
            last_name: false,
            variants: false,
        };
        assert_eq!(
            pair_players(&ours, &theirs, exact_only, |_, _| true).len(),
            2
        );
    }

    #[test]
    fn a_quoted_nickname_is_another_name() {
        let ours = names(&[&["Lawrence \"LJ\" Katz"], &["Samuel Mbingazo"]]);
        assert_eq!(
            pair_players(&ours, &["LJ Katz", "Lawrence Katz"], ALL, |_, _| true),
            // Both forms are his, so neither line is his alone.
            vec![]
        );
        assert_eq!(
            pair_players(&ours, &["LJ Katz"], ALL, |_, _| true),
            vec![(0, 0, PairedBy::Alias)]
        );
    }

    #[test]
    fn an_ambiguous_variant_pairs_nobody() {
        // Two players either of whom "Chris Parker" may be.
        let ours = names(&[&["Christopher Parker"], &["Christian Parker"]]);
        let pass = Passes {
            last_name: false,
            variants: true,
        };
        assert!(pair_players(&ours, &["Chris Parker"], pass, |_, _| true).is_empty());
        // One line either of two of ours may be, one of them already paired
        // by name: still ambiguous, so the open one does not take it.
        let ours = names(&[&["Cam Hiatt"], &["Cameron Hiatt"]]);
        let theirs = ["Cam Hiatt", "Cameron Hiatts"];
        assert_eq!(
            pair_players(&ours, &theirs, pass, |_, _| true),
            vec![(0, 0, PairedBy::Name)]
        );
        // An alias that two of ours share pairs neither.
        let ours = names(&[&["Christopher Jones", "Chris Jones"], &["Chris Jones"]]);
        assert!(pair_players(&ours, &["Chris Jones"], pass, |_, _| true).is_empty());
    }
}
