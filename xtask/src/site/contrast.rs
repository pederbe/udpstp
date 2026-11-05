//! `cargo xtask site contrast`: the palette's token pairs meet WCAG 2.2 AA
//! contrast (4.5:1 for text, 3:1 for large text and UI), in every palette
//! the site uses. The tokens are read from the stylesheets themselves, from
//! the block after each `/* tokens: <name> */` comment, and the pairs from
//! `website/contrast.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::read;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    palette: Vec<Palette>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Palette {
    name: String,
    /// The stylesheet, relative to the repository root.
    file: String,
    /// Palettes whose tokens this one inherits and overrides.
    #[serde(default)]
    inherits: Vec<String>,
    pairs: Vec<Pair>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    fg: String,
    bg: String,
    /// 4.5 for text, 3 for large text and UI.
    min: f64,
    /// What the pair is used for.
    #[serde(rename = "use")]
    usage: String,
}

pub fn run(root: &Path) -> Result<(), String> {
    let config: Config = toml::from_str(&read(&root.join("website/contrast.toml"))?)
        .map_err(|e| format!("website/contrast.toml: {e}"))?;
    let mut tokens_by_palette: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut checked = 0;
    for palette in &config.palette {
        let css = read(&root.join(&palette.file))?;
        let mut tokens = BTreeMap::new();
        for parent in &palette.inherits {
            tokens.extend(
                tokens_by_palette
                    .get(parent)
                    .ok_or_else(|| format!("{}: inherits unknown palette {parent}", palette.name))?
                    .clone(),
            );
        }
        tokens.extend(block_tokens(&css, &palette.name).ok_or_else(|| {
            format!(
                "{}: no `/* tokens: {} */` block",
                palette.file, palette.name
            )
        })?);
        for pair in &palette.pairs {
            let colour = |name: &str| {
                tokens
                    .get(name)
                    .and_then(|v| hex(v))
                    .ok_or_else(|| format!("{}: no hex colour --{name}", palette.name))
            };
            let ratio = ratio(colour(&pair.fg)?, colour(&pair.bg)?);
            checked += 1;
            if ratio + 1e-9 < pair.min {
                failures.push(format!(
                    "{}: --{} on --{} is {ratio:.2}:1, below {}:1 ({})",
                    palette.name, pair.fg, pair.bg, pair.min, pair.usage
                ));
            }
        }
        tokens_by_palette.insert(palette.name.clone(), tokens);
    }
    if failures.is_empty() {
        println!("contrast: {checked} token pairs meet their minimum");
        Ok(())
    } else {
        Err(format!(
            "contrast check failed:\n  {}",
            failures.join("\n  ")
        ))
    }
}

/// The `--name: value` declarations of the block after `/* tokens: name */`.
fn block_tokens(css: &str, name: &str) -> Option<BTreeMap<String, String>> {
    let marker = format!("/* tokens: {name} */");
    let start = css.find(&marker)? + marker.len();
    let open = start + css[start..].find('{')?;
    let close = open + css[open..].find('}')?;
    Some(
        css[open + 1..close]
            .split(';')
            .filter_map(|decl| {
                let (key, value) = decl.split_once(':')?;
                let key = key.trim().strip_prefix("--")?;
                Some((key.to_string(), value.trim().to_string()))
            })
            .collect(),
    )
}

fn hex(value: &str) -> Option<[f64; 3]> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let channel = |i: usize| {
        u8::from_str_radix(&digits[i..i + 2], 16)
            .ok()
            .map(f64::from)
    };
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// WCAG 2.2 relative luminance.
fn luminance([r, g, b]: [f64; 3]) -> f64 {
    let linear = |c: f64| {
        let c = c / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn ratio(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_ratios_match_wcag() {
        let black = hex("#000000").unwrap();
        let white = hex("#ffffff").unwrap();
        assert!((ratio(black, white) - 21.0).abs() < 1e-9);
        assert!((ratio(hex("#777777").unwrap(), white) - 4.48).abs() < 0.01);
    }

    #[test]
    fn reads_a_token_block() {
        let css = "a{}\n/* tokens: dark */\n:root {\n  --bg: #121214;\n  --sans: ui-sans-serif, system-ui;\n}\n";
        let tokens = block_tokens(css, "dark").unwrap();
        assert_eq!(tokens["bg"], "#121214");
        assert!(block_tokens(css, "light").is_none());
    }
}
