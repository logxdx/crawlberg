#![cfg(feature = "browser")]

use crawlberg::{BrowserConfig, BrowserMode, CrawlConfig, create_engine, scrape};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn should_not_store_redirect_cookies_when_browser_cookies_are_disabled() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("location", "/landing")
                .append_header("set-cookie", "hop=first; Path=/"),
        )
        .mount(&site)
        .await;
    Mock::given(method("GET"))
        .and(path("/landing"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>Landing</body></html>", "text/html"))
        .mount(&site)
        .await;
    for cookies_enabled in [false, true] {
        let engine = create_engine(Some(CrawlConfig {
            cookies_enabled,
            respect_robots_txt: false,
            browser: BrowserConfig {
                mode: BrowserMode::Always,
                chrome_path: std::env::var_os("CRAWLBERG_TEST_CHROME").map(Into::into),
                ..BrowserConfig::default()
            },
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }))
        .expect("engine");
        let page = scrape(&engine, &site.uri()).await.expect("browser scrape");
        assert!(page.browser_used);
        assert_eq!(page.status_code, 200);
    }
    let requests = site.received_requests().await.expect("recorded requests");
    let cookies = requests
        .iter()
        .filter(|request| request.url.path() == "/landing")
        .map(|request| request.headers.get("cookie").map(|value| value.to_str().expect("cookie header")))
        .collect::<Vec<_>>();
    assert_eq!(cookies, [None, Some("hop=first")]);
}
