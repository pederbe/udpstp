# The website

The website combines a static landing page with documentation built from this repository's Markdown using mdBook. This guide explains its sources, local build and checks.

## What it is made of

| Part | Where | Notes |
|---|---|---|
| Landing page | `website/index.html`, `landing.css`, `landing.js` | Static HTML and CSS, one external script (copy buttons, install tabs and motion), no framework, no third-party requests, no cookies, no analytics. Strict CSP: no inline script or style. Its numbers and terminal output are filled in at build time. |
| Docs theme | `website/book.toml`, `website/theme/` | mdBook 0.5.4's navy theme, retinted to palette D, plus a retinted light theme; `head.hbs` adds the author, Open Graph and the docs CSP, which allows mdBook's inline scripts. Without JavaScript the docs keep the navy palette, the default for every reader. |
| Docs pages | The repository's Markdown | `docs/getting-started.md`, `docs/guides/`, `docs/interfaces.md`, `docs/json-output.md`, `docs/security.md`, `docs/architecture.md`, `docs/technology.md`, `docs/adr/`, `docs/conformance/README.md`, `GLOSSARY.md`, `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, and `website/pages/` for About. Links to other repository files become GitHub links pinned to the release tag; a link to a file Git doesn't track fails the build. |
| Generated pages | `xtask/src/site/` | The command line (from `udpstp --help`), Server configuration (from `--print-config` and `ServerSettings`' documentation), exit statuses (from `exit.rs`), the key files page's recorded choices (from `keyfile.rs`, after `docs/key-files.md`), the conformance pages (from `cargo xtask req`'s data: coverage, every Requirement with its state and proving tests, and the Conformance Records by kind), the glossary (from `GLOSSARY.md`, one anchor per term), the ADR index and the citation. |
| Diagrams | Mermaid blocks in the Markdown | Rendered to SVG at build time by mermaid-cli, in a dark and a light variant (`website/diagrams/`), with 18px labels. Large figures keep their native label size in a named, keyboard-accessible scroll region. Each Mermaid block must be followed by a `<!-- description: … -->` line, which becomes the figure's caption and text alternative outside the scroll region. The served pages load no mermaid.js. |
| Images | `website/og.html`, `website/og.png`, `website/render.mjs` | The committed 1200x630 link-preview image and the PNG icons rendered from the logo (`docs/assets/logo/`). `website/fonts/fontconfig.conf` fixes Linux font rendering for reproducible social-image checks. |
| Fonts | `website/fonts/` | A subset of IBM Plex Sans Condensed Bold, renamed "udpstp Display" as its licence requires, and bundled DejaVu fonts for the social image ([provenance.md](provenance.md#website-fonts)). Body text uses the system font; code uses mdBook's Source Code Pro. |
| Hosting files | `website/_headers`, `website/_redirects`, `website/wrangler.jsonc` | Response headers (HSTS, CSP, and others) and the configuration for Cloudflare Workers static assets. Exact `.html` documentation URLs agree with the canonical URLs and sitemap; a root rewrite serves the landing page and `/index.html` redirects to `/`. The landing page may not be framed at all; the docs may be framed by the site itself, since mdBook's chapter list falls back to an `<iframe>` of `toc.html` without JavaScript. |

Every build also copies the install scripts (`install/install.sh` and `install/install.ps1`, [Installing udpstp](guides/installing-udpstp.md)) to the site's root, and writes `sitemap.xml`, `robots.txt`, `llms.txt`, `llms-full.txt`, a Markdown copy of every docs page beside its HTML (linked as its `text/markdown` alternate), `site.webmanifest`, `.well-known/security.txt` and a 404 page. Each content page has its own description, canonical URL, matching Open Graph and Twitter metadata, and JSON-LD appropriate to the page's content, with BreadcrumbList destinations that resolve.

The finish step makes guarded corrections to mdBook's generated controls and
navigation: a focusable main target, named native buttons and Contents landmark,
scrolling regions and a polite search-result status. Search snippets retain their
text and the result link's `aria-details` relationship without assigning a name
to generic text. Changed upstream templates fail the build so that an upgrade
cannot silently discard these corrections.

The docs use a dark and light theme with blue and coral accents. The
masthead carries the logo mark and name, which lead to the landing page, and a
Docs label leading to Getting started. In the chapter list, parts are labelled
like the landing page's eyebrows and the current chapter has a coral rail. Each
page title sits over a coral rule that draws in once on load. Second-level
headings carry the signal-blue rail and third-level headings are coral. Code
blocks, tables, notes and diagrams sit on lifted panels. Commands appear in a
signal-blue terminal like the landing page's, labelled Terminal or PowerShell,
and configuration files are labelled TOML. Getting started and the guides
overview open with a larger paragraph, and Getting started's closing links
become cards.

The Docs chapter list starts open on desktop and collapsed on smaller screens.
mdBook remembers a reader's choice made with the menu button. Getting started
focuses on a first authenticated Test. Guides give the steps for a task,
Explanations say why, and Reference pages list every option, event and field. The glossary begins with a browser-find shortcut.
On the first visit, a previously saved closed sidebar is reset to apply the new
default, whether it came from initialization or a reader's choice. Menu choices
made afterwards are remembered.
Conformance and Decisions start collapsed; the current page's branch opens
automatically, and their buttons let readers expand or collapse them.
The chapter list keeps its reserved space while mdBook adds the current page's
section links, then becomes visible at DOM readiness. Without JavaScript it is
visible immediately. This prevents its entries from jumping during startup.

The landing page's external script loads in the document head. It prepares the
first installation method's layout before the body is parsed, then connects the
tabs and copy buttons when the document is ready. Pending controls reserve their
space while remaining invisible and unfocusable. The initial panel named in CSS
must match the first tab chosen by the script. Without JavaScript, or if the
script cannot load, every method and its selectable command remain visible.
If setup fails partway through, it restores that fallback. This avoids a layout
shift when setup succeeds, but a slow script request delays the first paint;
recovery from a setup failure can change the layout as all methods become visible.

The head script marks entrances
as ready before the first paint, so the headline's words rise once on load and
each block fades up once as it enters view (1.1 seconds, a short stagger). The
terminal then prints its fixture output line by line and its cursor blinks
three times. Focusing anything inside a block, or printing, settles it at once.
Decorative loops run only while visible and stop in hidden tabs: a breathing
glow behind the hero, a pulse of light along the capacity search trace at the
hero's foot, the install card's aura, and the Data Phase illustration, where
Load PDUs flow from the Server to the Client, a Status PDU returns every
3-second Trial Interval and the Load Rate Adjustment Algorithm answers it. That
illustration also holds still while hovered or focused. A page-wide Pause
motion button, remembered in local storage, stops every loop. Reduced motion,
browsers without the Web Animations API or IntersectionObserver, a page
without JavaScript and print all show the complete, still result; the packets
and the pulse are then hidden. If the script fails after preparing entrances,
it removes the mark and the content shows. Only transforms, opacity, clip and
a box shadow animate, and decorative overflow is clipped at the page edge.

The simulated Client uses the CLI's colour roles with theme-specific shades:
blue headings, a green Maximum IP-Layer Capacity result, red
`not measured` markers, and a bold command. Table values and explanatory text
stay neutral. The underlying fixture text and column spacing are preserved;
colour adds emphasis without carrying information by itself. Forced colours
use the reader's system colours, and print output is plain dark text.

## Build it

On Linux or WSL, with Rust, Node.js 22.13 or later, and mdBook 0.5.4:

```sh
cargo install --locked mdbook@0.5.4
scripts/build-site.sh --dev
```

`cargo xtask site prepare` refuses to run on Windows itself; use WSL.

The script installs the pinned build tools (`npm ci --ignore-scripts` in `website/`, and `npx puppeteer browsers install chrome`, which downloads a headless Chrome into `~/.cache/puppeteer`), builds the `udpstp` binary for the reference pages, copies the committed social image, renders the icons, runs `cargo xtask site prepare`, `mdbook build`, and `cargo xtask site finish`. The site is in `target/site/public/`.

After changing `website/og.html`, run `node website/render.mjs target/site/rendered --update-social` and commit `website/og.png` with its source. Website CI uses `--check-social` to compare the committed image with a fresh render. Normal builds copy the image without rendering its text again.

Options:

- **`--dev`** builds a local preview (it sets `UDPSTP_SITE_DEV=1`): from a tree with uncommitted changes, or without the release tag, with links to repository files pinned to the current commit, and links to untracked files dropped with a warning, keeping their text.
- **`--audit`** also runs `npm audit` on the build tools. It asks the npm registry, so the default build leaves it out.

Without `--dev`, the build requires a clean tree and either the tag `v<version>` on the checked-out commit or `UDPSTP_SITE_REF` naming the revision used for repository links. It also checks that linked files are tracked. The badge shows the version. For an untagged source revision, llms.txt links to the latest published release and the source build instructions. Repository links pin to the release tag or the exact revision supplied through `UDPSTP_SITE_REF`.

**Supply chain.** The npm packages are pinned by the lock file, which records their hashes, and installed without running their install scripts. Puppeteer's Chrome is pinned by version, through the lock file's Puppeteer, but its download is not checked against a hash. Everything runs at build time only; none of it is served.

## Preview it

Serve the built site on the loopback interface only:

```sh
python3 -m http.server --bind 127.0.0.1 --directory target/site/public 8000
```

and open <http://127.0.0.1:8000/>. The landing page follows the system's light or dark setting; the docs have mdBook's theme picker.

For browser audits, use the intended response headers and routing through a local Cloudflare preview:

```sh
env WRANGLER_SEND_METRICS=false npx --yes wrangler@4.147.0 dev --config website/wrangler.jsonc --local --ip 127.0.0.1 --port 8000
```

This serves local files and does not deploy them. Start the preview after the build completes, and restart it after rebuilding or changing `_redirects`: replacing the output directory can leave asset watching with stale routing rules or headers. Check `/`, `/index.html`, a nested `.html` page, `toc.html`, `robots.txt`, `sitemap.xml` and a missing page with their response headers.

## What the build checks

`cargo xtask site finish` fails the build on any of these, and `cargo xtask site check` runs them again:

- **Links:** every internal link and image, and every `#anchor`, resolves.
- **No third-party resources:** no page loads a script, style or image from another site.
- **Contrast:** every token pair in `website/contrast.toml`, for the landing page and the docs, dark and light, meets WCAG 2.2 AA (4.5:1 for text, 3:1 for large text and UI). The tokens are read from the stylesheets themselves.
- **Text alternatives:** a Mermaid diagram without a description fails the build.
- **Page identities:** content-page metadata and JSON-LD agree with their titles, descriptions, canonical URLs, author and sitemap membership. Print, contents and error pages stay out of the sitemap and request no indexing.

`cargo xtask site prepare` also fails if the landing page's template names a number it doesn't fill, or if the conformance data gains a state the landing page's table has no column for.

Lighthouse and axe are separate browser checks, using the local preview above. Keep Chrome and Lighthouse versions and throttling settings fixed when comparing changes. Record three runs per page, theme and device profile, compare the median Performance and its metrics, and retain the raw reports. A representative set covers the landing page, Getting started, command line, configuration, conformance, glossary and an ADR. Lighthouse 13.5.0 can be run with:

```sh
npx --yes lighthouse@13.5.0 http://127.0.0.1:8000/ --only-categories=performance,accessibility,best-practices,seo --output=json --output-path=/tmp/udpstp-lighthouse.json
```

Set `CHROME_PATH` to the existing Chrome executable if Lighthouse does not find it. Use the mobile profile and `--preset=desktop`, and check both the light and dark themes. Run axe against every content page and the interactive navigation, search, theme and installation states. Supplement these checks with keyboard use, no-JavaScript content, reduced motion, forced colours, text spacing, 200% text enlargement, 400% zoom and print output. Real screen-reader checks are optional. A score alone does not prove that those interactions work.

## Hosting

The [hosting configuration](../website/wrangler.jsonc) serves `target/site/public` as Cloudflare Workers static assets. The [website workflow](../.github/workflows/website.yml) builds and checks that output. Its build and delivery are separate from binary release packaging.

Each push to the protected `main` branch builds and checks the website, then deploys the retained output when website delivery is enabled. Deployment verifies that the checked revision is still the current `main` revision. Pull requests retain their website checks in CI. A manual website run builds without deployment unless `deploy` is selected.

The site includes `/install.sh` and `/install.ps1`, copied from `install/` at the same source revision as the documentation. They are served as plain text with `no-cache`. They find the latest published binary through GitHub's `releases/latest` endpoint.
