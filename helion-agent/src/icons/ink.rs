//! Whether an icon's ink suits a dark or a light background, read from its
//! colours. The source's `-light` and `-dark` files aren't named consistently
//! (`github-light` is white, `portainer-dark` is near-white), so names say
//! nothing about which to show.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// Dark marks: needs a light background.
    Dark,
    /// Light marks: needs a dark background.
    Light,
    /// Colours (or both): reads on either.
    Either,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Dark,
    Light,
}

fn luminance(r: u8, g: u8, b: u8) -> f32 {
    (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
}

fn hex(digits: &str) -> Option<(u8, u8, u8)> {
    let v = |s: &str| u8::from_str_radix(s, 16).ok();
    match digits.len() {
        3 => {
            let d = |i: usize| v(&digits[i..=i]).map(|n| n * 17);
            Some((d(0)?, d(1)?, d(2)?))
        }
        6 => Some((v(&digits[0..2])?, v(&digits[2..4])?, v(&digits[4..6])?)),
        _ => None,
    }
}

/// Every colour the file names, as luminances. Unnamed ink is black, which is
/// what an SVG without a fill draws.
fn lights(svg: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let bytes = svg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let end = svg[i + 1..]
                .find(|c: char| !c.is_ascii_hexdigit())
                .map_or(svg.len(), |n| i + 1 + n);
            if let Some((r, g, b)) = hex(&svg[i + 1..end]) {
                out.push(luminance(r, g, b));
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    for (word, l) in [("\"black\"", 0.0), ("\"white\"", 1.0), (":black", 0.0), (":white", 1.0)] {
        out.extend(std::iter::repeat_n(l, svg.matches(word).count()));
    }
    out
}

pub fn classify(svg: &str) -> Ink {
    let colours = lights(svg);
    if colours.is_empty() {
        return Ink::Dark;
    }
    let dark = colours.iter().filter(|l| **l < 0.3).count();
    let light = colours.iter().filter(|l| **l > 0.7).count();
    let middle = colours.len() - dark - light;
    match (dark, light, middle) {
        (_, _, m) if m > 0 => Ink::Either,
        (d, l, _) if d > 0 && l > 0 => Ink::Either,
        (_, 0, _) => Ink::Dark,
        _ => Ink::Light,
    }
}

/// The first of `files` (the icon itself, then its variants) whose ink reads
/// on `background`; the first when none does.
pub fn pick(files: &[(String, Ink)], background: Background) -> Option<&str> {
    let suits = |ink: Ink| {
        matches!(
            (ink, background),
            (Ink::Either, _) | (Ink::Light, Background::Dark) | (Ink::Dark, Background::Light)
        )
    };
    files
        .iter()
        .find(|(_, ink)| suits(*ink))
        .or_else(|| files.first())
        .map(|(name, _)| name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_ink() {
        assert_eq!(classify(r##"<path style="fill:#1b1f23" d=""/>"##), Ink::Dark);
        assert_eq!(classify(r##"<path fill="#fff" d=""/>"##), Ink::Light);
        assert_eq!(classify(r##"<path fill="#FF80F2"/><path fill="black"/>"##), Ink::Either);
        assert_eq!(classify(r##"<path d=""/>"##), Ink::Dark, "no fill is black");
        assert_eq!(classify(r##"<path fill="#fff"/><path fill="#000"/>"##), Ink::Either);
    }

    #[test]
    fn picks_the_file_that_reads_on_the_background() {
        let files = vec![("github".to_string(), Ink::Dark), ("github-light".to_string(), Ink::Light)];
        assert_eq!(pick(&files, Background::Light), Some("github"));
        assert_eq!(pick(&files, Background::Dark), Some("github-light"));
        let colour = vec![("plex".to_string(), Ink::Either)];
        assert_eq!(pick(&colour, Background::Dark), Some("plex"));
        let only_dark = vec![("x".to_string(), Ink::Dark)];
        assert_eq!(pick(&only_dark, Background::Dark), Some("x"), "better than nothing");
    }
}
