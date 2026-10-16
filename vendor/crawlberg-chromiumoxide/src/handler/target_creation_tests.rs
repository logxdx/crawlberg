use super::*;
use futures::channel::oneshot;

type TestSocket = async_tungstenite::WebSocketStream<async_tungstenite::tokio::TokioAdapter<tokio::net::TcpStream>>;

async fn handler() -> (Handler, TestSocket) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    let address = listener.local_addr().expect("listener address");
    let accepted = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept handler connection");
        async_tungstenite::tokio::accept_async(stream)
            .await
            .expect("WebSocket handshake")
    });
    let conn = Connection::connect(format!("ws://{address}"))
        .await
        .expect("handler connection");
    let (_sender, receiver) = futures::channel::mpsc::channel(1);
    (
        Handler::new(conn, receiver, HandlerConfig::default()),
        accepted.await.expect("server task"),
    )
}

fn target_created() -> EventTargetCreated {
    serde_json::from_value(serde_json::json!({"targetInfo": {
        "targetId": "PAGE", "type": "page", "title": "", "url": "about:blank",
        "attached": false, "canAccessOpener": false
    }}))
    .expect("target-created event")
}

fn respond_to_page_creation(handler: &mut Handler) -> oneshot::Receiver<Result<Page>> {
    let (sender, receiver) = oneshot::channel();
    handler.create_page(CreateTargetParams::new("about:blank"), sender);
    assert_eq!(handler.pending_commands.len(), 1);
    let id = *handler.pending_commands.keys().next().expect("pending create command");
    let response = serde_json::from_value(serde_json::json!({"id": id, "result": {"targetId": "PAGE"}}))
        .expect("create-target response");
    handler.on_response(response);
    receiver
}

#[tokio::test]
async fn should_wait_for_target_discovery_when_creation_response_arrives_first() {
    let (mut handler, _socket) = handler().await;
    let mut page = respond_to_page_creation(&mut handler);
    assert!(
        page.try_recv()
            .expect("creation remains live before discovery")
            .is_none()
    );
    handler.on_target_created(target_created());
    assert_eq!(handler.targets.len(), 1);
    assert!(page.try_recv().expect("page initializes after discovery").is_none());
}

#[tokio::test]
async fn should_preserve_page_creation_when_target_discovery_is_repeated() {
    let (mut handler, _socket) = handler().await;
    handler.on_target_created(target_created());
    let mut page = respond_to_page_creation(&mut handler);
    handler.on_target_created(target_created());
    assert!(
        page.try_recv()
            .expect("duplicate discovery must preserve the page operation")
            .is_none()
    );
    assert_eq!(handler.target_ids.len(), 1);
}

#[tokio::test]
async fn should_time_out_creation_when_its_target_is_never_discovered() {
    let (mut handler, _socket) = handler().await;
    let page = respond_to_page_creation(&mut handler);
    handler.evict_timed_out_commands(Instant::now() + handler.config.request_timeout + Duration::from_millis(1));
    assert!(matches!(
        page.await.expect("creation must report an error"),
        Err(CdpError::Timeout)
    ));
}

#[tokio::test]
async fn should_report_target_destruction_before_discovery_as_an_error() {
    let (mut handler, _socket) = handler().await;
    let page = respond_to_page_creation(&mut handler);
    handler.on_target_destroyed(EventTargetDestroyed {
        target_id: TargetId::new("PAGE"),
    });
    let result = page.await.expect("target destruction must not cancel the operation");
    assert!(matches!(result, Err(CdpError::ChromeMessage(message)) if message.contains("destroyed before discovery")));
}
