//! Maud templates for the registry block's HTML pages.
//!
//! All templates share a common `layout` skeleton with navigation. Individual
//! page renderers return `Markup` which is then serialized to a response body.

use maud::{html, Markup, DOCTYPE};

use crate::blocks::registry::models::{PackageDetail, PackageSummary};

/// Shared page skeleton with navigation header.
pub fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href="/static/site.css";
            }
            body {
                header { nav {
                    a href="/" { "wafer" }
                    a href="/docs" { "Docs" }
                    a href="/registry" { "Registry" }
                    a href="/playground" { "Playground" }
                }}
                main { (body) }
            }
        }
    }
}

/// Registry index and search results page.
pub fn browse(packages: &[PackageSummary], query: &str, total: i64) -> Markup {
    layout("Registry", html! {
        h1 { "Package Registry" }
        form method="get" action="/registry" {
            input type="search" name="q" value=(query) placeholder="search packages";
            button type="submit" { "Search" }
        }
        @if packages.is_empty() {
            p.empty { "No packages published yet." }
        } @else {
            p { (total) " packages" }
            ul.packages {
                @for p in packages {
                    li {
                        a href={ "/registry/" (p.org) "/" (p.name) } {
                            strong { (p.org) "/" (p.name) }
                            @if let Some(latest) = &p.latest {
                                span.version { "@" (latest) }
                            }
                        }
                        @if let Some(s) = &p.summary { p.summary { (s) } }
                    }
                }
            }
        }
    })
}

/// Package detail page with version history and install snippet.
pub fn package_detail(pkg: &PackageDetail) -> Markup {
    layout(&format!("{}/{}", pkg.org, pkg.name), html! {
        h1 { (pkg.org) "/" (pkg.name) }
        @if let Some(s) = &pkg.summary { p.summary { (s) } }
        h2 { "Versions" }
        table.versions {
            thead { tr { th { "Version" } th { "ABI" } th { "Size" } th { "Published" } th { "" } } }
            tbody {
                @for v in &pkg.versions {
                    tr {
                        td { (v.version) }
                        td { (v.abi) }
                        td { (v.size_bytes) " bytes" }
                        td { (v.published_at) }
                        td {
                            @if v.yanked == 1 {
                                span.yanked { "YANKED" }
                            } @else {
                                a href={
                                    "/registry/download/" (pkg.org) "/" (pkg.name) "/" (v.version) ".wafer"
                                } { "Download" }
                            }
                        }
                    }
                }
            }
        }
        h2 { "Install" }
        pre { code { "wafer install " (pkg.org) "/" (pkg.name) } }
    })
}

/// 404 Not Found page.
pub fn not_found(what: &str) -> Markup {
    layout("Not Found", html! {
        h1 { "404" }
        p { (what) " not found." }
        p { a href="/registry" { "Back to registry" } }
    })
}

/// Coming soon placeholder (used by Task 11).
pub fn coming_soon() -> Markup {
    layout("Coming Soon", html! {
        div.coming-soon {
            h1 { "Publishing is coming soon" }
            p { "Publishing is not yet open to other users. Admins can continue via the CLI." }
        }
    })
}
