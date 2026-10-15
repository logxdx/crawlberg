use chromiumoxide::cdp::browser_protocol::network::{CookiePartitionKey, CookiePriority, CookieSameSite};

use super::*;

#[test]
fn should_exclude_unchanged_and_foreign_cookie_slots_from_result_delta() {
    let unchanged = left("old", "1");
    let changed = left("changed", "after");
    let before = vec![unchanged.clone(), left("changed", "before")];
    let foreign = browser_cookie(cookie("other.example", true, -1.0, None), 100.0).expect("cookie");
    let suffix = browser_cookie(cookie(".ample.com", true, -1.0, None), 100.0).expect("cookie");
    let after = vec![unchanged, changed, foreign, suffix];
    let delta = result_cookie_delta(&before, &after, "https://example.com/", "https://example.com/next");
    assert_eq!(delta.len(), 1);
    assert_eq!(
        (&delta[0].name, &delta[0].value),
        (&"changed".to_owned(), &"after".to_owned())
    );
}

#[test]
fn should_omit_same_value_rewrites_and_deleted_cookie_slots() {
    let before = vec![left("same", "1"), left("deleted", "1")];
    let after = vec![left("same", "1")];
    assert_eq!(
        result_cookie_delta(&before, &after, "https://example.com/", "https://example.com/").len(),
        0
    );
}

#[test]
fn should_match_domain_cookie_boundaries_but_not_host_only_subdomains() {
    let domain = browser_cookie(cookie(".example.com", true, -1.0, None), 100.0).expect("domain cookie");
    let host_only = browser_cookie(cookie("example.com", true, -1.0, None), 100.0).expect("host cookie");
    let delta = result_cookie_delta(
        &[],
        &[domain, host_only],
        "https://sub.example.com/",
        "https://sub.example.com/",
    );
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].domain.as_deref(), Some(".example.com"));
}

#[test]
fn a_host_only_result_cookie_has_its_host_as_domain() {
    let host_only = browser_cookie(cookie("example.com", true, -1.0, None), 100.0).expect("host cookie");
    let delta = result_cookie_delta(&[], &[host_only], "https://example.com/", "https://example.com/");
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].domain.as_deref(), Some("example.com"));
}

#[test]
fn a_carried_cookie_keeps_all_replayable_cdp_metadata() {
    let partition = CookiePartitionKey::new("https://top.example", true);
    let cookie = cookie(".example.com", false, 200.0, Some(partition.clone()));

    let carried = browser_cookie(cookie, 100.0).expect("the unexpired cookie must be carried");
    let params = carried.params;

    assert_eq!(params.url, None);
    assert_eq!(params.domain.as_deref(), Some(".example.com"));
    assert_eq!(params.path.as_deref(), Some("/private"));
    assert_eq!(params.secure, Some(true));
    assert_eq!(params.http_only, Some(true));
    assert_eq!(params.same_site, Some(CookieSameSite::Strict));
    assert_eq!(params.expires.as_ref().map(TimeSinceEpoch::inner), Some(&200.0));
    assert_eq!(params.priority, Some(CookiePriority::High));
    assert_eq!(params.source_scheme, Some(CookieSourceScheme::Secure));
    assert_eq!(params.source_port, Some(8443));
    assert_eq!(params.partition_key, Some(partition));
}

#[test]
fn a_host_only_cookie_stays_host_only_when_carried() {
    let carried =
        browser_cookie(cookie("example.com", true, -1.0, None), 100.0).expect("the session cookie must be carried");

    assert_eq!(carried.params.domain, None);
    assert_eq!(carried.params.url.as_deref(), Some("https://example.com:8443/private"));
    assert_eq!(carried.params.expires, None);
}

#[test]
fn an_expired_cookie_is_not_carried() {
    assert!(
        browser_cookie(cookie("example.com", false, 99.0, None), 100.0).is_none(),
        "a cookie that expired before capture must not be resurrected"
    );
}

#[test]
fn an_opaque_partition_cookie_is_not_replayed_without_its_key() {
    let mut opaque = cookie("example.com", true, -1.0, None);
    opaque.partition_key_opaque = Some(true);

    assert!(
        browser_cookie(opaque, 100.0).is_none(),
        "an opaque partition must not be replayed as an unpartitioned cookie"
    );
}

/// The cookie `name` of the host `example.com` with `value`, as a page leaves it.
fn left(name: &str, value: &str) -> BrowserCookie {
    let mut cookie = cookie("example.com", true, -1.0, None);
    cookie.name = name.to_owned();
    cookie.value = value.to_owned();
    browser_cookie(cookie, 100.0).expect("a session cookie is carried")
}

/// The crawl's cookies as `name=value` pairs in name order.
fn held(crawl: &CrawlCookies) -> Vec<String> {
    let mut pairs: Vec<String> = crawl
        .given()
        .iter()
        .filter(|cookie| cookie.params.expires.is_none())
        .map(|cookie| format!("{}={}", cookie.params.name, cookie.params.value))
        .collect();
    pairs.sort();
    pairs
}

#[test]
fn a_later_page_is_given_the_cookie_an_earlier_page_left() {
    let crawl = CrawlCookies::default();
    assert!(crawl.given().is_empty(), "a crawl starts with no cookie");

    crawl.absorb(&[], &[left("session", "1")]);

    assert_eq!(held(&crawl), ["session=1"]);
}

#[test]
fn a_page_that_changes_a_cookie_changes_it_for_the_crawl() {
    let crawl = CrawlCookies::default();
    crawl.absorb(&[], &[left("session", "1"), left("other", "kept")]);

    let given = crawl.given();
    crawl.absorb(&given, &[left("session", "2"), left("other", "kept")]);

    assert_eq!(held(&crawl), ["other=kept", "session=2"]);
}

#[test]
fn a_page_that_deletes_a_cookie_deletes_it_for_the_crawl() {
    let crawl = CrawlCookies::default();
    crawl.absorb(&[], &[left("session", "1"), left("other", "kept")]);

    let given = crawl.given();
    crawl.absorb(&given, &[left("other", "kept")]);

    assert_eq!(held(&crawl), ["other=kept"]);
}

/// Two pages start with the same cookies. The first to end changes one; the second ends
/// with the copy it was given, which is older than the crawl's by then.
#[test]
fn a_page_that_ends_with_an_older_copy_does_not_undo_a_newer_change() {
    let crawl = CrawlCookies::default();
    crawl.absorb(&[], &[left("session", "1")]);
    let first = crawl.given();
    let second = crawl.given();

    crawl.absorb(&first, &[left("session", "2"), left("added", "1")]);
    crawl.absorb(&second, &[left("session", "1")]);

    assert_eq!(held(&crawl), ["added=1", "session=2"]);
}

/// Two pages start with the same cookie. One changes it and one deletes it; the deletion
/// applies to the cookie the page was given, not to the one written since.
#[test]
fn a_deletion_does_not_remove_the_value_another_page_wrote_since() {
    let crawl = CrawlCookies::default();
    crawl.absorb(&[], &[left("session", "1")]);
    let writer = crawl.given();
    let deleter = crawl.given();

    crawl.absorb(&writer, &[left("session", "2")]);
    crawl.absorb(&deleter, &[]);

    assert_eq!(held(&crawl), ["session=2"]);
}

#[test]
fn should_bridge_only_the_last_effective_domain_attribute() {
    let url = url::Url::parse("https://a.example.org/folder/page").expect("test URL");
    for raw in [
        "session=1; Domain=evil.test; Domain=example.org",
        "session=1; Domain = evil.test; DOMAIN = example.org; Domain=",
    ] {
        let cookie = browser_cookie_from_http(&url, raw).expect("last domain matches source");
        assert_eq!(cookie.params.domain.as_deref(), Some(".example.org"));
        assert_eq!(cookie.params.path.as_deref(), Some("/folder"));
        assert_eq!(cookie.params.value, "1");
    }
    assert!(browser_cookie_from_http(&url, "session=1; Domain=example.org; Domain=evil.test").is_none());
}

#[test]
fn should_preserve_secure_cookie_when_an_insecure_response_updates_the_live_http_store() {
    use reqwest::cookie::CookieStore as _;
    let crawl = CrawlCookies::default();
    let secure = left("session", "original");
    crawl.absorb(&[], &[secure]);
    let store = crate::net::cookie::PolicyCookieStore::default();
    crawl.sync_http_store(&[], &store);
    let https = url::Url::parse("https://example.com/private/page").expect("secure URL");
    let http = url::Url::parse("http://example.com/private/page").expect("insecure URL");
    assert_eq!(
        store
            .cookies(&https)
            .and_then(|header| header.to_str().ok().map(str::to_owned)),
        Some("session=original".to_owned())
    );
    for raw in ["session=replaced; Path=/private", "session=; Path=/private; Max-Age=0"] {
        let headers = [
            reqwest::header::HeaderValue::from_str(raw).expect("response cookie"),
            reqwest::header::HeaderValue::from_static("unrelated=latest; Path=/private"),
        ];
        store.set_cookies(&mut headers.iter(), &http);
        crawl.absorb_http_response(http.as_str(), &set_cookie(&[raw, "unrelated=latest; Path=/private"]));
        let header = store
            .cookies(&https)
            .expect("the live HTTP jar keeps the protected cookie");
        let mut cookies = header.to_str().expect("cookie header").split("; ").collect::<Vec<_>>();
        cookies.sort_unstable();
        assert_eq!(cookies, ["session=original", "unrelated=latest"], "{raw}");
        assert_eq!(
            store
                .cookies(&http)
                .and_then(|header| header.to_str().ok().map(str::to_owned)),
            Some("unrelated=latest".to_owned())
        );
    }
}

#[test]
fn should_export_browser_cookies_without_widening_partitioned_cookies() {
    use reqwest::cookie::CookieStore as _;
    let crawl = CrawlCookies::default();
    let plain = left("plain", "1");
    let mut partitioned = left("partitioned", "secret");
    partitioned.params.partition_key = Some(CookiePartitionKey::new("https://top.example", false));
    crawl.absorb(&[], &[plain.clone(), partitioned]);
    let store = crate::net::cookie::PolicyCookieStore::default();
    crawl.sync_http_store(&[], &store);
    let url = url::Url::parse("https://example.com/private").expect("test URL");
    assert_eq!(
        store.cookies(&url).as_ref().and_then(|value| value.to_str().ok()),
        Some("plain=1")
    );
    let given = crawl.given();
    crawl.absorb(&given, &[]);
    crawl.sync_http_store(&given, &store);
    assert_eq!(store.cookies(&url), None);
}

#[test]
fn should_send_http_cookie_replacements_to_browser_with_storage_attributes() {
    let crawl = CrawlCookies::default();
    crawl.absorb_http_response(
        "https://example.com/private/action",
        &set_cookie(&["session=2; Path=/private; HttpOnly; Secure; SameSite=Strict; Max-Age=3600"]),
    );
    let given = crawl.given();
    assert_eq!(given.len(), 1);
    let params = &given[0].params;
    assert_eq!(params.value, "2");
    assert_eq!(params.path.as_deref(), Some("/private"));
    assert_eq!(params.secure, Some(true));
    assert_eq!(params.http_only, Some(true));
    assert_eq!(params.same_site.as_ref().map(AsRef::as_ref), Some("Strict"));
    assert!(params.expires.as_ref().is_some_and(|expiry| *expiry.inner() > 0.0));
    crawl.absorb_http_response(
        "https://example.com/private/action",
        &set_cookie(&["session=; Path=/private; Max-Age=0"]),
    );
    let given = crawl.given();
    assert_eq!(given.len(), 1);
    assert_eq!(given[0].params.value, "");
    assert_eq!(given[0].params.expires, Some(TimeSinceEpoch::new(0.0)));
}

fn set_cookie(values: &[&str]) -> HashMap<String, Vec<String>> {
    let values = values.iter().map(|value| (*value).to_owned()).collect();
    HashMap::from([("set-cookie".to_owned(), values)])
}

/// A deletion and a new value both leave the crawl without the cookie: the old copy is
/// what must not reach a later page.
#[test]
fn a_cookie_set_without_the_browser_leaves_the_crawl() {
    let crawl = CrawlCookies::default();
    crawl.absorb(&[], &[left("gone", "1"), left("new", "1"), left("other", "kept")]);

    crawl.forget_set_without_browser(
        "https://example.com/private/action",
        &set_cookie(&["gone=; Max-Age=0", "new=2; Path=/private"]),
    );

    assert_eq!(held(&crawl), ["other=kept"]);
}

#[test]
fn an_http_cookie_change_keeps_other_paths_domains_partitions_and_secure_cookies() {
    let change = invalidated_cookie(
        &url::Url::parse("https://example.com/private/action").unwrap(),
        "session=; Max-Age=0",
    )
    .unwrap();
    let current = left("session", "1");
    assert!(invalidates(&change, &current));
    let mut other_path = current.clone();
    other_path.params.path = Some("/other".to_owned());
    let mut other_host = current.clone();
    other_host.params.url = Some("https://sub.example.com/private".to_owned());
    let mut partitioned = current.clone();
    partitioned.params.partition_key = Some(CookiePartitionKey::new("https://top.example", false));
    for unrelated in [other_path, other_host, partitioned] {
        assert!(!invalidates(&change, &unrelated));
    }
    let insecure = invalidated_cookie(
        &url::Url::parse("http://example.com/private/action").unwrap(),
        "session=; Max-Age=0",
    )
    .unwrap();
    assert!(!invalidates(&insecure, &current));
    assert!(
        invalidated_cookie(
            &url::Url::parse("https://example.com/").unwrap(),
            "session=; Domain=other.com"
        )
        .is_none()
    );
    assert!(
        invalidated_cookie(
            &url::Url::parse("https://example.com/").unwrap(),
            "session=; Domain=com"
        )
        .is_none()
    );
}

#[test]
fn partitioned_and_rejected_secure_headers_leave_no_unpartitioned_invalidation() {
    let secure = url::Url::parse("https://example.com/").unwrap();
    let insecure = url::Url::parse("http://example.com/").unwrap();
    for raw in [
        "session=; Path=/; Secure; Partitioned",
        "session=; Path=/; Secure; pArTiTiOnEd=ignored",
        "__Host-session=; Path=/",
        "__Host-session=; Path=/other; Secure",
        "__Host-session=; Path=/; Secure; Domain=example.com",
        "__Secure-session=; Path=/",
        "__Host-session=; Secure",
        "__Http-session=; Secure",
        "__Host-Http-session=; Secure; Path=/",
    ] {
        assert!(invalidated_cookie(&secure, raw).is_none(), "reject {raw}");
    }
    assert!(invalidated_cookie(&insecure, "session=; Path=/; Secure").is_none());
    assert!(invalidated_cookie(&secure, "__Host-session=; Path=/; Secure").is_some());
    assert!(invalidated_cookie(&secure, "__Secure-session=; Path=/; Secure").is_some());
    assert!(
        invalidated_cookie(
            &url::Url::parse("http://127.0.0.1/").unwrap(),
            "session=; Path=/; Secure"
        )
        .is_some()
    );
}

#[test]
fn an_http_change_forgets_only_its_exact_slot_and_a_browser_write_clears_it() {
    let crawl = CrawlCookies::default();
    let root = left("session", "old");
    let mut other = root.clone();
    other.params.path = Some("/elsewhere".to_owned());
    crawl.absorb(&[], &[root.clone(), other.clone()]);
    crawl.forget_set_without_browser(
        "https://example.com/private/action",
        &set_cookie(&["session=; Max-Age=0"]),
    );
    let given = crawl.given();
    assert_eq!(given.iter().filter(|cookie| is_invalidated(cookie)).count(), 1);
    assert_eq!(given.iter().filter(|cookie| !is_invalidated(cookie)).count(), 1);
    crawl.absorb(&given, &[left("session", "new"), other]);
    assert_eq!(crawl.given().iter().filter(|cookie| is_invalidated(cookie)).count(), 0);
    assert_eq!(held(&crawl), ["session=new", "session=old"]);
    crawl.forget_set_without_browser(
        "http://example.com/private/action",
        &set_cookie(&["session=; Max-Age=0"]),
    );
    assert_eq!(held(&crawl), ["session=new", "session=old"]);
}

#[test]
fn a_response_for_another_host_or_with_no_cookie_changes_nothing() {
    let crawl = CrawlCookies::default();
    crawl.absorb(&[], &[left("session", "1")]);

    crawl.forget_set_without_browser("https://notexample.com/private/action", &set_cookie(&["session=2"]));
    crawl.forget_set_without_browser("https://other.org/private/action", &set_cookie(&["session=2"]));
    crawl.forget_set_without_browser(
        "https://example.com/private/action",
        &set_cookie(&["no pair", "different=1"]),
    );
    crawl.forget_set_without_browser("https://example.com/private/action", &HashMap::new());

    assert_eq!(held(&crawl), ["session=1"]);
}

#[test]
fn two_pages_that_set_the_same_cookie_leave_one_cookie_with_the_last_value() {
    let crawl = CrawlCookies::default();
    let first = crawl.given();
    let second = crawl.given();

    crawl.absorb(&first, &[left("session", "first")]);
    crawl.absorb(&second, &[left("session", "second")]);

    assert_eq!(held(&crawl), ["session=second"]);
}

/// Chrome keeps these as separate cookies, so the crawl does too: the same name on another
/// host, as a domain cookie, on another path, and in another partition.
#[test]
fn cookies_that_chrome_keeps_apart_stay_apart() {
    let host_only = |domain: &str| {
        let mut cookie = cookie(domain, true, -1.0, None);
        cookie.value = domain.to_owned();
        browser_cookie(cookie, 100.0).expect("a session cookie is carried")
    };
    let mut other_path = cookie("example.com", true, -1.0, None);
    other_path.path = "/public".to_owned();
    let partitioned = cookie(
        "example.com",
        true,
        -1.0,
        Some(CookiePartitionKey::new("https://top.example", false)),
    );
    let apart = [
        host_only("example.com"),
        host_only("other.example.com"),
        host_only(".example.com"),
        browser_cookie(other_path, 100.0).expect("a session cookie is carried"),
        browser_cookie(partitioned, 100.0).expect("a session cookie is carried"),
    ];
    let crawl = CrawlCookies::default();

    crawl.absorb(&[], &apart);
    assert_eq!(crawl.given().len(), apart.len());

    // ~keep The same cookie from another port of the host is one cookie in Chrome.
    let mut other_port = cookie("example.com", true, -1.0, None);
    other_port.source_port = 9443;
    other_port.value = "replaced".to_owned();
    crawl.absorb(
        &[],
        &[browser_cookie(other_port, 100.0).expect("a session cookie is carried")],
    );
    assert_eq!(crawl.given().len(), apart.len());
    assert!(crawl.given().iter().any(|cookie| cookie.params.value == "replaced"));
}

/// Chrome sends cookies of one path length in the order they were made, and a page's script
/// reads them in that order.
#[test]
fn a_page_is_given_the_cookies_in_the_order_the_crawl_got_them() {
    let names: Vec<String> = (0..12).map(|n| format!("cookie{n}")).collect();
    let crawl = CrawlCookies::default();
    let first: Vec<BrowserCookie> = names[..8].iter().map(|name| left(name, "1")).collect();
    crawl.absorb(&[], &first);
    let given = crawl.given();
    let mut second = given.clone();
    second.extend(names[8..].iter().map(|name| left(name, "1")));
    crawl.absorb(&given, &second);

    let order: Vec<String> = crawl.given().into_iter().map(|cookie| cookie.params.name).collect();
    assert_eq!(order, names);

    // ~keep Chrome makes a cookie anew when its value changes, so it moves behind the others.
    let given = crawl.given();
    let mut changed = given.clone();
    let moved = changed.remove(3);
    changed.push(left(&moved.params.name, "2"));
    crawl.absorb(&given, &changed);

    let order: Vec<String> = crawl.given().into_iter().map(|cookie| cookie.params.name).collect();
    let mut expected = names.clone();
    let moved = expected.remove(3);
    expected.push(moved);
    assert_eq!(order, expected);
}

/// What one page of a pair does to the cookie `session`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Nothing,
    Set(&'static str),
    Delete,
}

impl Act {
    fn on(self, value: Option<&'static str>) -> Option<&'static str> {
        match self {
            Self::Nothing => value,
            Self::Set(new) => Some(new),
            Self::Delete => None,
        }
    }
}

/// Run two pages against one crawl. `order` lists the page of each event: the first event
/// of a page is its start, the second its end. Returns the crawl's `session` value.
fn two_pages(initial: Option<&'static str>, acts: [Act; 2], order: [usize; 4]) -> Option<String> {
    let crawl = CrawlCookies::default();
    let mut start = vec![left("other", "kept")];
    start.extend(initial.map(|value| left("session", value)));
    crawl.absorb(&[], &start);

    let mut given: [Option<Vec<BrowserCookie>>; 2] = [None, None];
    for page in order {
        let Some(given) = given[page].take() else {
            given[page] = Some(crawl.given());
            continue;
        };
        let mut left_by_page: Vec<BrowserCookie> = given
            .iter()
            .filter(|cookie| cookie.params.name != "session" || acts[page] == Act::Nothing)
            .cloned()
            .collect();
        if let Act::Set(value) = acts[page] {
            left_by_page.push(left("session", value));
        }
        crawl.absorb(&given, &left_by_page);
    }

    let held = held(&crawl);
    assert!(
        held.contains(&"other=kept".to_owned()),
        "an untouched cookie stays: {held:?}"
    );
    assert!(held.len() <= 2, "one cookie for one name: {held:?}");
    held.iter()
        .find_map(|pair| pair.strip_prefix("session="))
        .map(str::to_owned)
}

/// Every order of the starts and ends of two pages, for every pair of actions, with and
/// without the cookie at the start.
#[test]
fn two_pages_in_every_order_leave_what_one_cookie_store_would() {
    const ORDERS: [[usize; 4]; 6] = [
        [0, 0, 1, 1],
        [1, 1, 0, 0],
        [0, 1, 0, 1],
        [0, 1, 1, 0],
        [1, 0, 1, 0],
        [1, 0, 0, 1],
    ];
    let actions = |first: &'static str| [Act::Nothing, Act::Set(first), Act::Delete];
    let mut cases = 0;
    for initial in [None, Some("start")] {
        for first in actions("first") {
            for second in actions("second") {
                for order in ORDERS {
                    let acts = [first, second];
                    let last_to_end = order[3];
                    let first_to_end = 1 - last_to_end;
                    let one_after_the_other = order[0] == order[1];
                    let expected = if one_after_the_other {
                        // ~keep The second page starts with what the first one left.
                        acts[last_to_end].on(acts[first_to_end].on(initial))
                    } else {
                        match (acts[first_to_end], acts[last_to_end]) {
                            // ~keep A page that did nothing changes nothing, whenever it ends.
                            (act, Act::Nothing) | (Act::Nothing, act) => act.on(initial),
                            // ~keep Of two values the one of the page that ends last stays.
                            (Act::Set(_) | Act::Delete, Act::Set(value)) => Some(value),
                            // ~keep A deletion does not remove a value another page wrote.
                            (Act::Set(value), Act::Delete) => Some(value),
                            (Act::Delete, Act::Delete) => None,
                        }
                    };
                    assert_eq!(
                        two_pages(initial, acts, order).as_deref(),
                        expected,
                        "initial {initial:?}, pages {acts:?}, events {order:?}"
                    );
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 108);
}

fn cookie(domain: &str, session: bool, expires: f64, partition_key: Option<CookiePartitionKey>) -> Cookie {
    Cookie {
        name: "session".to_owned(),
        value: "secret".to_owned(),
        domain: domain.to_owned(),
        path: "/private".to_owned(),
        expires,
        size: 13,
        http_only: true,
        secure: true,
        session,
        same_site: Some(CookieSameSite::Strict),
        priority: CookiePriority::High,
        source_scheme: CookieSourceScheme::Secure,
        source_port: 8443,
        partition_key,
        partition_key_opaque: Some(false),
    }
}
