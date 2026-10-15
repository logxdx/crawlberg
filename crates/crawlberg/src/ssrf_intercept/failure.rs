use std::sync::atomic::Ordering;

use super::Shared;

pub(super) fn child_fail(shared: &Shared, reason: &'static str) -> bool {
    // ~keep A requested stop and a failure cascade already end under the existing stopping guard.
    if !shared.stopping.load(Ordering::Acquire) {
        tracing::error!(
            target: "crawlberg::ssrf_intercept",
            phase = "child_target_controller",
            reason,
            browser_handler_task_id = ?shared.handler_abort.as_ref().map(tokio::task::AbortHandle::id),
            "browser child-target controller failed closed"
        );
    }
    false
}

pub(super) fn owned_transport_end(browser: &chromiumoxide::Browser) -> bool {
    browser.observed_owned_termination()
}

pub(super) fn owned_command_cancelled(
    browser: &chromiumoxide::Browser,
    error: &chromiumoxide::error::CdpError,
) -> bool {
    owned_transport_end(browser)
        && matches!(
            error,
            chromiumoxide::error::CdpError::ChannelSendError(_) | chromiumoxide::error::CdpError::NoResponse
        )
}

pub(super) async fn disable_finished(browser: &chromiumoxide::Browser) -> bool {
    match tokio::time::timeout(
        super::CONTROLLER_TEARDOWN_TIMEOUT,
        browser.execute(super::FetchDisableParams::default()),
    )
    .await
    {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => owned_command_cancelled(browser, &error),
        Err(_) => false,
    }
}

pub(super) fn child_transport_end(
    browser: Option<&chromiumoxide::Browser>,
    shared: &Shared,
    error: Option<&chromiumoxide::error::CdpError>,
    reason: &'static str,
) -> bool {
    if browser.is_some_and(|browser| browser.observe_owned_transport_end(error)) {
        return true;
    }
    child_fail(shared, reason)
}

pub(super) fn child_ack_fail(shared: &Shared, pending_command: &'static str, cdp_error_code: Option<i64>) -> bool {
    if !shared.stopping.load(Ordering::Acquire) {
        tracing::error!(
            target: "crawlberg::ssrf_intercept",
            phase = "child_target_controller",
            reason = "boundary_or_close_ack",
            browser_handler_task_id = ?shared.handler_abort.as_ref().map(tokio::task::AbortHandle::id),
            pending_command,
            cdp_error_code,
            "browser child-target controller failed closed"
        );
    }
    false
}

async fn quarantine_command<S: super::ChildSocket>(
    socket: &mut S,
    next_id: &mut u64,
    method: &str,
    params: serde_json::Value,
) -> Option<serde_json::Value> {
    let id = super::child_command_id(next_id)?;
    socket.send(super::child_message(id, method, params, None)).await.ok()?;
    loop {
        match socket.next().await? {
            Ok(async_tungstenite::tungstenite::Message::Text(text)) => {
                let Ok(value) = chromiumoxide::conn::parse_message::<serde_json::Value>(text.as_ref()) else {
                    continue;
                };
                if value["id"].as_u64() == Some(id) {
                    return Some(value);
                }
            }
            Ok(async_tungstenite::tungstenite::Message::Ping(payload)) => {
                socket.send(async_tungstenite::tungstenite::Message::Pong(payload)).await.ok()?;
            }
            Ok(async_tungstenite::tungstenite::Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

pub(super) fn quarantine_snapshot(shared: &Shared) -> (
    std::collections::HashSet<super::BrowserContextId>,
    std::collections::HashSet<super::TargetId>,
) {
    let registry = super::lock(&shared.registry);
    let contexts = registry.contexts.keys().chain(registry.retired_contexts.iter()).cloned().collect();
    let targets = registry.ownership.iter().filter_map(|(target, ownership)| {
        (!matches!(ownership, super::TargetOwnership::Other)).then(|| target.clone())
    }).collect();
    (contexts, targets)
}

pub(super) async fn quarantine_external_socket<S: super::ChildSocket>(
    mut socket: S,
    shared: std::sync::Arc<Shared>,
    mut contexts: std::collections::HashSet<super::BrowserContextId>,
    mut owned_targets: std::collections::HashSet<super::TargetId>,
) {
    // ~keep Late replies from the failed controller use IDs starting at one.
    let mut next_id = u64::MAX / 2;
    loop {
        let (current_contexts, current_targets) = quarantine_snapshot(&shared);
        contexts.extend(current_contexts);
        owned_targets.extend(current_targets);
        for context in &contexts {
            if quarantine_command(&mut socket, &mut next_id, "Target.disposeBrowserContext",
                serde_json::json!({"browserContextId": context})).await.is_none() {
                return;
            }
        }
        let Some(existing) = quarantine_command(
            &mut socket, &mut next_id, "Target.getBrowserContexts", serde_json::json!({}),
        ).await else { return };
        let Some(targets) = quarantine_command(
            &mut socket, &mut next_id, "Target.getTargets", serde_json::json!({"filter": [{}]}),
        ).await else { return };
        let absent = existing["result"]["browserContextIds"].as_array().is_some_and(|ids| {
            ids.iter().all(|id| id.as_str().is_some_and(|id| !contexts.iter().any(|context| id == context.inner())))
        }) && targets["result"]["targetInfos"].as_array().is_some_and(|infos| {
            infos.iter().all(|info| {
                info["targetId"].as_str().is_some_and(|id| !owned_targets.iter().any(|target| id == target.inner()))
                    && !contexts.iter().any(|context| info["browserContextId"].as_str() == Some(context.inner()))
            })
        });
        if absent && shared.opening.load(Ordering::Acquire) == 0 {
            let registry = super::lock(&shared.registry);
            if registry.contexts.keys().all(|context| contexts.contains(context)) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}
