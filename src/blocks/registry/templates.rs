//! Maud templates for the registry block's HTML pages.
//!
//! All templates share a common `layout` skeleton with navigation. Individual
//! page renderers return `Markup` which is then serialized to a response body.

use maud::{html, Markup, DOCTYPE};

use crate::blocks::registry::models::{PackageDetail, PackageSummary};

/// Shared page skeleton — matches the kit chrome used by content/index.html
/// (sa-header + sa-footer + design-system.css + theme.css) so registry pages
/// share the wafer.run look.
pub fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " — wafer.run" }
                link rel="icon" type="image/svg+xml" href="/favicon.svg";
                link rel="stylesheet" href="https://site-kit.suppers.ai/dist/design-system.css";
                script type="module" src="https://site-kit.suppers.ai/dist/components/sa-header.js" {}
                script type="module" src="https://site-kit.suppers.ai/dist/components/sa-footer.js" {}
                link rel="stylesheet" href="/css/theme.css";
            }
            body {
                a class="skip-link" href="#main-content" { "Skip to content" }
                sa-header {
                    a slot="brand" href="/" {
                        span class="brand-mark" aria-hidden="true" { "W" }
                        span { "wafer" span style="color: var(--text-secondary);" { ".run" } }
                    }
                    a slot="actions" href="/docs" { "Docs" }
                    a slot="actions" href="/playground" { "Playground" }
                    a slot="actions" href="/registry" { "Registry" }
                    a slot="actions" href="https://github.com/wafer-run/wafer-run"
                        target="_blank" rel="noopener" aria-label="GitHub"
                        style="display: flex; align-items: center; color: var(--sa-text);" {
                        svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor" {
                            path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0024 12c0-6.63-5.37-12-12-12z" {}
                        }
                    }
                }
                main id="main-content" { (body) }
                sa-footer {
                    div slot="brand" {
                        strong style="font-weight: 600; color: var(--sa-text);" {
                            "wafer" span style="color: var(--text-secondary);" { ".run" }
                        }
                        p style="margin: var(--sa-space-1) 0 0; color: var(--sa-text-muted); font-size: var(--sa-text-sm);" {
                            "The wafer thin runtime for tools, apps, and services."
                        }
                    }
                    a slot="links" href="/docs" { "Docs" }
                    a slot="links" href="/playground" { "Playground" }
                    a slot="links" href="/registry" { "Registry" }
                    a slot="links" href="https://github.com/wafer-run/wafer-run"
                        target="_blank" rel="noopener" { "GitHub" }
                    span slot="copyright" { "© 2026 wafer.run · MIT licensed" }
                }
            }
        }
    }
}

/// Registry index and search results page.
pub fn browse(packages: &[PackageSummary], query: &str, total: i64) -> Markup {
    layout(
        "Registry",
        html! {
            div class="page-title" {
                h1 { "Package Registry" }
                p { "Discover and install wafer blocks, flows, and interfaces" }
            }
            div class="registry-container" {
                form class="registry-search" method="get" action="/registry" {
                    input type="search" name="q" value=(query)
                        placeholder="Search packages…" autocomplete="off";
                    button type="submit" { "Search" }
                }
                @if packages.is_empty() && query.is_empty() {
                    div class="registry-empty" {
                        h2 { "No packages published yet" }
                        p { "Be the first to publish. Authenticate the CLI, then push a release:" }
                        pre { code { "wafer publish ./my-block" } }
                        p {
                            "See the "
                            a href="/docs/registry" { "registry docs" }
                            " for publishing details."
                        }
                    }
                } @else if packages.is_empty() {
                    div class="registry-empty" {
                        h2 { "No matches for “" (query) "”" }
                        p { "Try a broader query, or " a href="/registry" { "browse all packages" } "." }
                    }
                } @else {
                    p class="registry-count" { (total) " package" @if total != 1 { "s" } }
                    ul class="registry-list" {
                        @for p in packages {
                            li class="registry-item" {
                                a class="registry-item-link" href={ "/registry/" (p.org) "/" (p.name) } {
                                    span class="registry-item-name" {
                                        span class="registry-org" { (p.org) "/" }
                                        strong { (p.name) }
                                    }
                                    @if let Some(latest) = &p.latest {
                                        span class="registry-version" { "v" (latest) }
                                    }
                                }
                                @if let Some(s) = &p.summary { p class="registry-item-summary" { (s) } }
                            }
                        }
                    }
                }
            }
        },
    )
}

/// Package detail page with version history and install snippet.
pub fn package_detail(pkg: &PackageDetail) -> Markup {
    layout(
        &format!("{}/{}", pkg.org, pkg.name),
        html! {
            div class="page-title" {
                h1 { (pkg.org) "/" (pkg.name) }
                @if let Some(s) = &pkg.summary { p { (s) } }
            }
            div class="registry-container" {
                h2 { "Install" }
                pre { code { "wafer install " (pkg.org) "/" (pkg.name) } }
                h2 { "Versions" }
                table class="registry-versions" {
                    thead { tr { th { "Version" } th { "ABI" } th { "Size" } th { "Published" } th { "" } } }
                    tbody {
                        @for v in &pkg.versions {
                            tr {
                                td { "v" (v.version) }
                                td { (v.abi) }
                                td { (v.size_bytes) " bytes" }
                                td { (v.published_at) }
                                td {
                                    @if v.yanked == 1 {
                                        span class="registry-yanked" { "YANKED" }
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
            }
        },
    )
}

/// 404 Not Found page.
pub fn not_found(what: &str) -> Markup {
    layout(
        "Not Found",
        html! {
            div class="registry-container registry-empty" {
                h1 { "404" }
                p { (what) " not found." }
                p { a href="/registry" { "← Back to registry" } }
            }
        },
    )
}

/// CLI login device-code display.
///
/// Admin-only: rendered after `require_admin` passes and a fresh code has
/// been issued via `db::issue_cli_code`. The `<pre>` wrapping makes copying
/// the 64-hex-char code trivial.
pub fn cli_login_code(code: &str) -> Markup {
    layout(
        "CLI Login",
        html! {
            div class="page-title" {
                h1 { "CLI Login" }
                p { "Paste this code into your CLI prompt. Valid for 15 minutes." }
            }
            div class="registry-container" {
                pre class="cli-code" { code { (code) } }
                p style="color: var(--text-secondary);" { "This code is single-use." }
            }
        },
    )
}

/// Coming soon placeholder (used by Task 11).
pub fn coming_soon() -> Markup {
    layout(
        "Coming Soon",
        html! {
            div class="registry-container registry-empty" {
                h1 { "Publishing is coming soon" }
                p { "Publishing is not yet open to other users. Admins can continue via the CLI." }
            }
        },
    )
}
