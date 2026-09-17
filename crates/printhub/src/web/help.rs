//! The help wiki's pages and search. Named `help` so it does not clash with `crate::wiki`.

use askama::Template;
use axum::{
    extract::{Path, Query},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::{
    error::{AppError, render},
    session::CurrentUser,
};
use crate::{
    accounts::User,
    wiki::{self, Page, Section, Snip, Wiki},
};

#[derive(Template)]
#[template(path = "wiki.html")]
struct WikiPage<'a> {
    user: Option<User>,
    nav: Vec<NavItem>,
    home: Link,
    query: String,
    crumbs: Vec<Link>,
    title: &'a str,
    body: &'a [Section],
    children: Vec<Child>,
}

#[derive(Template)]
#[template(path = "wiki_search.html")]
struct SearchPage {
    user: Option<User>,
    nav: Vec<NavItem>,
    home: Link,
    query: String,
    hits: Vec<HitRow>,
}

/// A page in the navigation tree. `open` is set on the current page and its ancestors, so the
/// reader's place stays expanded.
struct NavItem {
    href: String,
    title: String,
    current: bool,
    open: bool,
    children: Vec<NavItem>,
}

struct Link {
    href: String,
    title: String,
}

struct Child {
    href: String,
    title: String,
    summary: String,
}

struct HitRow {
    href: String,
    title: String,
    /// The page's sections above it, and the heading the link lands on.
    place: String,
    snippet: Vec<Snip>,
}

fn loaded() -> Result<&'static Wiki, AppError> {
    wiki::wiki().map_err(|err| AppError::Internal(anyhow::anyhow!("{err}")))
}

pub async fn index(current: CurrentUser) -> Result<Response, AppError> {
    show(current.user, "")
}

pub async fn page(current: CurrentUser, Path(slug): Path<String>) -> Result<Response, AppError> {
    show(current.user, slug.trim_end_matches('/'))
}

fn show(user: User, slug: &str) -> Result<Response, AppError> {
    let wiki = loaded()?;
    let page = wiki.page(slug).ok_or(AppError::NotFound)?;
    let view = WikiPage {
        user: Some(user),
        nav: nav(wiki, page.slug),
        home: home(wiki)?,
        query: String::new(),
        crumbs: wiki
            .ancestors(page.slug)
            .into_iter()
            .map(|ancestor| Link {
                href: ancestor.href(),
                title: ancestor.title.clone(),
            })
            .collect(),
        title: &page.title,
        body: &page.body,
        children: wiki
            .children(page.slug)
            .map(|child| Child {
                href: child.href(),
                title: child.title.clone(),
                summary: child.summary.clone(),
            })
            .collect(),
    };
    Ok(render(&view)?.into_response())
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
}

pub async fn search(
    current: CurrentUser,
    Query(query): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let wiki = loaded()?;
    let hits = wiki
        .search(&query.q)
        .into_iter()
        .map(|hit| {
            let mut place: Vec<&str> = wiki
                .ancestors(hit.page.slug)
                .iter()
                .skip(1)
                .map(|page| page.title.as_str())
                .collect();
            if let Some(heading) = hit.anchor {
                place.push(&heading.text);
            }
            HitRow {
                href: hit.href(),
                title: hit.page.title.clone(),
                place: place.join(" › "),
                snippet: hit.snippet,
            }
        })
        .collect();
    let view = SearchPage {
        user: Some(current.user),
        nav: nav(wiki, ""),
        home: home(wiki)?,
        query: query.q,
        hits,
    };
    Ok(render(&view)?.into_response())
}

fn home(wiki: &Wiki) -> Result<Link, AppError> {
    let root = wiki.page("").ok_or(AppError::NotFound)?;
    Ok(Link {
        href: root.href(),
        title: root.title.clone(),
    })
}

/// The root's children and their descendants; the root itself is `home`, above them.
fn nav(wiki: &Wiki, current: &str) -> Vec<NavItem> {
    fn item(wiki: &Wiki, page: &Page, current: &str) -> NavItem {
        let children: Vec<NavItem> = wiki
            .children(page.slug)
            .map(|child| item(wiki, child, current))
            .collect();
        let is_current = page.slug == current;
        NavItem {
            href: page.href(),
            title: page.title.clone(),
            current: is_current,
            open: is_current || children.iter().any(|child| child.open),
            children,
        }
    }
    wiki.children("")
        .map(|page| item(wiki, page, current))
        .collect()
}
