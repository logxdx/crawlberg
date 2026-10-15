use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use chromiumoxide::cdp::browser_protocol::network::{Cookie, CookieParam, CookieSourceScheme, TimeSinceEpoch};
use chromiumoxide::cdp::browser_protocol::storage::{
    GetCookiesParams as StorageGetCookiesParams, SetCookiesParams as StorageSetCookiesParams,
};

use crate::error::CrawlError;
use crate::ssrf_intercept::Watch;
use crate::types::BrowserCookie;

/// The cookies the Chrome pages of one crawl have in common.
///
/// ~keep Each page has a browser context of its own, which the SSRF check needs: the context is
/// ~keep what ties a popup to its page's policy, and its disposal is what ends the page's pending
/// ~keep requests (xberg-io/crawlberg#506). So the pages cannot share Chrome's store. Each page
/// ~keep starts with a copy of these cookies in its own store, and what it changed is recorded
/// ~keep here when it ends. Chrome receives the full cookie attributes; HTTP requests use a
/// ~keep validated store for the same crawl. Partitioned cookies stay browser-only.
#[derive(Default)]
pub(crate) struct CrawlCookies {
    held: Mutex<Held>,
}

/// The cookies of a crawl, each with its place in the order Chrome made them.
///
/// ~keep Chrome sends the cookies of one path length oldest first, and a script reads them in
/// ~keep that order. A page's store makes the cookies it is given in the order of the list, so
/// ~keep the list keeps the order of the crawl's pages' stores.
#[derive(Default)]
struct Held {
    cookies: HashMap<Slot, (u64, BrowserCookie)>,
    made: u64,
    invalidated: HashMap<Slot, BrowserCookie>,
}

impl Held {
    /// Record `cookie` as a page left it. Chrome makes a cookie anew when its value changes,
    /// and keeps its age when only an attribute does.
    fn put(&mut self, slot: Slot, cookie: BrowserCookie) {
        let kept_age = self
            .cookies
            .get(&slot)
            .filter(|(_, held)| held.params.value == cookie.params.value)
            .map(|(age, _)| *age);
        let age = kept_age.unwrap_or_else(|| {
            self.made += 1;
            self.made
        });
        self.cookies.insert(slot, (age, cookie));
    }
}

/// What makes two cookies one cookie in Chrome's store. A cookie for one host only has that
/// host as its `site`; a cookie with a `Domain` has the domain with its leading dot.
#[derive(PartialEq, Eq, Hash)]
struct Slot {
    name: String,
    site: Option<String>,
    path: Option<String>,
    partition: Option<(String, bool)>,
}

impl Slot {
    fn of(cookie: &BrowserCookie) -> Self {
        let cookie = &cookie.params;
        let host = || {
            let url = url::Url::parse(cookie.url.as_deref()?).ok()?;
            url.host_str().map(str::to_owned)
        };
        Self {
            name: cookie.name.clone(),
            site: cookie.domain.clone().or_else(host),
            path: cookie.path.clone(),
            partition: cookie
                .partition_key
                .as_ref()
                .map(|key| (key.top_level_site.clone(), key.has_cross_site_ancestor)),
        }
    }
}

impl CrawlCookies {
    /// The cookies a page that starts now is given.
    pub(crate) fn given(&self) -> Vec<BrowserCookie> {
        let held = self.lock();
        let mut cookies: Vec<(u64, BrowserCookie)> = held.cookies.values().cloned().collect();
        cookies.sort_unstable_by_key(|(age, _)| *age);
        let mut cookies: Vec<_> = cookies.into_iter().map(|(_, cookie)| cookie).collect();
        cookies.extend(held.invalidated.values().cloned());
        cookies
    }

    /// Record what a page did to the cookies it was `given`, from the cookies it `left`.
    ///
    /// ~keep Only the page's own changes are recorded. Pages of one crawl load at the same time,
    /// ~keep so a page ends with a copy that can be older than the crawl's: a cookie it left as
    /// ~keep it was given says nothing, and a cookie it no longer has is removed only while the
    /// ~keep crawl still holds the one the page was given.
    pub(crate) fn absorb(&self, given: &[BrowserCookie], left: &[BrowserCookie]) {
        let mut given: HashMap<Slot, &BrowserCookie> = given.iter().map(|cookie| (Slot::of(cookie), cookie)).collect();
        let mut held = self.lock();
        // ~keep `left` is in the order of the page's store: new cookies keep that order.
        for after in left {
            let slot = Slot::of(after);
            if given.remove(&slot).is_none_or(|before| before.params != after.params) {
                held.invalidated.remove(&slot);
                held.put(slot, after.clone());
            }
        }
        for (slot, before) in given {
            if held
                .cookies
                .get(&slot)
                .is_some_and(|(_, held)| held.params == before.params)
            {
                held.cookies.remove(&slot);
            }
        }
    }

    /// Export current browser cookies and page deletions to the crawl's HTTP jar.
    pub(crate) fn sync_http_store(&self, given: &[BrowserCookie], store: &crate::net::cookie::PolicyCookieStore) {
        let held = self.lock();
        for (_, cookie) in held.cookies.values() {
            store_browser_cookie(store, cookie, false);
        }
        for before in given {
            if !held.cookies.contains_key(&Slot::of(before)) {
                store_browser_cookie(store, before, true);
            }
        }
        for cookie in held.invalidated.values() {
            store_browser_cookie(store, cookie, true);
        }
    }

    /// Store HTTP response cookies for the next browser page, including deletion tombstones.
    pub(crate) fn absorb_http_response(&self, response_url: &str, headers: &HashMap<String, Vec<String>>) {
        self.forget_set_without_browser(response_url, headers);
        let Ok(url) = url::Url::parse(response_url) else { return };
        let Some(set) = headers.get("set-cookie") else { return };
        let mut held = self.lock();
        for raw in set {
            let Some(cookie) = browser_cookie_from_http(&url, raw) else {
                continue;
            };
            let slot = Slot::of(&cookie);
            if held
                .cookies
                .get(&slot)
                .is_some_and(|(_, current)| !invalidates(&cookie, current))
            {
                continue;
            }
            held.invalidated.remove(&slot);
            held.put(slot, cookie);
        }
    }

    /// Invalidate exactly the cookie slots changed by an HTTP response. ~keep
    pub(crate) fn forget_set_without_browser(&self, response_url: &str, headers: &HashMap<String, Vec<String>>) {
        let Ok(url) = url::Url::parse(response_url) else { return };
        let Some(set) = headers.get("set-cookie") else { return };
        let mut held = self.lock();
        for raw in set {
            let Some(cookie) = invalidated_cookie(&url, raw) else {
                continue;
            };
            let slot = Slot::of(&cookie);
            if held
                .cookies
                .get(&slot)
                .is_some_and(|(_, current)| !invalidates(&cookie, current))
            {
                continue;
            }
            held.cookies.remove(&slot);
            held.invalidated.insert(slot, cookie);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn store_browser_cookie(store: &crate::net::cookie::PolicyCookieStore, cookie: &BrowserCookie, deleted: bool) {
    use reqwest::cookie::CookieStore as _;
    let params = &cookie.params;
    // ~keep HTTP has no top-level partition context; never widen a partitioned cookie.
    if params.partition_key.is_some() {
        return;
    }
    let domain = params.domain.as_deref().unwrap_or_default();
    let source = params.url.clone().unwrap_or_else(|| {
        format!(
            "{}://{}/",
            if params.secure == Some(true) { "https" } else { "http" },
            domain.trim_start_matches('.')
        )
    });
    let Ok(url) = url::Url::parse(&source) else { return };
    let mut raw = format!(
        "{}={}; Path={}",
        params.name,
        params.value,
        params.path.as_deref().unwrap_or("/")
    );
    if domain.starts_with('.') {
        raw.push_str(&format!("; Domain={domain}"));
    }
    if params.secure == Some(true) {
        raw.push_str("; Secure");
    }
    if params.http_only == Some(true) {
        raw.push_str("; HttpOnly");
    }
    if let Some(site) = &params.same_site {
        raw.push_str(&format!("; SameSite={}", site.as_ref()));
    }
    if deleted {
        raw.push_str("; Max-Age=0");
    } else if let Some(expiry) = &params.expires {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_secs_f64())
            .unwrap_or(0.0);
        raw.push_str(&format!("; Max-Age={:.0}", (*expiry.inner() - now).floor().max(0.0)));
    }
    if let Ok(header) = reqwest::header::HeaderValue::from_str(&raw) {
        store.set_cookies(&mut std::iter::once(&header), &url);
    }
}

fn browser_cookie_from_http(url: &url::Url, raw: &str) -> Option<BrowserCookie> {
    let parsed = cookie_store::Cookie::parse(raw, url).ok()?;
    if parsed.expires.is_expired() {
        return None;
    }
    let mut cookie = invalidated_cookie(url, raw)?;
    cookie.params.value = parsed.value().to_owned();
    cookie.params.path = Some(String::from(&parsed.path));
    cookie.params.expires = match &parsed.expires {
        cookie_store::CookieExpiration::AtUtc(expiry) => Some(TimeSinceEpoch::new(expiry.unix_timestamp() as f64)),
        cookie_store::CookieExpiration::SessionEnd => None,
    };
    cookie.params.secure = parsed.secure();
    cookie.params.http_only = parsed.http_only();
    cookie.params.same_site = parsed
        .same_site()
        .and_then(|site| serde_json::from_value(serde_json::Value::String(site.to_string())).ok());
    Some(cookie)
}

/// ~keep An expired parameter removes a retained context's old copy; ordinary crawl cookies
/// cannot express deletions because their absence does not mean a session cookie was deleted.
fn invalidated_cookie(url: &url::Url, raw: &str) -> Option<BrowserCookie> {
    let parsed = cookie_store::Cookie::parse(raw, url).ok()?;
    let name = parsed.name();
    if name.is_empty() || !accepted_cookie_change(url, name, raw) {
        return None;
    }
    let mut params = CookieParam::new(name, "");
    params.url = Some(url.to_string());
    params.path = Some(String::from(&parsed.path));
    params.expires = Some(TimeSinceEpoch::new(0.0));
    if let cookie_store::CookieDomain::Suffix(domain) = &parsed.domain {
        params.domain = crate::net::cookie::validate_cookie_domain(url.host_str()?, domain)
            .ok()?
            .map(|domain| format!(".{domain}"));
    }
    Some(BrowserCookie { params })
}

/// ~keep HTTP has no trusted Chrome partition key, and rejected cookie headers cannot delete
/// a retained cookie that Chrome itself would keep.
fn accepted_cookie_change(url: &url::Url, name: &str, raw: &str) -> bool {
    let attrs: Vec<_> = raw
        .split(';')
        .skip(1)
        .map(|part| {
            let (key, value) = part.trim().split_once('=').unwrap_or((part.trim(), ""));
            (key.trim(), value.trim())
        })
        .collect();
    let has = |name: &str| attrs.iter().any(|(key, _)| key.eq_ignore_ascii_case(name));
    if has("partitioned") || (has("secure") && !secure_cookie_source(url)) {
        return false;
    }
    if (name.starts_with("__Secure-") || name.starts_with("__Host-") || name.starts_with("__Http-")) && !has("secure") {
        return false;
    }
    if name.starts_with("__Host-")
        && (has("domain")
            || attrs
                .iter()
                .rev()
                .find(|(key, _)| key.eq_ignore_ascii_case("path"))
                .map(|(_, path)| *path)
                != Some("/"))
    {
        return false;
    }
    !(name.starts_with("__Http-") || name.starts_with("__Host-Http-")) || has("httponly")
}

fn secure_cookie_source(url: &url::Url) -> bool {
    url.scheme() == "https"
        || match url.host() {
            Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        }
}

fn is_invalidated(cookie: &BrowserCookie) -> bool {
    cookie
        .params
        .expires
        .as_ref()
        .is_some_and(|expires| *expires.inner() == 0.0)
}

fn invalidates(change: &BrowserCookie, current: &BrowserCookie) -> bool {
    let source = change.params.url.as_deref().and_then(|url| url::Url::parse(url).ok());
    let secure_source = source.as_ref().is_some_and(secure_cookie_source);
    Slot::of(change) == Slot::of(current) && (current.params.secure != Some(true) || secure_source)
}

/// Seed the page with cookies carried over from a previous fetch.
///
/// ~keep Replay is one browser-scoped operation against the watched page's exact context. A
/// ~keep failure aborts the hop because continuing would silently turn an authenticated chain
/// ~keep into an unauthenticated request.
pub(super) async fn apply_prior_cookies(
    watch: &Watch,
    prior_cookies: Option<&[BrowserCookie]>,
) -> Result<(), CrawlError> {
    let Some(cookies) = prior_cookies else {
        return Ok(());
    };
    if cookies.is_empty() {
        return Ok(());
    }
    let mut replay = Vec::new();
    let current = if cookies.iter().any(is_invalidated) {
        page_cookies(watch).await?
    } else {
        Vec::new()
    };
    for cookie in cookies {
        if is_invalidated(cookie) {
            for held in current.iter().filter(|held| invalidates(cookie, held)) {
                let mut expired = held.params.clone();
                expired.expires = Some(TimeSinceEpoch::new(0.0));
                replay.push(expired);
            }
        } else {
            replay.push(cookie.params.clone());
        }
    }
    let (browser, browser_context_id) = watch.cookie_store()?;
    browser
        .execute(StorageSetCookiesParams {
            cookies: replay,
            browser_context_id,
        })
        .await
        .map_err(|error| CrawlError::browser_error(format!("failed to seed browser cookies: {error}")))?;
    Ok(())
}

/// Read the whole browser context's jar before its page is released. A URL-scoped
/// `Network.getCookies` would omit a cookie whose `Path` matches only the next hop. ~keep
pub(super) async fn page_cookies(watch: &Watch) -> Result<Vec<BrowserCookie>, CrawlError> {
    let (browser, browser_context_id) = watch.cookie_store()?;
    let response = browser
        .execute(StorageGetCookiesParams { browser_context_id })
        .await
        .map_err(|error| CrawlError::browser_error(format!("failed to read browser cookies: {error}")))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CrawlError::browser_error(format!("system clock precedes Unix epoch: {error}")))?
        .as_secs_f64();
    Ok(response
        .result
        .cookies
        .into_iter()
        .filter_map(|cookie| browser_cookie(cookie, now))
        .collect())
}

/// Preserve a stored cookie as a CDP set-cookie parameter without reviving an expired cookie or
/// turning a host-only cookie into a domain cookie. ~keep
fn browser_cookie(cookie: Cookie, now: f64) -> Option<BrowserCookie> {
    if (!cookie.session && (!cookie.expires.is_finite() || cookie.expires <= now))
        || cookie.partition_key_opaque == Some(true)
    {
        return None;
    }
    let (url, domain) = if cookie.domain.starts_with('.') {
        (None, Some(cookie.domain.clone()))
    } else {
        (Some(host_only_cookie_url(&cookie)?), None)
    };
    Some(BrowserCookie {
        params: CookieParam {
            name: cookie.name,
            value: cookie.value,
            url,
            domain,
            path: Some(cookie.path),
            secure: Some(cookie.secure),
            http_only: Some(cookie.http_only),
            same_site: cookie.same_site,
            expires: (!cookie.session).then(|| TimeSinceEpoch::new(cookie.expires)),
            priority: Some(cookie.priority),
            same_party: None,
            source_scheme: Some(cookie.source_scheme),
            source_port: Some(cookie.source_port),
            partition_key: cookie.partition_key,
        },
    })
}

fn host_only_cookie_url(cookie: &Cookie) -> Option<String> {
    let scheme = match cookie.source_scheme {
        CookieSourceScheme::Secure => "https",
        CookieSourceScheme::NonSecure => "http",
        CookieSourceScheme::Unset if cookie.secure => "https",
        CookieSourceScheme::Unset => "http",
    };
    let mut url = url::Url::parse(&format!("{scheme}://cookie.invalid/")).ok()?;
    url.set_host(Some(&cookie.domain)).ok()?;
    if let Ok(port) = u16::try_from(cookie.source_port)
        && port != 0
    {
        url.set_port(Some(port)).ok()?;
    }
    url.set_path(&cookie.path);
    Some(url.into())
}

/// ~keep A store snapshot cannot identify same-value rewrites or deleted cookies. Comparing
/// after replay excludes profile/external cookies and copies inherited from earlier crawl pages.
pub(super) fn result_cookie_delta(
    before: &[BrowserCookie],
    after: &[BrowserCookie],
    requested: &str,
    final_url: &str,
) -> Vec<crate::types::CookieInfo> {
    let before: HashMap<_, _> = before.iter().map(|cookie| (Slot::of(cookie), &cookie.params)).collect();
    let hosts: Vec<_> = [requested, final_url]
        .into_iter()
        .filter_map(|raw| url::Url::parse(raw).ok())
        .filter_map(|url| url.host_str().map(str::to_owned))
        .collect();
    after
        .iter()
        .filter_map(|cookie| {
            let slot = Slot::of(cookie);
            if before.get(&slot).is_some_and(|params| **params == cookie.params) {
                return None;
            }
            let domain = slot.site?;
            let matches = hosts.iter().any(|host| {
                if domain.starts_with('.') {
                    let domain = domain.trim_start_matches('.');
                    host == domain || host.strip_suffix(domain).is_some_and(|prefix| prefix.ends_with('.'))
                } else {
                    host == &domain
                }
            });
            matches.then(|| {
                crate::types::CookieInfo {
                    name: cookie.params.name.clone(),
                    value: cookie.params.value.clone(),
                    domain: cookie.params.domain.clone(),
                    path: cookie.params.path.clone(),
                }
                .with_stored_scope(&domain, "/")
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
