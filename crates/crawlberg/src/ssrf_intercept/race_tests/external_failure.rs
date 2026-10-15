use super::*;
use async_tungstenite::tungstenite::Message;
use futures::SinkExt as _;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

async fn reject_service_worker_arm_relay(upstream: String, rejected: Arc<AtomicBool>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("relay listener");
    let endpoint = format!("ws://{}", listener.local_addr().expect("relay address"));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let upstream = upstream.clone();
            let rejected = Arc::clone(&rejected);
            tokio::spawn(async move {
                let client = async_tungstenite::tokio::accept_async(stream).await.expect("relay handshake");
                let (chrome, _) = async_tungstenite::tokio::connect_async(upstream).await.expect("relay upstream");
                let (mut to_client, mut from_client) = futures::StreamExt::split(client);
                let (mut to_chrome, mut from_chrome) = futures::StreamExt::split(chrome);
                let sessions = Arc::new(Mutex::new(HashSet::<String>::new()));
                let pending = Arc::new(Mutex::new(HashSet::<u64>::new()));
                let workers = Arc::clone(&sessions);
                let calls = Arc::clone(&pending);
                let forward = tokio::spawn(async move {
                    while let Some(Ok(message)) = futures::StreamExt::next(&mut from_client).await {
                        if let Message::Text(text) = &message {
                            let value: serde_json::Value = serde_json::from_str(text).expect("command JSON");
                            if value["method"] == "Target.setAutoAttach"
                                && value["sessionId"].as_str().is_some_and(|id| lock(&workers).contains(id))
                                && let Some(id) = value["id"].as_u64() {
                                lock(&calls).insert(id);
                            }
                        }
                        if to_chrome.send(message).await.is_err() { break }
                    }
                });
                while let Some(Ok(mut message)) = futures::StreamExt::next(&mut from_chrome).await {
                    if let Message::Text(text) = &message {
                        let value: serde_json::Value = serde_json::from_str(text).expect("Chrome JSON");
                        if value["method"] == "Target.attachedToTarget"
                            && value["params"]["targetInfo"]["type"] == "service_worker"
                            && let Some(session) = value["params"]["sessionId"].as_str() {
                            lock(&sessions).insert(session.to_owned());
                        }
                        if value["id"].as_u64().is_some_and(|id| lock(&pending).remove(&id)) {
                            message = Message::Text(serde_json::json!({
                                "id": value["id"], "error": {"code": -32000, "message": "controlled arm rejection"}
                            }).to_string().into());
                            rejected.store(true, Ordering::Release);
                        }
                    }
                    if to_client.send(message).await.is_err() { break }
                }
                forward.abort();
            });
        }
    });
    endpoint
}

async fn counted_denied_listener() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("denied listener");
    let url = format!("http://{}/denied", listener.local_addr().expect("denied address"));
    let connections = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::clone(&connections);
    let received = Arc::clone(&bytes);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            accepted.fetch_add(1, Ordering::SeqCst);
            let received = Arc::clone(&received);
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                let count = stream.read(&mut buffer).await.expect("denied request read");
                received.fetch_add(count, Ordering::SeqCst);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            });
        }
    });
    (url, connections, bytes)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_service_worker_never_reaches_a_denied_listener_after_external_controller_failure() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let test_name = "a_held_service_worker_never_reaches_a_denied_listener_after_external_controller_failure";
    let Some(mut owner) = launch(test_name).await else { return };
    let owner_tab = owner.new_page("about:blank").await.expect("owner tab");
    let (denied, connections, bytes) = counted_denied_listener().await;
    let site = MockServer::start().await;
    Mock::given(method("GET")).and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>service worker owner</p>", "text/html"))
        .mount(&site).await;
    Mock::given(method("GET")).and(wiremock::matchers::path_regex(r"^/(control|service)\.js$"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(format!(
            "self.addEventListener('install', event => event.waitUntil(fetch({denied:?}, {{mode:'no-cors'}})));"
        ), "application/javascript"))
        .mount(&site).await;
    let control_context = owner.create_browser_context(Default::default()).await.expect("control context");
    let mut control_params = chromiumoxide::cdp::browser_protocol::target::CreateTargetParams::new("about:blank");
    control_params.browser_context_id = Some(control_context.clone());
    let control_page = owner.new_page(control_params).await.expect("control tab");
    control_page.goto(site.uri().replace("127.0.0.1", "localhost")).await.expect("control page");
    control_page.evaluate("navigator.serviceWorker.register('/control.js').then(() => navigator.serviceWorker.ready)")
        .await.expect("control worker must install");
    assert!(connections.load(Ordering::SeqCst) >= 1, "the unprotected service worker must reach the listener");
    assert!(bytes.load(Ordering::SeqCst) > 0, "the control must deliver actual HTTP bytes");
    owner.dispose_browser_context(control_context.clone()).await.expect("dispose control worker");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !super::super::context_absent(&owner, &control_context).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("control context must disappear");
    drop(control_page);
    connections.store(0, Ordering::SeqCst);
    bytes.store(0, Ordering::SeqCst);
    let rejected = Arc::new(AtomicBool::new(false));
    let endpoint = reject_service_worker_arm_relay(owner.websocket_address().clone(), Arc::clone(&rejected)).await;
    let (browser, handler) = crate::browser_pool::connect_endpoint(&endpoint).await.expect("external connection");
    let handler = crate::browser_pool::spawn_handler(handler);
    let browser = Arc::new(browser);
    let firewall = BrowserFirewall::start_supervised(
        Arc::clone(&browser), BrowserOrigin::External, PageContext::Copied, handler.abort_handle(),
    ).await.expect("external firewall");
    let page = firewall.handle().new_page(None, None).await.expect("owned page");
    let watch = firewall.handle().watch(&page, &config(), 0).await.expect("owned watch");
    let context = watch.owned_context.clone().expect("isolated context");
    page.goto(site.uri().replace("127.0.0.1", "localhost")).await.expect("owned page navigation");
    let _ = page.evaluate("navigator.serviceWorker.register('/service.js').catch(() => 0); 1").await;
    tokio::time::timeout(Duration::from_secs(15), firewall.handle().failed()).await.expect("injected failure");
    assert!(rejected.load(Ordering::Acquire), "the relay must actually reject the paused service worker arm");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let params: GetTargetsParams = serde_json::from_value(serde_json::json!({"filter": [{}]}))
                .expect("all-target census parameters");
            let targets = owner.execute(params).await.expect("owner target census");
            if !targets.result.target_infos.iter().any(|target| target.browser_context_id.as_ref() == Some(&context)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("owned page and service worker disposal");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let survived = owner_tab.evaluate("6 * 7").await.expect("owner tab survives").into_value::<u32>().expect("owner result");
    assert_eq!(survived, 42);
    assert_eq!(connections.load(Ordering::SeqCst), 0, "the held service worker must never connect");
    assert_eq!(bytes.load(Ordering::SeqCst), 0, "the denied listener must receive no request bytes");
    drop(watch);
    firewall.stop().await;
    drop(page);
    drop(browser);
    let owner = Arc::get_mut(&mut owner).expect("exclusive browser owner");
    owner.kill().await.expect("Chrome process").expect("kill Chrome");
}
