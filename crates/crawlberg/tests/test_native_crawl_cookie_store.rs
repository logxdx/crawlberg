#![cfg(feature = "browser-native")]

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig, crawl, create_engine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn should_share_native_cookies_only_between_pages_of_one_enabled_crawl() {
    for cookies_enabled in [false, true] {
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("set-cookie", "session=first; Path=/; HttpOnly")
                    .set_body_raw("<html><body><a href='/next'>Next</a></body></html>", "text/html"),
            )
            .mount(&site)
            .await;
        Mock::given(method("GET"))
            .and(path("/next"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<html><body>Next</body></html>", "text/html"))
            .mount(&site)
            .await;
        let engine = create_engine(Some(CrawlConfig {
            cookies_enabled,
            respect_robots_txt: false,
            max_concurrent: Some(1),
            browser: BrowserConfig {
                backend: BrowserBackend::Native,
                mode: BrowserMode::Always,
                ..BrowserConfig::default()
            },
            ..CrawlConfig::builder().allow_private_networks(true).build()
        }))
        .expect("engine");
        let result = crawl(&engine, &site.uri()).await.expect("crawl succeeds");
        assert_eq!(result.pages.len(), 2);
        let second = crawl(&engine, &format!("{}/next", site.uri())).await.expect("later crawl");
        assert_eq!(second.pages.len(), 1);
        let requests = site.received_requests().await.expect("requests");
        let sent = requests
            .iter()
            .filter(|request| request.url.path() == "/next")
            .map(|request| request.headers.get("cookie").map(|value| value.to_str().expect("cookie header")))
            .collect::<Vec<_>>();
        let expected = if cookies_enabled { vec![Some("session=first"), None] } else { vec![None, None] };
        assert_eq!(sent, expected, "cookies_enabled={cookies_enabled}");
    }
}
