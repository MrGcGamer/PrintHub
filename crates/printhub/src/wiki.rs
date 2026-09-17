//! The help wiki: Markdown pages compiled into the binary, checked and rendered once, and
//! searched in memory.
//!
//! A page's place in the tree is its slug: `filament/materials/pla` sits under
//! `filament/materials`, which must exist. The root page has the empty slug. Every internal
//! link and every image a page uses is checked when the wiki loads, so a broken one fails the
//! tests rather than a reader.

use std::{collections::HashMap, sync::LazyLock};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
use thiserror::Error;

/// Every Markdown file under `wiki/`, as `(slug, source)`. A test checks that none is missing.
const SOURCES: &[(&str, &str)] = &[
    ("", include_str!("../wiki/index.md")),
    ("printer", include_str!("../wiki/printer/index.md")),
    ("printer/nozzle", include_str!("../wiki/printer/nozzle.md")),
    ("queue", include_str!("../wiki/queue/index.md")),
    ("queue/upload", include_str!("../wiki/queue/upload.md")),
    ("queue/waiting", include_str!("../wiki/queue/waiting.md")),
    (
        "queue/print-windows",
        include_str!("../wiki/queue/print-windows.md"),
    ),
    ("filament", include_str!("../wiki/filament/index.md")),
    (
        "filament/spools",
        include_str!("../wiki/filament/spools.md"),
    ),
    ("filament/trays", include_str!("../wiki/filament/trays.md")),
    (
        "filament/weighing",
        include_str!("../wiki/filament/weighing.md"),
    ),
    (
        "filament/materials",
        include_str!("../wiki/filament/materials/index.md"),
    ),
    (
        "filament/materials/pla",
        include_str!("../wiki/filament/materials/pla.md"),
    ),
    (
        "filament/materials/pla-finishes",
        include_str!("../wiki/filament/materials/pla-finishes.md"),
    ),
    (
        "filament/materials/petg",
        include_str!("../wiki/filament/materials/petg.md"),
    ),
    (
        "filament/materials/abs",
        include_str!("../wiki/filament/materials/abs.md"),
    ),
    (
        "filament/materials/asa",
        include_str!("../wiki/filament/materials/asa.md"),
    ),
    (
        "filament/materials/pc",
        include_str!("../wiki/filament/materials/pc.md"),
    ),
    (
        "filament/materials/nylon",
        include_str!("../wiki/filament/materials/nylon.md"),
    ),
    (
        "filament/materials/tpu",
        include_str!("../wiki/filament/materials/tpu.md"),
    ),
    (
        "filament/materials/fibre-filled",
        include_str!("../wiki/filament/materials/fibre-filled.md"),
    ),
    (
        "filament/materials/drying",
        include_str!("../wiki/filament/materials/drying.md"),
    ),
    ("stats", include_str!("../wiki/stats/index.md")),
    ("stats/balances", include_str!("../wiki/stats/balances.md")),
    ("accounts", include_str!("../wiki/accounts.md")),
];

/// Slugs that routes other than a page's own already answer.
const RESERVED: [&str; 1] = ["search"];

/// A photo shown on a page, with the credit its licence requires.
pub struct Image {
    pub file: &'static str,
    pub bytes: &'static [u8],
    pub width: u32,
    pub height: u32,
    pub author: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub source_url: &'static str,
}

pub const IMAGES: &[Image] = &[
    Image {
        file: "abs-millennium-falcon.jpg",
        bytes: include_bytes!("../wiki/images/abs-millennium-falcon.jpg"),
        width: 960,
        height: 720,
        author: "syvwlch",
        license: "CC BY 2.0",
        license_url: "https://creativecommons.org/licenses/by/2.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:Millennium_Falcon_(5893908434).jpg",
    },
    Image {
        file: "nylon-snow-machine-nozzle.jpg",
        bytes: include_bytes!("../wiki/images/nylon-snow-machine-nozzle.jpg"),
        width: 960,
        height: 640,
        author: "Svitlana Lozova",
        license: "CC BY-SA 4.0",
        license_url: "https://creativecommons.org/licenses/by-sa/4.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:3D_printed_snow_machine_nozzle.jpg",
    },
    Image {
        file: "petg-pulley-shaft.jpg",
        bytes: include_bytes!("../wiki/images/petg-pulley-shaft.jpg"),
        width: 960,
        height: 720,
        author: "newman72",
        license: "CC BY 2.0",
        license_url: "https://creativecommons.org/licenses/by/2.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:First_PETG_3D_FLM_Printed_Pulley_Shaft_Design_(49014483321).jpg",
    },
    Image {
        file: "petg-warp-core-lamp.jpg",
        bytes: include_bytes!("../wiki/images/petg-warp-core-lamp.jpg"),
        width: 960,
        height: 720,
        author: "Larsan",
        license: "CC BY-SA 4.0",
        license_url: "https://creativecommons.org/licenses/by-sa/4.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:Stratum_0_Warpcore_frickelraum.jpg",
    },
    Image {
        file: "pla-benchy-black.jpg",
        bytes: include_bytes!("../wiki/images/pla-benchy-black.jpg"),
        width: 960,
        height: 711,
        author: "3DBenchy",
        license: "CC BY 2.0",
        license_url: "https://creativecommons.org/licenses/by/2.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:-3DBenchy_Ultimaker2_cura_layer_0,2mm_v3_(17034996319).jpg",
    },
    Image {
        file: "pla-galaxy-chopsticks.jpg",
        bytes: include_bytes!("../wiki/images/pla-galaxy-chopsticks.jpg"),
        width: 960,
        height: 519,
        author: "Marcosticks",
        license: "CC BY-SA 4.0",
        license_url: "https://creativecommons.org/licenses/by-sa/4.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:Marcosticks-3D-printed_Ergonomic_chopsticks.png",
    },
    Image {
        file: "pla-silk-stella-octangula.jpg",
        bytes: include_bytes!("../wiki/images/pla-silk-stella-octangula.jpg"),
        width: 960,
        height: 948,
        author: "Regular Polyhedron",
        license: "CC BY-SA 4.0",
        license_url: "https://creativecommons.org/licenses/by-sa/4.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:Pink_3D_Printed_Stella_Octangula_-3.jpg",
    },
    Image {
        file: "pla-translucent-benchy.jpg",
        bytes: include_bytes!("../wiki/images/pla-translucent-benchy.jpg"),
        width: 960,
        height: 711,
        author: "3DBenchy",
        license: "CC BY 2.0",
        license_url: "https://creativecommons.org/licenses/by/2.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:3DBenchy.com_-_-3DBenchy_-_Green_Translucent_PLA_v01_(16725186103).jpg",
    },
    Image {
        file: "pla-wood-benchy.jpg",
        bytes: include_bytes!("../wiki/images/pla-wood-benchy.jpg"),
        width: 960,
        height: 711,
        author: "3DBenchy",
        license: "CC BY 2.0",
        license_url: "https://creativecommons.org/licenses/by/2.0/",
        source_url: "https://commons.wikimedia.org/wiki/File:-3DBenchy_by_CT3D.xyz_-_3D-printed_with_ECO_Wood_filament_(16500744844).jpg",
    },
    Image {
        file: "tpu-flexible-wrench.jpg",
        bytes: include_bytes!("../wiki/images/tpu-flexible-wrench.jpg"),
        width: 960,
        height: 540,
        author: "Creative Tools",
        license: "CC BY 2.0",
        license_url: "https://creativecommons.org/licenses/by/2.0/",
        source_url: "https://www.flickr.com/photos/33907867@N02/16385359401",
    },
];

pub fn image(file: &str) -> Option<&'static Image> {
    IMAGES.iter().find(|image| image.file == file)
}

#[derive(Debug, Error)]
#[error("wiki page {slug:?}: {problem}")]
pub struct WikiError {
    pub slug: String,
    pub problem: String,
}

pub struct Page {
    pub slug: &'static str,
    pub title: String,
    /// One sentence, shown under the page's link in its section and in search results.
    pub summary: String,
    pub body: Vec<Section>,
    pub headings: Vec<Heading>,
    /// Lower-cased, for search only.
    keywords: String,
    order: i64,
    /// The text of each paragraph, list item, table cell and heading.
    blocks: Vec<String>,
}

impl Page {
    pub fn href(&self) -> String {
        href(self.slug)
    }
}

pub fn href(slug: &str) -> String {
    if slug.is_empty() {
        "/wiki".to_owned()
    } else {
        format!("/wiki/{slug}")
    }
}

/// Rendered Markdown followed by the photos of an image-only paragraph, if one ended it.
pub struct Section {
    pub html: String,
    pub gallery: Vec<Figure>,
}

pub struct Figure {
    pub image: &'static Image,
    pub alt: String,
    pub caption: String,
}

pub struct Heading {
    pub id: String,
    pub text: String,
}

pub struct Wiki {
    /// Depth-first in reading order, so a position in this list is also the tree order.
    pages: Vec<Page>,
    by_slug: HashMap<&'static str, usize>,
}

static WIKI: LazyLock<Result<Wiki, WikiError>> = LazyLock::new(|| Wiki::load(SOURCES));

pub fn wiki() -> Result<&'static Wiki, &'static WikiError> {
    WIKI.as_ref()
}

impl Wiki {
    fn load(sources: &[(&'static str, &str)]) -> Result<Self, WikiError> {
        let mut pages = Vec::new();
        let mut links = Vec::new();
        for (slug, source) in sources {
            let fail = |problem: String| WikiError {
                slug: (*slug).to_owned(),
                problem,
            };
            if RESERVED.contains(slug) {
                return Err(fail("the slug is taken by a route".into()));
            }
            let (front, markdown) = front_matter(source).map_err(fail)?;
            let rendered = render(markdown).map_err(fail)?;
            links.extend(rendered.links.into_iter().map(|link| (*slug, link)));
            let mut blocks = rendered.blocks;
            blocks.insert(0, front.summary.clone());
            pages.push(Page {
                slug,
                title: front.title,
                summary: front.summary,
                body: rendered.body,
                headings: rendered.headings,
                keywords: front.keywords.to_lowercase(),
                order: front.order,
                blocks,
            });
        }

        let by_slug: HashMap<&str, usize> = pages
            .iter()
            .enumerate()
            .map(|(index, page)| (page.slug, index))
            .collect();
        if by_slug.len() != pages.len() {
            return Err(WikiError {
                slug: String::new(),
                problem: "two pages share a slug".into(),
            });
        }
        for page in &pages {
            if let Some(parent) = parent(page.slug)
                && !by_slug.contains_key(parent)
            {
                return Err(WikiError {
                    slug: page.slug.to_owned(),
                    problem: format!("its parent {parent:?} does not exist"),
                });
            }
        }

        let mut ordered = Vec::with_capacity(pages.len());
        let mut remaining: Vec<Option<Page>> = pages.into_iter().map(Some).collect();
        depth_first(&mut remaining, &by_slug, "", &mut ordered);
        let by_slug = ordered
            .iter()
            .enumerate()
            .map(|(index, page)| (page.slug, index))
            .collect();
        let wiki = Self {
            pages: ordered,
            by_slug,
        };

        for (slug, link) in links {
            if !wiki.resolves(&link, slug) {
                return Err(WikiError {
                    slug: slug.to_owned(),
                    problem: format!("the link {link:?} leads nowhere"),
                });
            }
        }
        Ok(wiki)
    }

    pub fn page(&self, slug: &str) -> Option<&Page> {
        self.by_slug.get(slug).map(|index| &self.pages[*index])
    }

    pub fn children(&self, slug: &str) -> impl Iterator<Item = &Page> {
        self.pages
            .iter()
            .filter(move |page| !page.slug.is_empty() && parent(page.slug) == Some(slug))
    }

    /// From the root down to the page's parent.
    pub fn ancestors(&self, slug: &str) -> Vec<&Page> {
        let mut chain = Vec::new();
        let mut next = parent(slug);
        while let Some(slug) = next {
            if let Some(page) = self.page(slug) {
                chain.push(page);
            }
            next = parent(slug);
        }
        chain.reverse();
        chain
    }

    /// Whether `link` names a page, and a heading on it if it has a fragment. `from` resolves a
    /// bare `#fragment`. Links that leave the wiki are not checked.
    pub fn resolves(&self, link: &str, from: &str) -> bool {
        let (path, fragment) = link.split_once('#').unwrap_or((link, ""));
        let slug = if path.is_empty() {
            from
        } else if path == "/wiki" {
            ""
        } else if let Some(slug) = path.strip_prefix("/wiki/") {
            slug
        } else {
            return !path.starts_with('/') && path.contains("://");
        };
        self.page(slug).is_some_and(|page| {
            fragment.is_empty() || page.headings.iter().any(|heading| heading.id == fragment)
        })
    }

    /// Pages matching every word of `query`, best first. Titles weigh most, then keywords,
    /// then headings, then how often the words occur in the text.
    pub fn search(&self, query: &str) -> Vec<Hit<'_>> {
        let terms = terms(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(u32, usize, Hit)> = self
            .pages
            .iter()
            .enumerate()
            .filter_map(|(position, page)| {
                let title = fold(&page.title);
                let keywords = fold(&page.keywords);
                let headings: Vec<Vec<char>> =
                    page.headings.iter().map(|h| fold(&h.text)).collect();
                let blocks: Vec<Vec<char>> = page.blocks.iter().map(|b| fold(b)).collect();
                let mut score = 0;
                for term in &terms {
                    let in_body: usize = blocks.iter().map(|b| occurrences(b, term).len()).sum();
                    let term_score = 8 * u32::from(!occurrences(&title, term).is_empty())
                        + 5 * u32::from(!occurrences(&keywords, term).is_empty())
                        + 3 * u32::from(headings.iter().any(|h| !occurrences(h, term).is_empty()))
                        + in_body.min(5) as u32;
                    if term_score == 0 {
                        return None;
                    }
                    score += term_score;
                }

                let matched_in = |text: &[char]| {
                    terms
                        .iter()
                        .filter(|term| !occurrences(text, term).is_empty())
                        .count()
                };
                let anchor = if matched_in(&title) == terms.len() {
                    None
                } else {
                    headings
                        .iter()
                        .enumerate()
                        .map(|(i, heading)| (matched_in(heading), i))
                        .filter(|(matched, _)| *matched > 0)
                        .max_by_key(|(matched, i)| (*matched, std::cmp::Reverse(*i)))
                        .map(|(_, i)| &page.headings[i])
                };

                let best_block = blocks
                    .iter()
                    .enumerate()
                    .max_by_key(|(i, block)| {
                        (
                            matched_in(block),
                            block.len() >= SNIPPET_MIN_BLOCK,
                            std::cmp::Reverse(*i),
                        )
                    })
                    .map_or(0, |(i, _)| i);
                let snippet = snippet(&page.blocks[best_block], &blocks[best_block], &terms);
                Some((
                    score,
                    position,
                    Hit {
                        page,
                        anchor,
                        snippet,
                    },
                ))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        hits.into_iter().map(|(_, _, hit)| hit).collect()
    }
}

fn parent(slug: &str) -> Option<&str> {
    if slug.is_empty() {
        None
    } else {
        Some(slug.rsplit_once('/').map_or("", |(parent, _)| parent))
    }
}

/// Moves `slug` and then its descendants, siblings by `order` then title, into `ordered`.
fn depth_first(
    remaining: &mut [Option<Page>],
    by_slug: &HashMap<&str, usize>,
    slug: &str,
    ordered: &mut Vec<Page>,
) {
    if let Some(page) = by_slug.get(slug).and_then(|index| remaining[*index].take()) {
        ordered.push(page);
    }
    let mut children: Vec<(i64, String, &'static str)> = remaining
        .iter()
        .flatten()
        .filter(|page| !page.slug.is_empty() && parent(page.slug) == Some(slug))
        .map(|page| (page.order, page.title.clone(), page.slug))
        .collect();
    children.sort();
    for (_, _, child) in children {
        depth_first(remaining, by_slug, child, ordered);
    }
}

pub struct Hit<'a> {
    pub page: &'a Page,
    /// The heading that matched best, when the title alone does not explain the hit.
    pub anchor: Option<&'a Heading>,
    pub snippet: Vec<Snip>,
}

impl Hit<'_> {
    pub fn href(&self) -> String {
        match self.anchor {
            Some(heading) => format!("{}#{}", self.page.href(), heading.id),
            None => self.page.href(),
        }
    }
}

/// A run of snippet text, marked when it matched a search word.
#[derive(Debug, PartialEq, Eq)]
pub struct Snip {
    pub text: String,
    pub mark: bool,
}

struct FrontMatter {
    title: String,
    summary: String,
    keywords: String,
    order: i64,
}

/// Reads the `key: value` lines between the leading `---` fences.
fn front_matter(source: &str) -> Result<(FrontMatter, &str), String> {
    let rest = source
        .strip_prefix("---\n")
        .ok_or("no front matter: the file must start with ---")?;
    let (header, body) = rest
        .split_once("\n---\n")
        .ok_or("the front matter is not closed with ---")?;
    let mut fields = HashMap::new();
    for line in header.lines() {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("front matter line {line:?} is not key: value"))?;
        fields.insert(key.trim(), value.trim());
    }
    let required = |key: &str| {
        fields
            .get(key)
            .filter(|value| !value.is_empty())
            .map(|value| (*value).to_owned())
            .ok_or_else(|| format!("front matter needs {key}"))
    };
    Ok((
        FrontMatter {
            title: required("title")?,
            summary: required("summary")?,
            keywords: fields
                .get("keywords")
                .copied()
                .unwrap_or_default()
                .to_owned(),
            order: match fields.get("order") {
                Some(raw) => raw
                    .parse()
                    .map_err(|_| format!("order {raw:?} is not a number"))?,
                None => 0,
            },
        },
        body,
    ))
}

struct Rendered {
    body: Vec<Section>,
    headings: Vec<Heading>,
    blocks: Vec<String>,
    links: Vec<String>,
}

const MARKDOWN: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_HEADING_ATTRIBUTES)
    .union(Options::ENABLE_SMART_PUNCTUATION);

/// Renders `markdown`, giving every heading an id and turning a paragraph of nothing but
/// images into a gallery. Raw HTML is refused: the text is trusted, but markup belongs in the
/// templates.
fn render(markdown: &str) -> Result<Rendered, String> {
    let events: Vec<Event> = Parser::new_ext(markdown, MARKDOWN).collect();
    let mut body = Vec::new();
    let mut pending: Vec<Event> = Vec::new();
    let mut headings = Vec::new();
    let mut links = Vec::new();

    let mut index = 0;
    while index < events.len() {
        match &events[index] {
            Event::Start(Tag::Heading {
                level,
                id,
                classes,
                attrs,
            }) => {
                let end = closing(&events, index);
                let text = plain(&events[index + 1..end]);
                let id = id
                    .as_ref()
                    .map_or_else(|| anchor(&text), |id| id.to_string());
                if id.is_empty() {
                    return Err(format!("the heading {text:?} has no usable id"));
                }
                if headings.iter().any(|h: &Heading| h.id == id) {
                    return Err(format!("two headings have the id {id:?}"));
                }
                pending.push(Event::Start(Tag::Heading {
                    level: *level,
                    id: Some(id.clone().into()),
                    classes: classes.clone(),
                    attrs: attrs.clone(),
                }));
                pending.extend(events[index + 1..=end].iter().cloned());
                headings.push(Heading { id, text });
                index = end + 1;
            }
            Event::Start(Tag::Paragraph) => {
                let end = closing(&events, index);
                match gallery(&events[index + 1..end])? {
                    Some(figures) => {
                        let mut html = String::new();
                        html::push_html(&mut html, pending.drain(..));
                        body.push(Section {
                            html,
                            gallery: figures,
                        });
                        index = end + 1;
                    }
                    None => {
                        pending.push(events[index].clone());
                        index += 1;
                    }
                }
            }
            Event::Start(Tag::Image { .. }) => {
                return Err("an image must be alone in its paragraph, with other images".into());
            }
            Event::Html(raw) | Event::InlineHtml(raw) => {
                return Err(format!("raw HTML is not allowed: {raw:?}"));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                links.push(dest_url.to_string());
                pending.push(events[index].clone());
                index += 1;
            }
            other => {
                pending.push(other.clone());
                index += 1;
            }
        }
    }
    let mut html = String::new();
    html::push_html(&mut html, pending.into_iter());
    body.push(Section {
        html,
        gallery: Vec::new(),
    });

    Ok(Rendered {
        body,
        headings,
        blocks: blocks(&events),
        links,
    })
}

/// The index of the `End` matching the `Start` at `start`.
fn closing(events: &[Event], start: usize) -> usize {
    let mut depth = 0usize;
    for (index, event) in events.iter().enumerate().skip(start) {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
    }
    events.len() - 1
}

fn plain(events: &[Event]) -> String {
    let mut text = String::new();
    for event in events {
        match event {
            Event::Text(t) | Event::Code(t) => text.push_str(t),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text.trim().to_owned()
}

fn blocks(events: &[Event]) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for event in events {
        match event {
            Event::Text(t) | Event::Code(t) => current.push_str(t),
            Event::SoftBreak | Event::HardBreak => current.push(' '),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::TableCell
                | TagEnd::CodeBlock,
            ) => {
                let text = current.trim();
                if !text.is_empty() {
                    blocks.push(text.to_owned());
                }
                current.clear();
            }
            _ => {}
        }
    }
    blocks
}

/// The figures of a paragraph that holds only images, or `None` if it holds anything else.
fn gallery(inner: &[Event]) -> Result<Option<Vec<Figure>>, String> {
    let mut figures = Vec::new();
    let mut index = 0;
    while index < inner.len() {
        match &inner[index] {
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                let end = closing(inner, index);
                let image = image(dest_url).ok_or_else(|| format!("no image {dest_url:?}"))?;
                let alt = plain(&inner[index + 1..end]);
                if alt.is_empty() {
                    return Err(format!("the image {dest_url:?} needs alt text"));
                }
                figures.push(Figure {
                    image,
                    alt,
                    caption: title.to_string(),
                });
                index = end + 1;
            }
            Event::SoftBreak => index += 1,
            Event::Text(text) if text.trim().is_empty() => index += 1,
            _ => return Ok(None),
        }
    }
    Ok((!figures.is_empty()).then_some(figures))
}

/// `Print profile` → `print-profile`.
fn anchor(text: &str) -> String {
    let mut id = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_end_matches('-').to_owned()
}

fn terms(query: &str) -> Vec<Vec<char>> {
    let mut terms: Vec<Vec<char>> = Vec::new();
    for word in query.split(|c: char| !c.is_alphanumeric()) {
        let term = fold(word);
        if !term.is_empty() && !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms
}

/// Lower case one character at a time, so positions in the result are positions in the text.
fn fold(text: &str) -> Vec<char> {
    text.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

fn occurrences(haystack: &[char], needle: &[char]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    (0..=haystack.len() - needle.len())
        .filter(|&start| haystack[start..start + needle.len()] == *needle)
        .collect()
}

/// Blocks shorter than this, such as a table cell, make a poor snippet when a sentence matches
/// as well.
const SNIPPET_MIN_BLOCK: usize = 40;
/// Characters of context kept before the first match.
const SNIPPET_LEAD: usize = 60;
const SNIPPET_LENGTH: usize = 200;

/// About `SNIPPET_LENGTH` characters of `text` around the first match, cut at spaces, with
/// every match marked. `folded` is `fold(text)`.
fn snippet(text: &str, folded: &[char], terms: &[Vec<char>]) -> Vec<Snip> {
    let chars: Vec<char> = text.chars().collect();
    let first = terms
        .iter()
        .filter_map(|term| occurrences(folded, term).first().copied())
        .min()
        .unwrap_or(0);

    let mut start = first.saturating_sub(SNIPPET_LEAD);
    if start > 0 {
        start = (start..first)
            .find(|&i| chars[i - 1].is_whitespace())
            .unwrap_or(start);
    }
    let mut end = (start + SNIPPET_LENGTH).min(chars.len());
    if end < chars.len() {
        end = (first.max(start + 1)..end)
            .rev()
            .find(|&i| chars[i].is_whitespace())
            .unwrap_or(end);
    }

    let mut marked = vec![false; chars.len()];
    for term in terms {
        for at in occurrences(folded, term) {
            marked[at..at + term.len()].fill(true);
        }
    }

    let mut snips: Vec<Snip> = Vec::new();
    if start > 0 {
        snips.push(Snip {
            text: "…".into(),
            mark: false,
        });
    }
    for i in start..end {
        match snips.last_mut() {
            Some(last) if last.mark == marked[i] => last.text.push(chars[i]),
            _ => snips.push(Snip {
                text: chars[i].to_string(),
                mark: marked[i],
            }),
        }
    }
    if end < chars.len() {
        match snips.last_mut() {
            Some(last) if !last.mark => last.text.push('…'),
            _ => snips.push(Snip {
                text: "…".into(),
                mark: false,
            }),
        }
    }
    snips
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(title: &str, body: &str) -> String {
        format!("---\ntitle: {title}\nsummary: About {title}.\n---\n{body}")
    }

    fn load(pages: &[(&'static str, String)]) -> Result<Wiki, WikiError> {
        let sources: Vec<(&'static str, &str)> = pages
            .iter()
            .map(|(slug, text)| (*slug, text.as_str()))
            .collect();
        Wiki::load(&sources)
    }

    #[test]
    fn the_shipped_wiki_loads() {
        if let Err(err) = wiki() {
            panic!("{err}");
        }
    }

    #[test]
    fn every_markdown_file_is_listed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wiki");
        let mut found = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|ext| ext == "md") {
                    let relative = path.strip_prefix(&root).unwrap().with_extension("");
                    let slug = relative.to_str().unwrap().replace('\\', "/");
                    let slug = slug
                        .strip_suffix("index")
                        .map_or(slug.as_str(), |s| s.trim_end_matches('/'))
                        .to_owned();
                    found.push(slug);
                }
            }
        }
        found.sort();
        let mut listed: Vec<String> = SOURCES.iter().map(|(s, _)| (*s).to_owned()).collect();
        listed.sort();
        assert_eq!(found, listed);
    }

    #[test]
    fn every_image_file_is_listed_and_used() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wiki/images");
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        files.sort();
        let listed: Vec<&str> = IMAGES.iter().map(|image| image.file).collect();
        assert_eq!(files, listed, "IMAGES is sorted by file name");
        let wiki = wiki().unwrap();
        for image in IMAGES {
            let used = wiki.pages.iter().any(|page| {
                page.body
                    .iter()
                    .any(|s| s.gallery.iter().any(|f| f.image.file == image.file))
            });
            assert!(used, "{} is on no page", image.file);
        }
    }

    #[test]
    fn every_help_link_in_the_templates_resolves() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let wiki = wiki().unwrap();
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            for rest in text.split("href=\"/wiki").skip(1) {
                let link = format!("/wiki{}", rest.split('"').next().unwrap());
                if link.contains("{{") {
                    continue;
                }
                assert!(wiki.resolves(&link, ""), "{}: {link}", path.display());
                checked += 1;
            }
        }
        assert!(checked > 20, "only {checked} help links found");
    }

    #[test]
    fn slugs_nest_at_most_three_deep() {
        // The navigation template has one loop per level.
        for (slug, _) in SOURCES {
            assert!(slug.split('/').count() <= 3, "{slug}");
        }
    }

    #[test]
    fn pages_are_ordered_depth_first_by_order_then_title() {
        let wiki = load(&[
            ("", page("Home", "")),
            ("b", page("B", "")),
            (
                "a",
                "---\ntitle: A\nsummary: A.\norder: 2\n---\n".to_owned(),
            ),
            ("b/x", page("X", "")),
            (
                "c",
                "---\ntitle: C\nsummary: C.\norder: -1\n---\n".to_owned(),
            ),
        ])
        .unwrap();
        let slugs: Vec<&str> = wiki.pages.iter().map(|p| p.slug).collect();
        assert_eq!(slugs, ["", "c", "b", "b/x", "a"]);
        let children: Vec<&str> = wiki.children("").map(|p| p.slug).collect();
        assert_eq!(children, ["c", "b", "a"]);
        let ancestors: Vec<&str> = wiki.ancestors("b/x").iter().map(|p| p.slug).collect();
        assert_eq!(ancestors, ["", "b"]);
    }

    #[test]
    fn broken_pages_are_refused() {
        let root = ("", page("Home", ""));
        let problem = |pages: &[(&'static str, String)]| load(pages).err().unwrap().problem;
        assert!(problem(&[root.clone(), ("a/b", page("B", ""))]).contains("parent"));
        assert!(problem(&[root.clone(), ("a", page("A", "[x](/wiki/nope)"))]).contains("nowhere"));
        assert!(problem(&[root.clone(), ("a", page("A", "[x](/wiki#nope)"))]).contains("nowhere"));
        assert!(problem(&[root.clone(), ("a", page("A", "<b>x</b>"))]).contains("HTML"));
        assert!(problem(&[root.clone(), ("a", page("A", "![x](missing.jpg)"))]).contains("image"));
        assert!(
            problem(&[
                root.clone(),
                ("a", page("A", "Look: ![x](pla-wood-benchy.jpg)"))
            ])
            .contains("alone")
        );
        assert!(problem(&[root.clone(), ("a", page("A", "## Same\n\n## Same"))]).contains("two"));
        assert!(problem(&[root.clone(), ("search", page("S", ""))]).contains("route"));
        assert!(problem(&[("", "title: no fences".to_owned())]).contains("front matter"));
    }

    #[test]
    fn headings_get_ids_and_links_resolve_to_them() {
        let wiki = load(&[
            (
                "",
                page(
                    "Home",
                    "## Print profile\n\n### Custom {#mine}\n\n[here](#print-profile)",
                ),
            ),
            (
                "a",
                page("A", "[home](/wiki#mine) and [out](https://example.com)"),
            ),
        ])
        .unwrap();
        let home = wiki.page("").unwrap();
        assert!(home.body[0].html.contains(r#"<h2 id="print-profile">"#));
        assert!(wiki.resolves("/wiki#mine", "a"));
        assert!(wiki.resolves("#print-profile", ""));
        assert!(!wiki.resolves("/wiki/a#print-profile", ""));
        assert_eq!(anchor("Bed, textured PEI (°C)"), "bed-textured-pei-c");
    }

    #[test]
    fn image_paragraphs_become_galleries_between_the_text() {
        let wiki = load(&[(
            "",
            page(
                "Home",
                "Before.\n\n![A boat](pla-wood-benchy.jpg \"Wood PLA\")\n![Another](pla-benchy-black.jpg)\n\nAfter.",
            ),
        )])
        .unwrap();
        let body = &wiki.page("").unwrap().body;
        assert_eq!(body.len(), 2);
        assert!(body[0].html.contains("Before."));
        let captions: Vec<&str> = body[0].gallery.iter().map(|f| f.caption.as_str()).collect();
        assert_eq!(captions, ["Wood PLA", ""]);
        assert_eq!(body[0].gallery[0].alt, "A boat");
        assert!(body[1].html.contains("After.") && body[1].gallery.is_empty());
    }

    #[test]
    fn search_needs_every_word_and_ranks_titles_first() {
        let wiki = load(&[
            ("", page("Home", "Nothing about spools here.")),
            ("spools", page("Spools", "A spool holds filament.")),
            (
                "trays",
                page("Trays", "## Binding a spool\n\nPut the spool in a tray."),
            ),
        ])
        .unwrap();
        let slugs = |query: &str| -> Vec<&str> {
            wiki.search(query).iter().map(|hit| hit.page.slug).collect()
        };
        assert_eq!(slugs("spool"), ["spools", "trays", ""]);
        assert_eq!(slugs("SPOOL tray"), ["trays"]);
        assert!(slugs("").is_empty() && slugs("—").is_empty());

        let hits = wiki.search("binding");
        assert_eq!(hits[0].href(), "/wiki/trays#binding-a-spool");
        let spools = wiki.search("spools");
        assert_eq!(
            spools[0].href(),
            "/wiki/spools",
            "a title match needs no anchor"
        );
    }

    #[test]
    fn snippets_mark_matches_and_cut_at_spaces() {
        let text = format!(
            "{} needle in the middle {}",
            "word ".repeat(30),
            "tail ".repeat(60)
        );
        let snips = snippet(&text, &fold(&text), &terms("NEEDLE"));
        let joined: String = snips.iter().map(|s| s.text.as_str()).collect();
        assert!(joined.starts_with("…word"), "{joined}");
        assert!(joined.ends_with("tail…"), "{joined}");
        let marked: Vec<&str> = snips
            .iter()
            .filter(|s| s.mark)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(marked, ["needle"]);

        let short = snippet("PLA and pla", &fold("PLA and pla"), &terms("pla"));
        assert_eq!(
            short,
            [
                Snip {
                    text: "PLA".into(),
                    mark: true
                },
                Snip {
                    text: " and ".into(),
                    mark: false
                },
                Snip {
                    text: "pla".into(),
                    mark: true
                },
            ]
        );
    }
}
