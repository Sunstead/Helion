//! What the icon source offers: its `metadata.json`, trimmed to the icons that
//! are SVGs and the variants it names for them.

use serde::{ Deserialize, Serialize };
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    pub icons: BTreeMap<String, Entry>,
    /// Names with `-`, `_` and `.` taken out, for those that stay unique, so
    /// `opencloud` finds `open-cloud`. Built by [`Index::finish`].
    #[serde(skip)]
    squashed: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    /// Other files for the same icon (the source's `colors`), for the other
    /// kind of background. Which is which is decided from the ink, not from
    /// their names: the source isn't consistent about it.
    pub variants: Vec<String>,
}

#[derive(Deserialize)]
struct Meta {
    base: String,
    #[serde(default)]
    colors: Option<Colors>,
}

#[derive(Deserialize)]
struct Colors {
    dark: Option<String>,
    light: Option<String>,
}

fn squash(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, '-' | '_' | '.')).collect()
}

/// An icon name is a file name on disk and in a URL, so it is lowercase
/// letters, digits, `-`, `_` and `.`, and never starts with a dot.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() &&
        name.len() <= 64 &&
        !name.starts_with('.') &&
        name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
}

impl Index {
    /// Parses the source's `metadata.json`. Aliases are left out: they are
    /// free text ("Web Server", "In-Memory Database") and would match
    /// containers that merely share a word.
    pub fn parse(json: &[u8]) -> Result<Self, serde_json::Error> {
        let all: BTreeMap<String, Meta> = serde_json::from_slice(json)?;
        let icons = all
            .into_iter()
            .filter(|(name, meta)| meta.base == "svg" && valid_name(name))
            .map(|(name, meta)| {
                let mut variants: Vec<String> = meta.colors
                    .into_iter()
                    .flat_map(|c| [c.dark, c.light])
                    .flatten()
                    .filter(|v| valid_name(v) && *v != name)
                    .collect();
                variants.sort();
                variants.dedup();
                (name, Entry { variants })
            })
            .collect();
        Ok(Self { icons, squashed: BTreeMap::new() }.finish())
    }

    /// Builds what isn't stored: call after parsing or loading from disk.
    pub fn finish(mut self) -> Self {
        let mut seen: BTreeMap<String, Option<String>> = BTreeMap::new();
        for name in self.icons.keys() {
            seen.entry(squash(name))
                .and_modify(|first| *first = None)
                .or_insert_with(|| Some(name.clone()));
        }
        self.squashed = seen
            .into_iter()
            .filter_map(|(squashed, name)| name.map(|n| (squashed, n)))
            .collect();
        self
    }

    /// The icon called `name`, or the one that differs from it only in
    /// separators. Never a guess: two icons alike that way match neither.
    pub fn lookup(&self, name: &str) -> Option<&str> {
        if let Some((exact, _)) = self.icons.get_key_value(name) {
            return Some(exact);
        }
        self.squashed.get(&squash(name)).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.icons.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        r#"{
        "gitea": {"base": "svg", "aliases": ["Git hosting"]},
        "github": {"base": "svg", "colors": {"dark": "github", "light": "github-light"}},
        "plex": {"base": "png"},
        "../evil": {"base": "svg"},
        "Upper": {"base": "svg"}
    }"#;

    #[test]
    fn keeps_svg_icons_and_the_variants_they_name() {
        let index = Index::parse(SAMPLE.as_bytes()).unwrap();
        assert!(index.icons.contains_key("gitea"));
        assert!(!index.icons.contains_key("plex"), "a png-only icon can't be served as an svg");
        assert_eq!(index.icons["github"].variants, ["github-light"]);
    }

    #[test]
    fn drops_names_that_are_not_safe_file_names() {
        let index = Index::parse(SAMPLE.as_bytes()).unwrap();
        assert!(!index.icons.contains_key("../evil") && !index.icons.contains_key("Upper"));
        assert!(valid_name("uptime-kuma") && valid_name("a.b_c"));
        assert!(!valid_name("") && !valid_name(".hidden") && !valid_name("a/b") && !valid_name("A"));
    }

    #[test]
    fn finds_a_name_that_differs_only_in_separators() {
        let index = Index::parse(
            br#"{"open-cloud": {"base": "svg"}, "a-b": {"base": "svg"}, "ab": {"base": "svg"}, "gitea": {"base": "svg"}}"#
        ).unwrap();
        assert_eq!(index.lookup("opencloud"), Some("open-cloud"));
        assert_eq!(index.lookup("open_cloud"), Some("open-cloud"));
        assert_eq!(index.lookup("gitea"), Some("gitea"));
        assert_eq!(index.lookup("a-b"), Some("a-b"), "exact wins");
        assert_eq!(index.lookup("a.b"), None, "two icons alike that way: neither");
        assert_eq!(index.lookup("open"), None);
    }

    #[test]
    fn aliases_do_not_become_names() {
        let index = Index::parse(SAMPLE.as_bytes()).unwrap();
        assert!(!index.icons.contains_key("git-hosting"));
    }
}
