//! `CrawlConfig::use_sitemap`: a crawl that also starts from the site's sitemaps.
//!
//! The sitemap's URLs join the seed page's links at depth 1, under the same scope, robots and
//! `max_pages` rules, and their own links are followed. Off, the crawl never asks for a sitemap.

use crawlberg::{CrawlConfig, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn page(mock: &MockServer, at: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("<html><body>{body}</body></html>"))
                .append_header("content-type", "text/html"),
        )
        .mount(mock)
        .await;
}

async fn text(mock: &MockServer, at: &str, body: String, content_type: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .append_header("content-type", content_type),
        )
        .mount(mock)
        .await;
}

/// `/` links only to `/a`; the sitemap also lists `/orphan` (which links to `/deep`), a
/// robots-disallowed `/blocked`, `/a` again, and a URL on another host.
async fn site(robots_names_sitemap: bool) -> MockServer {
    let mock = MockServer::start().await;
    let uri = mock.uri();
    page(&mock, "/", r#"<a href="/a">A</a>"#).await;
    page(&mock, "/a", "a").await;
    page(&mock, "/orphan", r#"<a href="/deep">deep</a>"#).await;
    page(&mock, "/deep", "deep").await;
    page(&mock, "/blocked", "blocked").await;
    let sitemap_line = if robots_names_sitemap {
        format!("Sitemap: {uri}/sitemap.xml\n")
    } else {
        String::new()
    };
    text(
        &mock,
        "/robots.txt",
        format!("User-agent: *\nDisallow: /blocked\n{sitemap_line}"),
        "text/plain",
    )
    .await;
    let urls = ["/orphan", "/blocked", "/a"]
        .iter()
        .map(|p| format!("<url><loc>{uri}{p}</loc></url>"))
        .collect::<String>();
    text(
        &mock,
        "/sitemap.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{urls}<url><loc>https://elsewhere.test/x</loc></url></urlset>"#
        ),
        "application/xml",
    )
    .await;
    mock
}

fn config(use_sitemap: bool, respect_robots_txt: bool) -> CrawlConfig {
    let base = CrawlConfig::builder()
        .allow_private_networks(true)
        .use_sitemap(use_sitemap)
        .build();
    CrawlConfig {
        max_depth: Some(2),
        respect_robots_txt,
        ..base
    }
}

fn paths(result: &crawlberg::CrawlResult, uri: &str) -> Vec<(String, usize)> {
    let mut out: Vec<_> = result
        .pages
        .iter()
        .map(|p| (p.url.trim_start_matches(uri).to_owned(), p.depth))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn sitemap_urls_join_the_crawl_at_depth_one_and_their_links_are_followed() {
    let mock = site(true).await;
    let uri = mock.uri();
    let handle = create_engine(Some(config(true, true))).expect("engine");

    let result = crawl(&handle, &uri).await.expect("crawl");

    let got = paths(&result, &uri);
    let as_paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
    assert!(as_paths.contains(&"/orphan"), "sitemap URL crawled: {got:?}");
    assert!(as_paths.contains(&"/deep"), "a sitemap page's link followed: {got:?}");
    assert!(!as_paths.contains(&"/blocked"), "robots.txt still applies: {got:?}");
    assert!(
        got.iter().all(|(p, _)| !p.contains("elsewhere")),
        "scope still applies: {got:?}"
    );
    assert_eq!(
        as_paths.iter().filter(|p| **p == "/a").count(),
        1,
        "deduplicated: {got:?}"
    );
    assert!(got.contains(&("/orphan".to_owned(), 1)), "queued at depth 1: {got:?}");
}

#[tokio::test]
async fn without_a_sitemap_directive_it_reads_sitemap_xml() {
    let mock = site(false).await;
    let uri = mock.uri();
    let handle = create_engine(Some(config(true, false))).expect("engine");

    let result = crawl(&handle, &uri).await.expect("crawl");

    let got = paths(&result, &uri);
    assert!(got.iter().any(|(p, _)| p == "/orphan"), "{got:?}");
}

#[tokio::test]
async fn off_by_default_the_crawl_never_asks_for_a_sitemap() {
    let mock = site(true).await;
    let uri = mock.uri();
    let handle = create_engine(Some(config(false, true))).expect("engine");

    let result = crawl(&handle, &uri).await.expect("crawl");

    let got = paths(&result, &uri);
    assert!(got.iter().all(|(p, _)| p != "/orphan"), "{got:?}");
    let requests = mock.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().all(|r| r.url.path() != "/sitemap.xml"),
        "sitemap.xml was requested"
    );
}
