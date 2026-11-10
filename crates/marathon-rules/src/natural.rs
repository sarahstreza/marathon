//! Natural ("human") string ordering, so that `Ep 2` sorts before `Ep 10`.

use std::cmp::Ordering;

/// One run of a string: either consecutive ASCII digits or everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chunk<'a> {
    Digits(&'a str),
    Text(&'a str),
}

/// Splits `s` into alternating runs of ASCII digits and non-digits.
fn chunks(s: &str) -> impl Iterator<Item = Chunk<'_>> {
    let mut rest = s;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let is_digit = first.is_ascii_digit();
        let end = rest
            .find(|c: char| c.is_ascii_digit() != is_digit)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(if is_digit {
            Chunk::Digits(run)
        } else {
            Chunk::Text(run)
        })
    })
}

/// Compares two digit runs by numeric value without parsing (so arbitrarily
/// long runs cannot overflow).
fn cmp_digits(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Compares two text runs case-insensitively.
fn cmp_text(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// Natural ordering of two strings.
///
/// Runs of ASCII digits compare by numeric value and other text compares
/// case-insensitively; a digit run sorts before a text run at the same
/// position. Strings that are equal under those rules (such as `Ep 02` and
/// `Ep 2`, or `a` and `A`) fall back to plain byte order, so the result is a
/// total order consistent with `==`.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = chunks(a);
    let mut right = chunks(b);
    loop {
        let ord = match (left.next(), right.next()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(Chunk::Digits(x)), Some(Chunk::Digits(y))) => cmp_digits(x, y),
            (Some(Chunk::Text(x)), Some(Chunk::Text(y))) => cmp_text(x, y),
            (Some(Chunk::Digits(_)), Some(Chunk::Text(_))) => Ordering::Less,
            (Some(Chunk::Text(_)), Some(Chunk::Digits(_))) => Ordering::Greater,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(
            sorted(vec!["Ep 10", "Ep 2", "Ep 1", "Ep 100"]),
            vec!["Ep 1", "Ep 2", "Ep 10", "Ep 100"]
        );
    }

    #[test]
    fn text_is_case_insensitive() {
        assert_eq!(
            sorted(vec!["banana", "Apple", "cherry"]),
            vec!["Apple", "banana", "cherry"]
        );
    }

    #[test]
    fn mixed_paths() {
        assert_eq!(
            sorted(vec![
                "Show/Season 10/a.mkv",
                "Show/Season 2/b.mkv",
                "Show/Season 2/a.mkv",
                "Show/Season 1/z.mkv",
            ]),
            vec![
                "Show/Season 1/z.mkv",
                "Show/Season 2/a.mkv",
                "Show/Season 2/b.mkv",
                "Show/Season 10/a.mkv",
            ]
        );
    }

    #[test]
    fn ties_break_by_bytes_for_a_total_order() {
        assert_eq!(natural_cmp("Ep 02", "Ep 2"), "Ep 02".cmp("Ep 2"));
        assert_ne!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
    }

    #[test]
    fn huge_numbers_do_not_overflow() {
        let big = "x99999999999999999999999999999999999999";
        let bigger = "x100000000000000000000000000000000000000";
        assert_eq!(natural_cmp(big, bigger), Ordering::Less);
    }

    #[test]
    fn digits_sort_before_text() {
        assert_eq!(natural_cmp("1abc", "abc"), Ordering::Less);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
    }

    #[test]
    fn unicode_does_not_panic() {
        assert_eq!(
            sorted(vec!["Ørsted 2", "Café 10", "Café 9"]),
            vec!["Café 9", "Café 10", "Ørsted 2"]
        );
    }
}
