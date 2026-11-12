//! Parsing show, season, episode, and title out of file paths.
//!
//! Recognised forms (case-insensitive), checked in this order:
//!
//! 1. `S01E02` / `s1e2` / `S01.E02` anywhere in the file name;
//! 2. `1x02` anywhere in the file name (one- or two-digit season, two- or
//!    three-digit episode, so `1920x1080` is not mistaken for an episode),
//!    including multi-episode files like `6x01x02`;
//! 3. a parent folder named `Season N`, with the episode taken from `E3`,
//!    `Ep 3`, `Episode 3`, or a leading number in the file name.
//!
//! Anything else is unparsed: no show, season, or episode, and the title is
//! the file name. Unparsed items fall back to natural sort by path, which is
//! the rules engine's job (see [`marathon_rules::ItemKey::natural_order`]).

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// What was parsed from one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub show: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    /// The episode title if one follows the episode code, otherwise the file
    /// name without its extension.
    pub title: String,
}

/// Compiles a regex literal.
fn re(pattern: &str) -> Regex {
    // The patterns are literals in this file, covered by the tests below, so
    // a failure here is a programming error caught before release.
    #[allow(clippy::expect_used)]
    Regex::new(pattern).expect("regex literal is valid")
}

/// `S01E02`, `s1e2`, `S01.E02`, `S01 E02`. Group 1 is the whole code.
static SXXEYY: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:^|[^a-z0-9])(s(\d{1,3})[ ._-]?e(\d{1,4}))(?:[^0-9]|$)"));

/// `1x02`, also as the start of a multi-episode `1x02x03`. Group 1 is the
/// whole code (without any extra episodes).
static NXNN: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:^|[^a-z0-9])((\d{1,2})x(\d{2,3}))(?:[^0-9a-z]|x\d|$)"));

/// Extra episodes of a multi-episode `S01E02` file, right after the first
/// code: the `E03` or `-E03` in `S01E02E03` or `S01E02-E03`.
static EXTRA_EPISODES: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:[-_ ]?e\d{1,4})+"));

/// Extra episodes of a multi-episode `1x02` file, right after the first
/// code: the `x03` or `-x03` in `1x02x03` or `1x02-x03`. Kept apart from
/// [`EXTRA_EPISODES`] so a codec tag (`S01E02 x264`) is never taken for one.
static EXTRA_NXNN_EPISODES: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:[-_ ]?x\d{2,3})+"));

/// A `Season N` folder name.
static SEASON_FOLDER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^season[ ._-]*(\d{1,3})$"));

/// `E3`, `Ep 3`, `Ep. 3`, `Episode 3`, used inside a `Season N` folder.
/// Group 1 is the whole marker.
static EPISODE_MARKER: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:^|[^a-z0-9])((?:episode|ep|e)[ ._-]*(\d{1,4}))(?:[^0-9]|$)"));

/// A leading episode number, used inside a `Season N` folder: `03 - Title`.
/// Group 1 is the number.
static LEADING_NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{1,4})(?:[^0-9]|$)"));

/// Returns `true` if `name` is a `Season N` folder name.
pub fn is_season_folder(name: &str) -> bool {
    SEASON_FOLDER.is_match(name)
}

/// Parses a `/`-separated path relative to a source root.
pub fn parse_path(path: &str) -> ParsedName {
    let mut parts = path.rsplit('/');
    // `rsplit` always yields at least one element.
    let file = parts.next().unwrap_or(path);
    let parent = parts.next();
    let grandparent = parts.next();

    let stem = strip_extension(file);
    let folder_season = parent.and_then(|p| captured_number(&SEASON_FOLDER.captures(p)?, 1));
    // In a `Season N` folder, the show is the folder above it.
    let folder_show = folder_season
        .and(grandparent)
        .map(clean)
        .filter(|s| !s.is_empty());

    let code = find_code(&SXXEYY, stem)
        .map(|found| (found, &*EXTRA_EPISODES))
        .or_else(|| find_code(&NXNN, stem).map(|found| (found, &*EXTRA_NXNN_EPISODES)));
    if let Some(((season, episode, before, after), extra)) = code {
        let after = extra.replace(after, "");
        let show = folder_show
            .or_else(|| non_empty(clean(before)))
            .or_else(|| parent.map(clean).filter(|s| !s.is_empty()));
        return ParsedName {
            show,
            season: Some(season),
            episode: Some(episode),
            title: non_empty(clean(&after)).unwrap_or_else(|| clean_or_raw(stem)),
        };
    }

    if let Some(season) = folder_season {
        let (episode, title) = match EPISODE_MARKER
            .captures(stem)
            .or_else(|| LEADING_NUMBER.captures(stem))
        {
            Some(caps) => {
                // Group 1 is the marker or number, group 2 (if present) the
                // digits of a marker.
                let episode = captured_number(&caps, 2).or_else(|| captured_number(&caps, 1));
                let end = caps.get(1).map_or(0, |m| m.end());
                (episode, non_empty(clean(&stem[end..])))
            }
            None => (None, None),
        };
        return ParsedName {
            show: folder_show,
            season: Some(season),
            episode,
            title: title.unwrap_or_else(|| clean_or_raw(stem)),
        };
    }

    ParsedName {
        show: None,
        season: None,
        episode: None,
        title: clean_or_raw(stem),
    }
}

/// Finds an episode code with `re` (group 1: whole code, 2: season,
/// 3: episode) and returns the numbers and the text before and after it.
fn find_code<'a>(re: &Regex, stem: &'a str) -> Option<(u32, u32, &'a str, &'a str)> {
    let caps = re.captures(stem)?;
    let code = caps.get(1)?;
    Some((
        captured_number(&caps, 2)?,
        captured_number(&caps, 3)?,
        &stem[..code.start()],
        &stem[code.end()..],
    ))
}

fn captured_number(caps: &Captures<'_>, group: usize) -> Option<u32> {
    caps.get(group)?.as_str().parse().ok()
}

/// Removes a file extension: the part after the last `.`, if it is short and
/// alphanumeric (so `Mr. Robot` keeps its dot).
fn strip_extension(file: &str) -> &str {
    match file.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem
        }
        _ => file,
    }
}

/// Tidies a fragment of a file name into a display string: dot- or
/// underscore-separated names (`The.Office.US`) get spaces, and separator
/// characters are trimmed from both ends.
fn clean(s: &str) -> String {
    let spaced = if s.contains(' ') {
        s.to_owned()
    } else {
        s.replace(['.', '_'], " ")
    };
    spaced
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—' | '.' | '_'))
        .to_owned()
}

fn clean_or_raw(stem: &str) -> String {
    non_empty(clean(stem)).unwrap_or_else(|| stem.to_owned())
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use marathon_rules::ItemKey;

    use super::*;

    fn parsed(show: Option<&str>, season: u32, episode: u32, title: &str) -> ParsedName {
        ParsedName {
            show: show.map(str::to_owned),
            season: Some(season),
            episode: Some(episode),
            title: title.to_owned(),
        }
    }

    #[test]
    fn sxxeyy_with_show_and_title() {
        assert_eq!(
            parse_path("TV/Test Show/Season 1/Test Show - S01E02 - The Second One.mkv"),
            parsed(Some("Test Show"), 1, 2, "The Second One")
        );
    }

    #[test]
    fn lowercase_unpadded_sxey() {
        assert_eq!(
            parse_path("shows/office/office s1e2 diversity day.mp4"),
            parsed(Some("office"), 1, 2, "diversity day")
        );
    }

    #[test]
    fn dotted_scene_name() {
        assert_eq!(
            parse_path("The.Office.US.S03E14.The.Injury.720p.mkv"),
            parsed(Some("The Office US"), 3, 14, "The Injury 720p")
        );
    }

    #[test]
    fn nxnn() {
        assert_eq!(
            parse_path("Taskmaster/Taskmaster 1x02 - The Pie Whisperer.mkv"),
            parsed(Some("Taskmaster"), 1, 2, "The Pie Whisperer")
        );
    }

    #[test]
    fn resolution_is_not_an_episode() {
        let p = parse_path("Essays/Camera Systems 1920x1080.mkv");
        assert_eq!(p.season, None);
        assert_eq!(p.title, "Camera Systems 1920x1080");
    }

    #[test]
    fn multi_episode_file_takes_first_episode() {
        assert_eq!(
            parse_path("Show/Show - S02E05E06 - Double.mkv"),
            parsed(Some("Show"), 2, 5, "Double")
        );
        assert_eq!(
            parse_path("Show/Show - S02E05-E06 - Double.mkv"),
            parsed(Some("Show"), 2, 5, "Double")
        );
        assert_eq!(
            parse_path("TV/Parks and Recreation/Parks and Recreation - 6x01x02 - London.mkv"),
            parsed(Some("Parks and Recreation"), 6, 1, "London")
        );
        assert_eq!(
            parse_path("Show/Show - 8x12-x13 - Double.mkv"),
            parsed(Some("Show"), 8, 12, "Double")
        );
    }

    #[test]
    fn codec_tag_after_code_stays_in_title() {
        assert_eq!(
            parse_path("Show/Show - S01E02 x264.mkv"),
            parsed(Some("Show"), 1, 2, "x264")
        );
    }

    #[test]
    fn code_without_prefix_takes_show_from_folder() {
        assert_eq!(
            parse_path("TV/Firefly/S01E03 Bushwhacked.mkv"),
            parsed(Some("Firefly"), 1, 3, "Bushwhacked")
        );
    }

    #[test]
    fn code_only_uses_file_name_as_title() {
        assert_eq!(
            parse_path("TV/Firefly/S01E03.mkv"),
            parsed(Some("Firefly"), 1, 3, "S01E03")
        );
    }

    #[test]
    fn season_folder_beats_file_prefix_for_show() {
        // The folder name is more consistent across files than prefixes.
        assert_eq!(
            parse_path("TV/The Office (US)/Season 3/The.Office.S03E14.mkv").show,
            Some("The Office (US)".to_owned())
        );
    }

    #[test]
    fn season_folder_with_episode_marker() {
        assert_eq!(
            parse_path("TV/Taskmaster/Season 4/Episode 3 - Friendship Is Truly Blind.mkv"),
            parsed(Some("Taskmaster"), 4, 3, "Friendship Is Truly Blind")
        );
        assert_eq!(
            parse_path("TV/Taskmaster/Season 4/Ep. 3.mkv"),
            parsed(Some("Taskmaster"), 4, 3, "Ep. 3")
        );
    }

    #[test]
    fn season_folder_with_leading_number() {
        assert_eq!(
            parse_path("TV/Taskmaster/season_10/07 - Mr. Bucket.mkv"),
            parsed(Some("Taskmaster"), 10, 7, "Mr. Bucket")
        );
    }

    #[test]
    fn season_folder_without_episode_number() {
        let p = parse_path("TV/Taskmaster/Season 2/Champion of Champions.mkv");
        assert_eq!(p.show.as_deref(), Some("Taskmaster"));
        assert_eq!(p.season, Some(2));
        assert_eq!(p.episode, None);
        assert_eq!(p.title, "Champion of Champions");
    }

    #[test]
    fn unparsed_names_keep_their_file_name() {
        let p = parse_path("Essays/Ep 2 - Pacing.mkv");
        assert_eq!(
            p,
            ParsedName {
                show: None,
                season: None,
                episode: None,
                title: "Ep 2 - Pacing".to_owned(),
            }
        );
        assert_eq!(
            parse_path("Misc/Café Society's Ünïcode Clip — Ørsted.mkv").title,
            "Café Society's Ünïcode Clip — Ørsted"
        );
        assert_eq!(
            parse_path("Essays/why_doors_matter.mp4").title,
            "why doors matter"
        );
        assert_eq!(parse_path("Mr. Robot").title, "Mr. Robot");
    }

    #[test]
    fn season_folder_detection() {
        assert!(is_season_folder("Season 1"));
        assert!(is_season_folder("season_12"));
        assert!(is_season_folder("SEASON.3"));
        assert!(!is_season_folder("Seasonal Specials"));
        assert!(!is_season_folder("Season"));
    }

    /// Unparsed items fall back to natural sort by path: `Ep 2` sorts before
    /// `Ep 10`, using the parsed metadata exactly as the server builds it.
    #[test]
    fn unparsed_items_fall_back_to_natural_sort() {
        let key = |id: &str, path: &str| {
            let p = parse_path(path);
            ItemKey {
                id: id.into(),
                path: path.into(),
                show: p.show,
                season: p.season,
                episode: p.episode,
                position: None,
            }
        };
        let mut items = [
            key("ten", "Essays/Ep 10 - Difficulty Curves.mkv"),
            key("two", "Essays/Ep 2 - Pacing.mkv"),
            key("one", "Essays/Ep 1 - Intro.mkv"),
        ];
        items.sort_by(ItemKey::natural_order);
        let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["one", "two", "ten"]);
    }

    /// Parsed episodes sort by season and episode, not by file name.
    #[test]
    fn parsed_items_sort_by_season_and_episode() {
        let key = |path: &str| {
            let p = parse_path(path);
            ItemKey {
                id: path.into(),
                path: path.into(),
                show: p.show,
                season: p.season,
                episode: p.episode,
                position: None,
            }
        };
        let mut items = [
            key("Show/Season 10/Show - S10E01 - X.mkv"),
            key("Show/Season 2/Show - S02E10 - X.mkv"),
            key("Show/Season 2/Show - S02E09 - X.mkv"),
        ];
        items.sort_by(ItemKey::natural_order);
        let eps: Vec<_> = items.iter().map(|i| (i.season, i.episode)).collect();
        assert_eq!(
            eps,
            [(Some(2), Some(9)), (Some(2), Some(10)), (Some(10), Some(1))]
        );
    }
}
