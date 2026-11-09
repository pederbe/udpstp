//! `cargo xtask site finish` and `cargo xtask site check`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Paths, SITE, copy, files_under, html_escape, read, relative, write};

/// The author, as every page's JSON-LD names him.
fn author() -> Value {
    json!({"@type": "Person", "@id": "https://pederbe.dev/#person",
        "name": "Peder Bergan", "url": "https://pederbe.dev",
        "sameAs": ["https://github.com/pederbe", "https://www.linkedin.com/in/pederbergan/",
            "https://bsky.app/profile/pederbe.dev"]})
}

pub fn run(paths: &Paths) -> Result<(), String> {
    let public = paths.public();
    if !public.join("getting-started.html").is_file() {
        return Err(format!(
            "{} isn't a built book; run `mdbook build target/site/book` first",
            public.display()
        ));
    }
    let extra = paths.extra();
    for file in files_under(&extra)? {
        copy(&file, &public.join(relative(&file, &extra)))?;
    }
    // The landing page's monospace face: mdBook's Source Code Pro, under a
    // name that doesn't change with mdBook's fingerprint.
    let fonts = public.join("fonts");
    let mono = files_under(&fonts)?
        .into_iter()
        .find(|f| {
            f.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with("source-code-pro-v11-all-charsets-500") && n.ends_with(".woff2")
            })
        })
        .ok_or("mdBook's output has no Source Code Pro 500")?;
    copy(&mono, &fonts.join("source-code-pro-500.woff2"))?;
    let list = read(&paths.pages_list())?;
    let mut pages = Vec::new();
    for line in list.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [book, title, description, part] = fields[..] else {
            return Err(format!("pages.tsv: bad line {line:?}"));
        };
        pages.push((book, title, description, part));
    }
    // Each part's crumb leads to its first page, as the chapter list does.
    let mut part_pages: BTreeMap<&str, String> = BTreeMap::new();
    for (book, _, _, part) in &pages {
        part_pages
            .entry(part)
            .or_insert_with(|| book.trim_end_matches(".md").to_string() + ".html");
    }
    for (book, title, description, part) in pages {
        let html_name = book.trim_end_matches(".md").to_string() + ".html";
        let path = public.join(&html_name);
        let page = read(&path)?;
        // Only real index pages are breadcrumb parents. The Reference and
        // Project sidebar groups have no landing page of their own.
        let crumb = if book.starts_with("decisions/") && book != "decisions/index.md" {
            Some(("Decisions", "decisions/index.html"))
        } else {
            part_pages
                .get(part)
                .filter(|first| matches!(part, "Guides" | "Conformance") && **first != html_name)
                .map(|first| (part, first.as_str()))
        };
        write(
            &path,
            head(&page, book, &html_name, title, description, crumb)?,
        )?;
    }
    // Accessibility fixes to mdBook's own markup, on every docs page.
    let mut applied: BTreeMap<&str, usize> = BTreeMap::new();
    let files = files_under(&public)?;
    let search_scripts: Vec<_> = files
        .iter()
        .filter(|file| {
            let name = relative(file, &public);
            name.starts_with("searcher-") && name.ends_with(".js")
        })
        .collect();
    if search_scripts.len() != 1 {
        return Err(format!(
            "expected exactly one mdBook searcher script, found {}; review search_teasers in finish.rs",
            search_scripts.len()
        ));
    }
    for file in files {
        let name = relative(&file, &public);
        let toc_script = name.starts_with("toc-") && name.ends_with(".js");
        let book_script = name.starts_with("book-") && name.ends_with(".js");
        if name.starts_with("searcher-") && name.ends_with(".js") {
            write(&file, search_teasers(&read(&file)?)?)?;
            continue;
        }
        if book_script {
            // mdBook keeps pages out of the back/forward cache with an unload
            // handler, which Chrome reports as a deprecated API; reload a page
            // restored from that cache instead, to the same effect.
            let script = read(&file)?;
            const UNLOAD: &str = "window.onunload = function() { };";
            if !script.contains(UNLOAD) {
                return Err(format!(
                    "{name}: mdBook's unload handler changed; review finish.rs"
                ));
            }
            const SHORTCUT_FOCUS: &str =
                "return /^(?:input|select|textarea)$/i.test(target.nodeName);";
            if !script.contains(SHORTCUT_FOCUS) {
                return Err(format!(
                    "{name}: mdBook's keyboard shortcut focus check changed; review finish.rs"
                ));
            }
            write(
                &file,
                script.replace(
                    UNLOAD,
                    "window.addEventListener('pageshow', function(event) { if (event.persisted) window.location.reload(); });",
                ).replace(
                    SHORTCUT_FOCUS,
                    "return /^(?:input|select|textarea)$/i.test(target.nodeName) || target.closest('button, [tabindex=\"0\"], #mdbook-sidebar') !== null;",
                ),
            )?;
            continue;
        }
        if !(name.ends_with(".html") || toc_script) || name == "index.html" {
            continue;
        }
        let page = read(&file)?;
        let depth = Path::new(&name)
            .parent()
            .map_or(0, |parent| parent.components().count());
        let (fixed, counts) = mdbook_accessibility(&page, &"../".repeat(depth));
        let chapter_list = toc_script || name == "toc.html";
        for ((from, _, expect), count) in ACCESSIBILITY_FIXES.iter().zip(&counts) {
            *applied.entry(*from).or_insert(0) += count;
            let required = match expect {
                Expect::Somewhere => false,
                Expect::EveryPage => !chapter_list,
                Expect::ChapterList => chapter_list,
            };
            if required && *count == 0 {
                return Err(format!(
                    "{name}: mdBook's markup changed, so the accessibility fix for {from} \
                     no longer applies; review ACCESSIBILITY_FIXES in finish.rs"
                ));
            }
        }
        let fixed = if name == "toc.html" {
            contents_landmark(&fixed)?
        } else {
            fixed
        };
        if fixed != page {
            write(&file, fixed)?;
        }
    }
    for (from, count) in &applied {
        if *count == 0 {
            return Err(format!(
                "the accessibility fix for {from} applied nowhere; review ACCESSIBILITY_FIXES in finish.rs"
            ));
        }
    }
    // mdBook's single-page print view and its chapter list duplicate the
    // pages; keep them out of search results.
    for name in ["print.html", "toc.html", "404.html"] {
        let path = public.join(name);
        if path.is_file() {
            let page = read(&path)?;
            let page = if name == "toc.html" {
                ensure_contents_title(&page)?
            } else {
                page
            };
            write(&path, ensure_noindex(&page)?)?;
        }
    }
    check(paths)?;
    println!("site finished: {}", public.display());
    Ok(())
}

/// Generic text cannot have an accessible name (WAI-ARIA 1.2, generic role).
/// Keep the teaser text and the result link's aria-details target intact.
fn search_teasers(script: &str) -> Result<String, String> {
    const TEMPLATE: &str = "            + '<span class=\"teaser\" id=\"mdbook-teaser_' + teaser_count\n            + '\" aria-label=\"Search Result Teaser\">' + teaser + '</span>';";
    if script.matches(TEMPLATE).count() != 1 {
        return Err(
            "mdBook's search teaser template changed or is duplicated; review search_teasers in finish.rs"
                .into(),
        );
    }
    Ok(script.replace(
        TEMPLATE,
        &TEMPLATE.replace(" aria-label=\"Search Result Teaser\"", ""),
    ))
}

/// The fixes `mdbook_accessibility` makes to mdBook 0.5.4's markup: (what
/// mdBook writes, what replaces it, where it must be found). Every fix must
/// apply where it is expected, so that an mdBook upgrade that changes the
/// markup fails the build instead of silently undoing the fix.
const ACCESSIBILITY_FIXES: &[(&str, &str, Expect)] = &[
    (
        "<body>",
        "<body>\n<a class=\"skip-link\" href=\"#main-content\">Skip to content</a>",
        Expect::EveryPage,
    ),
    (
        "<main>",
        "<main id=\"main-content\" tabindex=\"-1\">",
        Expect::EveryPage,
    ),
    // A static footer keeps the same author and project links with scripts
    // disabled, and sits outside main as the page's contentinfo landmark.
    (
        "</main>",
        "</main><footer class=\"udpstp-footer\">\
         <p>udpstp is free software under MIT OR Apache-2.0 · \
         <a href=\"https://github.com/pederbe/udpstp\">Source</a> · \
         <a href=\"__UDPSTP_ROOT__about.html\">About</a> · <a href=\"__UDPSTP_ROOT__index.html\">udpstp.com</a></p>\
         <p>Created by <a href=\"https://pederbe.dev\" rel=\"author me\">Peder Bergan</a> \
         <span class=\"udpstp-profiles\">\
         <a href=\"https://github.com/pederbe\" rel=\"me\">GitHub</a> · \
         <a href=\"https://www.linkedin.com/in/pederbergan/\" rel=\"me\">LinkedIn</a> · \
         <a href=\"https://bsky.app/profile/pederbe.dev\" rel=\"me\">Bluesky</a></span></p>\
         <p>Also by Peder: <a href=\"https://fastmash.io/\">Fastmash</a> · \
         <a href=\"https://ripscope.com/\">Ripscope</a> · \
         <a href=\"https://esdieff.com/\">Esdieff</a></p></footer>",
        Expect::EveryPage,
    ),
    // Code blocks and wide tables scroll, so they take keyboard focus.
    (
        "<pre><code class=",
        "<pre><code tabindex=\"0\" class=",
        Expect::Somewhere,
    ),
    (
        "<div class=\"table-wrapper\">",
        "<div class=\"table-wrapper\" tabindex=\"0\" role=\"region\" aria-label=\"Scrollable table\">",
        Expect::Somewhere,
    ),
    ("<th>", "<th scope=\"col\">", Expect::Somewhere),
    // The menu bar is a landmark.
    (
        "<div id=\"mdbook-menu-bar\" class=\"menu-bar sticky\">",
        "<div id=\"mdbook-menu-bar\" class=\"menu-bar sticky\" role=\"banner\">",
        Expect::EveryPage,
    ),
    // Native buttons bring keyboard activation without emulating it. The
    // footer script names each fold and keeps its expanded state current.
    (
        "<a class=\"chapter-fold-toggle\">",
        "<button type=\"button\" class=\"chapter-fold-toggle\" aria-label=\"Show or hide sections\">",
        Expect::ChapterList,
    ),
    (
        "<div>❱</div></a>",
        "<div aria-hidden=\"true\">❱</div></button>",
        Expect::ChapterList,
    ),
    // mdBook also creates nested folds in its on-this-page list at runtime.
    (
        "const toggle = document.createElement('a');",
        "const toggle = document.createElement('button');\n                    toggle.type = 'button';",
        Expect::Somewhere,
    ),
    // scrollIntoView on the active link also moves Chrome's sequential
    // focus starting point past the skip link. Scroll only the chapter list.
    (
        "activeSection.scrollIntoView({ block: 'center' });",
        "this.scrollTop += activeSection.getBoundingClientRect().top - this.getBoundingClientRect().top - this.clientHeight / 2 + activeSection.clientHeight / 2;",
        Expect::Somewhere,
    ),
    (
        "<label id=\"mdbook-sidebar-toggle\" class=\"icon-button\" for=\"mdbook-sidebar-toggle-anchor\"",
        "<button type=\"button\" id=\"mdbook-sidebar-toggle\" class=\"icon-button\"",
        Expect::EveryPage,
    ),
    (
        "</label>",
        "</button><noscript><a class=\"sidebar-contents-link\" href=\"__UDPSTP_ROOT__toc.html\">Contents</a></noscript>",
        Expect::EveryPage,
    ),
    (
        "<iframe class=\"sidebar-iframe-outer\"",
        "<iframe title=\"Table of contents\" class=\"sidebar-iframe-outer\"",
        Expect::EveryPage,
    ),
    // The masthead leads home and to the start of the docs, as on the
    // landing page. The mark's colours follow the reader's theme.
    (
        "<h1 class=\"menu-title\">udpstp</h1>",
        "<div class=\"menu-title\"><a class=\"docs-brand\" href=\"__UDPSTP_ROOT__index.html\">\
         <svg class=\"brand-mark\" viewBox=\"0 0 32 32\" width=\"22\" height=\"22\" aria-hidden=\"true\" focusable=\"false\">\
         <path class=\"brand-trace\" d=\"M5 27H10V21H15V15H20V11H27\"/><path class=\"brand-limit\" d=\"M5 5.5H27\"/></svg>udpstp</a>\
         <a class=\"docs-home\" href=\"__UDPSTP_ROOT__getting-started.html\">Docs</a></div>",
        Expect::EveryPage,
    ),
    (
        "placeholder=\"Search this book ...\"",
        "aria-label=\"Search this book\" placeholder=\"Search this book ...\"",
        Expect::EveryPage,
    ),
    (
        "aria-describedby=\"searchresults-header\"",
        "aria-describedby=\"mdbook-searchresults-header\"",
        Expect::EveryPage,
    ),
    (
        "<div id=\"mdbook-searchresults-header\" class=\"searchresults-header\">",
        "<div id=\"mdbook-searchresults-header\" class=\"searchresults-header\" role=\"status\" aria-live=\"polite\" aria-atomic=\"true\">",
        Expect::EveryPage,
    ),
];

/// Where an accessibility fix must apply.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// On at least one page of the site.
    Somewhere,
    /// On every docs page (every mdBook HTML page but its chapter list).
    EveryPage,
    /// In the chapter list: toc.html and the toc-*.js that writes it.
    ChapterList,
}

/// A file of mdBook's output with the accessibility fixes applied, and how
/// many times each applied.
fn mdbook_accessibility(page: &str, root: &str) -> (String, Vec<usize>) {
    let mut out = page.to_string();
    let mut counts = Vec::new();
    for (from, to, _) in ACCESSIBILITY_FIXES {
        counts.push(out.matches(from).count());
        out = out.replace(from, to);
    }
    (out.replace("__UDPSTP_ROOT__", root), counts)
}

/// Name the standalone and no-script iframe chapter list as navigation.
/// The script-generated sidebar already has its own navigation landmark.
fn contents_landmark(page: &str) -> Result<String, String> {
    const OPEN: &str = "<body class=\"sidebar-iframe-inner\">";
    const CLOSE: &str = "</body>";
    if page.matches(OPEN).count() != 1 || page.matches(CLOSE).count() != 1 {
        return Err(
            "toc.html: mdBook's contents body markup changed; review contents_landmark in finish.rs"
                .to_string(),
        );
    }
    Ok(page
        .replace(
            OPEN,
            &format!("{OPEN}<nav aria-label=\"Table of contents\">"),
        )
        .replace(CLOSE, &format!("</nav>{CLOSE}")))
}

/// A docs page's `<head>` with its own description, canonical URL, Open
/// Graph URL and text, Markdown alternate and JSON-LD.
fn head(
    page: &str,
    book: &str,
    html_name: &str,
    title: &str,
    description: &str,
    part: Option<(&str, &str)>,
) -> Result<String, String> {
    const META: &str = "<meta name=\"description\" content=\"";
    let start = page
        .find(META)
        .ok_or_else(|| format!("{html_name}: no meta description"))?;
    let end = start
        + META.len()
        + page[start + META.len()..]
            .find('"')
            .ok_or("unterminated meta description")?;
    let description = if description.is_empty() {
        title.to_string()
    } else {
        description.to_string()
    };
    let mut out = format!(
        "{}{META}{}{}",
        &page[..start],
        html_escape(&description),
        &page[end..]
    );
    let branded_title = format!("{title} - udpstp");
    let title_start = out.find("<title>").ok_or("no page title")? + "<title>".len();
    let title_end = title_start
        + out[title_start..]
            .find("</title>")
            .ok_or("unclosed page title")?;
    out.replace_range(title_start..title_end, &html_escape(&branded_title));
    let url = format!("{SITE}/{html_name}");
    let markdown_name = Path::new(book)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut crumbs = vec![("udpstp".to_string(), format!("{SITE}/"))];
    if let Some((name, first)) = part {
        crumbs.push((name.to_string(), format!("{SITE}/{first}")));
    }
    crumbs.push((title.to_string(), url.clone()));
    let items: Vec<Value> = crumbs
        .iter()
        .enumerate()
        .map(|(i, (name, item))| {
            json!({"@type": "ListItem", "position": i + 1, "name": name, "item": item})
        })
        .collect();
    let kind = page_kind(book);
    let mut document = json!({"@type": kind, "name": title, "description": description,
        "url": url, "inLanguage": "en",
        "isPartOf": {"@type": "WebSite", "name": "udpstp", "url": format!("{SITE}/")},
        "about": {"@type": "SoftwareApplication", "@id": format!("{SITE}/#app"), "name": "udpstp"},
        "author": author(), "creator": author(),
        "license": ["https://spdx.org/licenses/MIT.html", "https://spdx.org/licenses/Apache-2.0.html"]});
    if kind == "TechArticle" {
        document["headline"] = json!(title);
    }
    let json_ld = json!({"@context": "https://schema.org", "@graph": [document,
        {"@type": "BreadcrumbList", "itemListElement": items}]});
    let json_ld = json_ld.to_string().replace('<', "\\u003c");
    let og_type = if kind == "TechArticle" {
        "article"
    } else {
        "website"
    };
    let tags = format!(
        "<link rel=\"canonical\" href=\"{url}\">\n\
         <meta property=\"og:url\" content=\"{url}\">\n\
         <meta property=\"og:type\" content=\"{og_type}\">\n\
         <meta property=\"og:title\" content=\"{t}\">\n\
         <meta property=\"og:description\" content=\"{d}\">\n\
         <meta name=\"twitter:title\" content=\"{t}\">\n\
         <meta name=\"twitter:description\" content=\"{d}\">\n\
         <meta name=\"twitter:image\" content=\"{SITE}/og.png\">\n\
         <meta name=\"twitter:image:alt\" content=\"udpstp: measure the capacity your network really delivers. A Rust implementation of UDPSTP (RFC 9946).\">\n\
         <link rel=\"alternate\" type=\"text/markdown\" href=\"{markdown_name}\">\n\
         <script type=\"application/ld+json\">{json_ld}</script>\n",
        t = html_escape(&branded_title),
        d = html_escape(&description),
    );
    if out.contains("rel=\"canonical\"") {
        return Err(format!("{html_name}: already has a canonical URL"));
    }
    out = out.replacen("</head>", &format!("{tags}</head>"), 1);
    Ok(out)
}

/// The kind of content actually shown, rather than treating every page as
/// an article. These are Schema.org types, not protocol domain terms.
fn page_kind(book: &str) -> &'static str {
    match book {
        "about.md" => "AboutPage",
        "glossary.md"
        | "conformance/deviations.md"
        | "conformance/interpretations.md"
        | "conformance/obudpst-differences.md" => "CollectionPage",
        "citation.md" | "security-policy.md" | "contributing.md" => "WebPage",
        _ if book.ends_with("/index.md") => "CollectionPage",
        _ => "TechArticle",
    }
}

/// Checks the built site: internal links and their anchors, no third-party
/// resources, one title and language per page, and contrast.
pub fn check(paths: &Paths) -> Result<(), String> {
    let public = paths.public();
    let files = files_under(&public)?;
    let mut problems = Vec::new();
    let config = read(&paths.website.join("wrangler.jsonc"))?;
    let redirects = read(&public.join("_redirects"))?;
    if let Err(problem) = check_hosting_routes(&config, &redirects) {
        problems.push(problem);
    }
    let mut ids: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let html: Vec<&PathBuf> = files
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e == "html"))
        .collect();
    for file in &html {
        let page = read(file)?;
        ids.insert(
            (*file).clone(),
            attribute_values(&page, "id").into_iter().collect(),
        );
    }
    let mut checked = 0;
    for file in &html {
        let page = read(file)?;
        let name = relative(file, &public);
        if !page.contains("<html lang=\"en\"") {
            problems.push(format!("{name}: no lang=\"en\""));
        }
        if page.matches("<title>").count() != 1 {
            problems.push(format!("{name}: not exactly one <title>"));
        }
        for tag in [
            "<script src=\"http",
            "<link rel=\"stylesheet\" href=\"http",
            "<img src=\"http",
            "url(http",
        ] {
            if page.contains(tag) {
                problems.push(format!("{name}: loads a third-party resource ({tag}…)"));
            }
        }
        for link in attribute_values(&page, "href")
            .into_iter()
            .chain(attribute_values(&page, "src"))
            .chain(srcset_values(&page))
        {
            if link.contains("://")
                || link.starts_with("mailto:")
                || link.starts_with("data:")
                || link.starts_with("javascript:")
                || link.contains("{{")
            {
                continue;
            }
            checked += 1;
            let (path, fragment) = match link.split_once('#') {
                Some((path, fragment)) => (path, Some(fragment)),
                None => (link.as_str(), None),
            };
            let path = path.split('?').next().unwrap_or_default();
            let target = if path.is_empty() {
                (*file).clone()
            } else if let Some(absolute) = path.strip_prefix('/') {
                public.join(absolute)
            } else {
                file.parent().unwrap_or(&public).join(path)
            };
            let target = normalize(&target);
            let target = if target.is_dir() {
                target.join("index.html")
            } else {
                target
            };
            if !target.is_file() {
                problems.push(format!("{name}: broken link {link}"));
                continue;
            }
            if let Some(fragment) = fragment.filter(|f| !f.is_empty())
                && let Some(known) = ids.get(&target)
                && !known.contains(&percent_decode(fragment))
            {
                problems.push(format!("{name}: link {link} names a missing anchor"));
            }
        }
    }
    problems.extend(metadata_problems(paths)?);
    if let Err(contrast) = super::contrast::run(&paths.root) {
        problems.push(contrast);
    }
    if problems.is_empty() {
        println!(
            "site check: {} pages, {checked} internal links and their anchors, metadata and sitemap consistent, contrast met",
            html.len()
        );
        Ok(())
    } else {
        Err(format!("site check failed:\n  {}", problems.join("\n  ")))
    }
}

/// Checks the structure we generate, including all intended content pages.
/// This complements browser audits, which only sample representative pages.
fn metadata_problems(paths: &Paths) -> Result<Vec<String>, String> {
    let public = paths.public();
    let mut intended = BTreeMap::from([("index.html".to_string(), "SoftwareApplication")]);
    for line in read(&paths.pages_list())?.lines() {
        let book = line.split('\t').next().ok_or("empty pages.tsv line")?;
        intended.insert(
            book.trim_end_matches(".md").to_string() + ".html",
            page_kind(book),
        );
    }
    let canonical = |name: &str| {
        if name == "index.html" {
            format!("{SITE}/")
        } else {
            format!("{SITE}/{name}")
        }
    };
    let expected_urls: BTreeSet<String> = intended.keys().map(|name| canonical(name)).collect();
    let sitemap = read(&public.join("sitemap.xml"))?;
    let sitemap_urls: Vec<String> = sitemap
        .split("<loc>")
        .skip(1)
        .filter_map(|part| part.split_once("</loc>").map(|(url, _)| url.to_string()))
        .collect();
    let mut problems = Vec::new();
    if sitemap_urls.len() != expected_urls.len()
        || sitemap_urls.iter().cloned().collect::<BTreeSet<_>>() != expected_urls
    {
        problems.push(
            "sitemap.xml: URLs must match intended content exactly, without duplicates".into(),
        );
    }
    let robots = read(&public.join("robots.txt"))?;
    if !robots.contains("User-agent: *\nAllow: /")
        || !robots.contains(&format!("Sitemap: {SITE}/sitemap.xml"))
    {
        problems.push("robots.txt: content must be crawlable and the sitemap discoverable".into());
    }
    let image = std::fs::read(public.join("og.png")).map_err(|e| format!("og.png: {e}"))?;
    if image.get(..8) != Some(b"\x89PNG\r\n\x1a\n")
        || image.get(16..24)
            != Some(
                [1200u32.to_be_bytes(), 630u32.to_be_bytes()]
                    .concat()
                    .as_slice(),
            )
    {
        problems.push("og.png: expected a 1200 by 630 PNG social preview".into());
    }
    let mut titles = BTreeMap::new();
    let mut descriptions = BTreeMap::new();
    for (name, kind) in &intended {
        let page = read(&public.join(name))?;
        let checked = check_page_metadata(&page, &canonical(name), kind, &expected_urls);
        if let Err(problem) = checked {
            problems.push(format!("{name}: {problem}"));
            continue;
        }
        let title = page_title(&page)?;
        let description = one_tag_value(&page, "meta", "name", "description", "content")?;
        for (value, seen, label) in [
            (&title, &mut titles, "title"),
            (&description, &mut descriptions, "description"),
        ] {
            if let Some(other) = seen.insert(value.clone(), name) {
                problems.push(format!("{name}: duplicate {label} shared with {other}"));
            }
        }
    }
    for name in ["print.html", "toc.html", "404.html"] {
        let page = read(&public.join(name))?;
        if one_tag_value(&page, "meta", "name", "robots", "content").as_deref() != Ok("noindex") {
            problems.push(format!("{name}: duplicate or error page needs noindex"));
        }
    }
    Ok(problems)
}

fn check_page_metadata(
    page: &str,
    url: &str,
    kind: &str,
    content_urls: &BTreeSet<String>,
) -> Result<(), String> {
    let title = page_title(page)?;
    let meta = |selector: &str, name: &str| one_tag_value(page, "meta", selector, name, "content");
    let description = meta("name", "description")?;
    if title.chars().count() < 8 || description.chars().count() < 32 {
        return Err("title or description is too short to identify the content".into());
    }
    if one_tag_value(page, "link", "rel", "canonical", "href")? != url
        || meta("property", "og:url")? != url
    {
        return Err("canonical and Open Graph URLs disagree with the content URL".into());
    }
    for (key, expected) in [("title", &title), ("description", &description)] {
        if meta("property", &format!("og:{key}"))? != *expected
            || meta("name", &format!("twitter:{key}"))? != *expected
        {
            return Err(format!("HTML, Open Graph and Twitter {key} disagree"));
        }
    }
    let og_type = if kind == "TechArticle" {
        "article"
    } else {
        "website"
    };
    if meta("property", "og:type")? != og_type {
        return Err("Open Graph type does not describe the page's content".into());
    }
    let image = format!("{SITE}/og.png");
    let alt = meta("property", "og:image:alt")?;
    if meta("property", "og:image")? != image
        || meta("name", "twitter:image")? != image
        || meta("property", "og:image:width")? != "1200"
        || meta("property", "og:image:height")? != "630"
        || meta("name", "twitter:image:alt")? != alt
        || alt.chars().count() < 20
        || meta("name", "twitter:card")? != "summary_large_image"
    {
        return Err("social preview image, dimensions or text alternative disagree".into());
    }
    let main = main_content(page)?;
    if main.matches("<h1>").count() + main.matches("<h1 ").count() != 1 {
        return Err("content page needs exactly one main heading".into());
    }
    if !tag_values(page, "meta", "name", "robots", "content").is_empty() {
        return Err("intended content must remain indexable".into());
    }
    let blocks: Vec<_> = page
        .split("<script type=\"application/ld+json\">")
        .skip(1)
        .filter_map(|part| part.split_once("</script>").map(|(body, _)| body))
        .collect();
    if blocks.len() != 1 {
        return Err("expected exactly one JSON-LD graph".into());
    }
    let graph: Value =
        serde_json::from_str(blocks[0]).map_err(|e| format!("invalid JSON-LD: {e}"))?;
    if graph["@context"] != "https://schema.org" {
        return Err("JSON-LD must use Schema.org".into());
    }
    let nodes = graph["@graph"].as_array().ok_or("JSON-LD needs a graph")?;
    let document = nodes
        .iter()
        .find(|node| node["@type"] == kind)
        .ok_or("JSON-LD has the wrong content type")?;
    if document["url"] != url {
        return Err("JSON-LD content URL disagrees with canonical".into());
    }
    if kind == "SoftwareApplication" {
        check_software_graph(nodes)?;
    } else {
        if html_escape(document["name"].as_str().ok_or("document needs a name")?) + " - udpstp"
            != title
            || html_escape(
                document["description"]
                    .as_str()
                    .ok_or("document needs a description")?,
            ) != description
        {
            return Err("JSON-LD document identity disagrees with visible metadata".into());
        }
        if document["author"] != author() || document["creator"] != author() {
            return Err("JSON-LD author or creator disagrees with the maintainer credit".into());
        }
        if kind == "TechArticle" && document["headline"] != document["name"] {
            return Err("article headline disagrees with its name".into());
        }
        check_licence(document)?;
        let crumbs = nodes
            .iter()
            .find(|node| node["@type"] == "BreadcrumbList")
            .and_then(|node| node["itemListElement"].as_array())
            .ok_or("JSON-LD needs breadcrumbs")?;
        if crumbs.first().and_then(|item| item["item"].as_str()) != Some(&format!("{SITE}/"))
            || crumbs.last().and_then(|item| item["item"].as_str()) != Some(url)
        {
            return Err("breadcrumbs must lead from the site home to this page".into());
        }
        for (index, crumb) in crumbs.iter().enumerate() {
            if crumb["position"] != index + 1
                || !content_urls.contains(crumb["item"].as_str().unwrap_or(""))
            {
                return Err("breadcrumb order or destination is invalid".into());
            }
        }
    }
    Ok(())
}

fn check_hosting_routes(config: &str, redirects: &str) -> Result<(), String> {
    if !config
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .any(|line| line.trim() == "\"html_handling\": \"none\",")
    {
        return Err("wrangler.jsonc: exact .html content URLs require html_handling none".into());
    }
    let rules: Vec<_> = redirects
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if rules
        != [
            "/index.html / 301",
            "/ /index.html 200",
            "/changelog.html https://github.com/pederbe/udpstp/releases 301",
            "/changelog.md https://github.com/pederbe/udpstp/releases 301",
        ]
    {
        return Err(
            "_redirects: home routes or GitHub Releases redirects are missing or reordered".into(),
        );
    }
    Ok(())
}

fn check_software_graph(nodes: &[Value]) -> Result<(), String> {
    let app = nodes
        .iter()
        .find(|node| node["@type"] == "SoftwareApplication")
        .ok_or("missing software application")?;
    let source = nodes
        .iter()
        .find(|node| node["@type"] == "SoftwareSourceCode")
        .ok_or("missing source code")?;
    let person = nodes
        .iter()
        .find(|node| node["@type"] == "Person")
        .ok_or("missing author")?;
    if person != &author()
        || app["name"] != "udpstp"
        || source["name"] != "udpstp"
        || source["codeRepository"] != super::REPO
        || source["programmingLanguage"] != "Rust"
        || source["targetProduct"]["@id"] != app["@id"]
        || app["author"]["@id"] != person["@id"]
        || source["author"]["@id"] != person["@id"]
        || app["creator"]["@id"] != person["@id"]
        || source["creator"]["@id"] != person["@id"]
    {
        return Err(
            "software and source metadata disagree with the repository or maintainer".into(),
        );
    }
    for node in [app, source] {
        check_licence(node)?;
        if node.get("review").is_some() || node.get("aggregateRating").is_some() {
            return Err("software metadata must not invent reviews or ratings".into());
        }
    }
    if app["offers"]["price"] != "0"
        || app["operatingSystem"] != "Linux, macOS, Windows"
        || source["runtimePlatform"] != "Linux, macOS, Windows"
    {
        return Err(
            "software platform or price metadata disagrees with the visible content".into(),
        );
    }
    Ok(())
}

fn check_licence(node: &Value) -> Result<(), String> {
    if node["license"]
        != json!([
            "https://spdx.org/licenses/MIT.html",
            "https://spdx.org/licenses/Apache-2.0.html"
        ])
    {
        return Err("JSON-LD licence must match MIT OR Apache-2.0".into());
    }
    Ok(())
}

fn page_title(page: &str) -> Result<String, String> {
    page.split_once("<title>")
        .and_then(|(_, tail)| tail.split_once("</title>"))
        .map(|(title, _)| title.to_string())
        .ok_or_else(|| "missing page title".into())
}

/// mdBook also has a heading in the banner; only the content's heading
/// identifies this page. Accept attributes added by the accessibility fixes.
fn main_content(page: &str) -> Result<&str, String> {
    if page.matches("<main>").count() + page.matches("<main ").count() != 1 {
        return Err("content page needs exactly one main element".into());
    }
    page.split_once("<main")
        .and_then(|(_, rest)| rest.split_once('>'))
        .and_then(|(_, content)| content.split_once("</main>"))
        .map(|(content, _)| content)
        .ok_or_else(|| "content page has an unclosed main element".into())
}

/// mdBook's standalone chapter list also needs a title when visited directly.
fn ensure_contents_title(page: &str) -> Result<String, String> {
    match page.matches("<title>").count() {
        1 => Ok(page.to_string()),
        0 if page.contains("</head>") => {
            Ok(page.replacen("</head>", "<title>Contents - udpstp</title>\n</head>", 1))
        }
        0 => Err("cannot title the Contents page without its head".into()),
        _ => Err("duplicate Contents page titles".into()),
    }
}

/// mdBook already excludes its print and TOC pages. Keep that directive
/// once, and add it to the error page if mdBook did not supply it.
fn ensure_noindex(page: &str) -> Result<String, String> {
    match tag_values(page, "meta", "name", "robots", "content").as_slice() {
        [value] if value == "noindex" => Ok(page.to_string()),
        [] if page.contains("</head>") => Ok(page.replacen(
            "</head>",
            "<meta name=\"robots\" content=\"noindex\">\n</head>",
            1,
        )),
        [] => Err("cannot mark a page noindex without its head".into()),
        _ => Err("duplicate or conflicting robots metadata".into()),
    }
}

fn tag_values(page: &str, tag: &str, selector: &str, name: &str, attribute: &str) -> Vec<String> {
    page.split(&format!("<{tag} "))
        .skip(1)
        .filter_map(|tail| tail.split_once('>').map(|(head, _)| format!(" {head}")))
        .filter(|head| raw_attribute_values(head, selector) == [name])
        .flat_map(|head| raw_attribute_values(&head, attribute))
        .collect()
}

fn one_tag_value(
    page: &str,
    tag: &str,
    selector: &str,
    name: &str,
    attribute: &str,
) -> Result<String, String> {
    let values = tag_values(page, tag, selector, name, attribute);
    match values.as_slice() {
        [value] => Ok(value.clone()),
        _ => Err(format!(
            "expected exactly one {tag} {selector}=\"{name}\", found {}",
            values.len()
        )),
    }
}

/// Resolves `.` and `..` in a path without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// The values of every `name="…"` attribute in an HTML page, unescaped.
fn attribute_values(page: &str, name: &str) -> Vec<String> {
    raw_attribute_values(page, name)
        .into_iter()
        .map(|value| value.replace("&amp;", "&"))
        .collect()
}

fn raw_attribute_values(page: &str, name: &str) -> Vec<String> {
    let needle = format!(" {name}=\"");
    let mut out = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find(&needle) {
        rest = &rest[start + needle.len()..];
        let Some(end) = rest.find('"') else { break };
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

fn srcset_values(page: &str) -> Vec<String> {
    attribute_values(page, "srcset")
        .iter()
        .flat_map(|set| {
            set.split(',')
                .filter_map(|c| c.split_whitespace().next().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // WAI-ARIA 1.2 generic role: remove prohibited naming without changing
    // the result link, snippet text, ID or aria-details relationship.
    #[test]
    fn search_teasers_preserve_the_result_and_reject_template_changes() {
        let template = "            + '<span class=\"teaser\" id=\"mdbook-teaser_' + teaser_count\n            + '\" aria-label=\"Search Result Teaser\">' + teaser + '</span>';";
        let script = format!("link.setAttribute('aria-details', 'mdbook-teaser_1');\n{template}");
        let out = search_teasers(&script).unwrap();
        assert_eq!(
            out,
            script.replace(" aria-label=\"Search Result Teaser\"", "")
        );
        assert!(search_teasers("").is_err());
        assert!(search_teasers(&format!("{template}\n{template}")).is_err());
        assert!(search_teasers(&template.replace("Search Result Teaser", "Snippet")).is_err());
    }

    // docs/website.md accessibility policy: preserve content while making
    // mdBook's controls, scrolling regions and search status accessible.
    #[test]
    fn accessibility_fixes_report_what_they_matched() {
        let page = "<body><main><label id=\"mdbook-sidebar-toggle\" class=\"icon-button\" for=\"mdbook-sidebar-toggle-anchor\"></label>\
                    <pre><code class=\"language-sh\">a</code></pre><pre><code class=\"x\">b</code></pre>\
                    <input aria-describedby=\"searchresults-header\"><div id=\"mdbook-searchresults-header\" class=\"searchresults-header\"></div>";
        let (fixed, counts) = mdbook_accessibility(page, "../");
        // The captured fixture has eight applicable replacements: body,
        // main, two code blocks, the opener's two tags, search description and status.
        assert_eq!(counts.iter().sum::<usize>(), 8);
        assert!(fixed.contains("<button type=\"button\" id=\"mdbook-sidebar-toggle\""));
        assert!(fixed.contains("href=\"../toc.html\""));
        assert!(fixed.contains("href=\"#main-content\""));
        assert!(fixed.contains("<main id=\"main-content\" tabindex=\"-1\">"));
        assert_eq!(fixed.matches("<pre><code tabindex=\"0\" class=").count(), 2);
        assert!(fixed.contains("aria-describedby=\"mdbook-searchresults-header\""));
        assert!(fixed.contains("role=\"status\" aria-live=\"polite\" aria-atomic=\"true\""));
        let (fixed, _) =
            mdbook_accessibility("<a class=\"chapter-fold-toggle\"><div>❱</div></a>", "");
        assert!(fixed.contains("<button type=\"button\" class=\"chapter-fold-toggle\""));
        assert!(fixed.ends_with("</button>"));
        assert!(!fixed.contains("</a>"));
    }

    // docs/website.md accessibility policy: Contents is a named navigation
    // landmark, retaining its no-script links into the parent page.
    #[test]
    fn contents_landmark_preserves_the_helper_and_parent_targets() {
        let page = "<head><title>Contents - udpstp</title><meta name=\"robots\" content=\"noindex\"></head>\
                    <body class=\"sidebar-iframe-inner\"><ol class=\"chapter\"><li>\
                    <a href=\"getting-started.html\" target=\"_parent\">Getting started</a>\
                    </li></ol></body>";
        let out = contents_landmark(page).unwrap();
        assert_eq!(
            out.matches("<nav aria-label=\"Table of contents\">")
                .count(),
            1
        );
        assert_eq!(
            out.replace("<nav aria-label=\"Table of contents\">", "")
                .replace("</nav>", ""),
            page
        );
    }

    // docs/website.md guarded mdBook fixes: upstream template changes must
    // fail the build rather than silently discard navigation semantics.
    #[test]
    fn contents_landmark_rejects_changed_or_duplicate_body_markup() {
        for page in [
            "<body></body>",
            "<body class=\"sidebar-iframe-inner\">",
            "<body class=\"sidebar-iframe-inner\"><body class=\"sidebar-iframe-inner\"></body>",
            "<body class=\"sidebar-iframe-inner\"></body></body>",
        ] {
            assert!(contents_landmark(page).unwrap_err().contains("toc.html"));
        }
    }

    // docs/website.md metadata policy: each content page has one canonical
    // URL and matching title, description and truthful breadcrumbs.
    #[test]
    fn heads_get_canonical_urls_and_their_description() {
        let page = "<html lang=\"en\"><head><title>x</title><meta name=\"description\" content=\"book\"></head></html>";
        let out = head(
            page,
            "guides/a.md",
            "guides/a.html",
            "A \"guide\"",
            "About A.",
            Some(("Guides", "guides/index.html")),
        )
        .unwrap();
        let graph: Value = serde_json::from_str(
            out.split_once("<script type=\"application/ld+json\">")
                .unwrap()
                .1
                .split_once("</script>")
                .unwrap()
                .0,
        )
        .unwrap();
        assert_eq!(graph["@graph"][1]["itemListElement"][1]["name"], "Guides");
        assert_eq!(
            graph["@graph"][1]["itemListElement"][1]["item"],
            "https://udpstp.com/guides/index.html"
        );
        assert!(out.contains("<meta name=\"description\" content=\"About A.\">"));
        assert!(out.contains("<link rel=\"canonical\" href=\"https://udpstp.com/guides/a.html\">"));
        assert!(out.contains("<link rel=\"alternate\" type=\"text/markdown\" href=\"a.md\">"));
        assert_eq!(graph["@graph"][0]["headline"], "A \"guide\"");
        assert_eq!(graph["@graph"][1]["itemListElement"][2]["position"], 3);
        assert!(out.contains("<title>A &quot;guide&quot; - udpstp</title>"));
        assert!(
            out.contains("<meta name=\"twitter:title\" content=\"A &quot;guide&quot; - udpstp\">")
        );
    }

    // docs/website.md metadata policy: reject conflicting preview metadata
    // and validate the page's actual main heading and structured-data type.
    #[test]
    fn metadata_check_rejects_duplicate_or_disagreeing_preview_tags() {
        let template = "<html lang=\"en\"><head><title>Old</title><meta name=\"description\" content=\"book\">\
            <meta property=\"og:image\" content=\"https://udpstp.com/og.png\">\
            <meta property=\"og:image:width\" content=\"1200\"><meta property=\"og:image:height\" content=\"630\">\
            <meta property=\"og:image:alt\" content=\"udpstp: measure the capacity your network really delivers. A Rust implementation of UDPSTP (RFC 9946).\">\
            <meta name=\"twitter:card\" content=\"summary_large_image\"></head><body>\
            <div id=\"mdbook-menu-bar\" class=\"menu-bar sticky\"><h1 class=\"menu-title\">udpstp</h1></div>\
            <main id=\"main-content\" tabindex=\"-1\"><h1 id=\"guides\"><a class=\"header\" href=\"#guides\">Guides</a></h1></main></body></html>";
        let page = head(
            template,
            "guides/index.md",
            "guides/index.html",
            "Guides",
            "UDPSTP task guides for Client users and Server administrators.",
            None,
        )
        .unwrap();
        let url = "https://udpstp.com/guides/index.html";
        let urls = BTreeSet::from(["https://udpstp.com/".to_string(), url.to_string()]);
        assert_eq!(
            check_page_metadata(&page, url, "CollectionPage", &urls),
            Ok(())
        );
        let duplicate = page.replace(
            "</head>",
            "<meta name=\"description\" content=\"Extra description\"></head>",
        );
        assert!(
            check_page_metadata(&duplicate, url, "CollectionPage", &urls)
                .unwrap_err()
                .contains("exactly one")
        );
        let wrong_url = page.replace(
            "<meta property=\"og:url\" content=\"https://udpstp.com/guides/index.html\">",
            "<meta property=\"og:url\" content=\"https://udpstp.com/guides/\">",
        );
        assert!(
            check_page_metadata(&wrong_url, url, "CollectionPage", &urls)
                .unwrap_err()
                .contains("URLs disagree")
        );
        assert!(check_page_metadata(&page, url, "TechArticle", &urls).is_err());
        let missing_main = page
            .replace("<main id=\"main-content\" tabindex=\"-1\">", "<div>")
            .replace("</main>", "</div>");
        assert!(
            check_page_metadata(&missing_main, url, "CollectionPage", &urls)
                .unwrap_err()
                .contains("main element")
        );
        let two_content_headings = page.replace("</main>", "<h1>Extra title</h1></main>");
        assert!(
            check_page_metadata(&two_content_headings, url, "CollectionPage", &urls)
                .unwrap_err()
                .contains("main heading")
        );
    }

    #[test]
    fn upstream_print_and_toc_noindex_are_preserved_once() {
        // docs/website.md indexing policy excludes duplicate helper pages;
        // mdBook 0.5.4's print and table-of-contents templates emit this metadata.
        let upstream = "<!DOCTYPE HTML><html lang=\"en\"><head>\n        <meta charset=\"UTF-8\">\n        <meta name=\"robots\" content=\"noindex\">\n        <title>udpstp</title>\n</head><body></body></html>";
        assert_eq!(ensure_noindex(upstream).unwrap(), upstream);
        let error = upstream.replace("        <meta name=\"robots\" content=\"noindex\">\n", "");
        let fixed = ensure_noindex(&error).unwrap();
        assert_eq!(
            fixed
                .matches("<meta name=\"robots\" content=\"noindex\">")
                .count(),
            1
        );
        assert_eq!(ensure_noindex(&fixed).unwrap(), fixed);
        assert!(ensure_noindex(&upstream.replace("noindex", "index")).is_err());
        assert!(
            ensure_noindex(&upstream.replace(
                "</head>",
                "<meta name=\"robots\" content=\"noindex\"></head>"
            ))
            .is_err()
        );
    }

    // docs/website.md accessibility policy: the Contents helper has a title;
    // preserve an upstream title and reject ambiguous document heads.
    #[test]
    fn contents_title_is_added_once_and_upstream_titles_are_preserved() {
        let page = "<head><meta name=\"robots\" content=\"noindex\"></head>";
        let titled = ensure_contents_title(page).unwrap();
        assert!(titled.contains("<title>Contents - udpstp</title>"));
        assert_eq!(ensure_contents_title(&titled).unwrap(), titled);
        assert!(ensure_contents_title("<body></body>").is_err());
        assert!(ensure_contents_title("<title>a</title><title>b</title>").is_err());
    }

    // docs/website.md canonical serving policy: exact .html paths remain
    // stable, with a working root and one canonical home URL.
    #[test]
    fn hosting_preserves_html_routes_and_one_canonical_home() {
        let config = "{\n\"html_handling\": \"none\",\n}";
        let redirects = include_str!("../../../website/_redirects");
        assert_eq!(check_hosting_routes(config, redirects), Ok(()));
        assert!(check_hosting_routes("// \"html_handling\": \"none\",", redirects).is_err());
        assert!(check_hosting_routes(config, "/ /index.html 200\n/index.html / 301\n").is_err());
        assert!(check_hosting_routes(config, "").is_err());
    }

    #[test]
    fn attributes_and_paths() {
        let page = "<a href=\"a.html#x\">a</a><img src=\"b.svg\" srcset=\"c.svg 1x, d.svg 2x\">";
        assert_eq!(attribute_values(page, "href"), ["a.html#x"]);
        assert_eq!(srcset_values(page), ["c.svg", "d.svg"]);
        assert_eq!(normalize(Path::new("a/b/../c/./d")), PathBuf::from("a/c/d"));
        assert_eq!(percent_decode("RFC9946-6.1-4%20x"), "RFC9946-6.1-4 x");
    }
}
