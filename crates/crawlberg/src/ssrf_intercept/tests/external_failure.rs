use super::*;

#[tokio::test]
async fn a_failed_external_controller_keeps_its_socket_until_owned_targets_are_absent() {
    for cancel in [false, true] {
        assert_external_failure_retains_held_workers(cancel).await;
    }
}

async fn assert_external_failure_retains_held_workers(cancel: bool) {
    let page = watched("ROOT");
    let shared = Arc::new(shared_with(&page, "FOREIGN"));
    let context = register_owned_context(&shared, &page);
    let (mut socket, _commands, controller) =
        test_child_controller_with_quarantine(shared, Duration::from_secs(1), true).await;
    send_attached_target(&mut socket, "S-WORKER", "WORKER", Some("ROOT"), &context).await;
    let arm = next_child_message(&mut socket).await;
    assert_eq!(arm["method"], "Target.setAutoAttach");
    if cancel {
        controller.abort();
        assert!(controller.await.expect_err("aborted controller").is_cancelled());
    } else {
        socket
            .send(async_tungstenite::tungstenite::Message::Text(
                serde_json::json!({"id": arm["id"], "error": {"code": -32000, "message": "rejected"}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("reject worker arm");
        assert!(!controller.await.expect("failed controller"));
    }

    for (context_present, worker_still_present) in [(true, false), (false, true), (false, false)] {
        let dispose = next_child_message(&mut socket).await;
        assert_eq!(dispose["method"], "Target.disposeBrowserContext");
        assert_eq!(
            dispose["params"]["browserContextId"].as_str(),
            Some(context.inner().as_str())
        );
        answer_child_command(&mut socket, &dispose).await;
        let contexts = next_child_message(&mut socket).await;
        assert_eq!(contexts["method"], "Target.getBrowserContexts");
        let ids = if context_present {
            serde_json::json!([context.inner()])
        } else {
            serde_json::json!([])
        };
        socket
            .send(async_tungstenite::tungstenite::Message::Text(
                serde_json::json!({"id": contexts["id"], "result": {"browserContextIds": ids}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("answer context census");
        let targets = next_child_message(&mut socket).await;
        assert_eq!(targets["method"], "Target.getTargets");
        assert_eq!(targets["params"]["filter"], serde_json::json!([{}]));
        let infos = if worker_still_present {
            serde_json::json!([{"targetId": "WORKER", "browserContextId": context.inner()}])
        } else {
            serde_json::json!([])
        };
        socket
            .send(async_tungstenite::tungstenite::Message::Text(
                serde_json::json!({"id": targets["id"], "result": {"targetInfos": infos}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("answer all-target census");
    }
    let ended = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("the quarantine releases its socket after authoritative absence");
    assert!(matches!(
        ended,
        None | Some(Ok(async_tungstenite::tungstenite::Message::Close(_))) | Some(Err(_))
    ));
}
