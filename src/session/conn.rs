use std::collections::hash_map;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use futures::StreamExt;
use log::error;
use log::info;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::handshake::server::Request;

use super::events::EventPipe;
use super::events::EventSinkCtx;
use super::map::EventSinkEntry;
use super::Handler;
use super::LifecycleError;
use super::MapUpdater;
use super::Session;
use super::SessionId;
use super::State;

#[async_trait]
pub trait Connect: State {
    async fn start_session(stream: TcpStream) -> anyhow::Result<()>;
}

async fn handoff_or_make_pipe<T: State>(
    id: SessionId,
    state: &T,
) -> anyhow::Result<(EventPipe<T>, MapUpdater<T>)> {
    let key = state.get_key();
    let event_sink_map = super::map::get_or_create_event_sink_map().await;
    let mut old_sink_ctx = {
        let unlocked_map = event_sink_map.read().await;
        match unlocked_map.get(&key) {
            None => None,
            Some(entry) => entry.get().await.ok(),
        }
    };

    //  map_updater: A tokio watch channel that broadcasts updates to the map.  Accepts None to
    //    block all interested parties until the handoff is complete, and Some(EventSinkCtx) to
    //    update/unblock.  The public interface to the event sink map acts like a future that
    //    doesn't resolve unless Some was most recently sent to the watch.
    //  old_event_pipe: The event sink/stream pair from the previous session.
    //  handoff_complete: A tokio Notify that blocks us until the previous session's task is
    //    finished.  This means the previous session's WebSocket is closed, which guarantees no
    //    more communication from that client.
    let (map_updater, old_event_pipe, handoff_complete) = loop {
        match old_sink_ctx {
            Some(ctx) => {
                // There's another active session for this key.  Let's take it over.
                info!("Handoff {:?} -> {:?} for {:?}", ctx.id, id, key);

                let packet = match ctx.handoff_session().await {
                    Ok(packet) => packet,
                    Err(e) => {
                        old_sink_ctx = None;
                        error!("Handoff {:?} -> {:?} cancelled: {}", ctx.id, id, e);

                        continue;
                    }
                };

                break (
                    packet.map_updater,
                    Some((ctx.event_sink, packet.event_stream)),
                    Some(packet.handoff_complete),
                );
            }
            None => {
                let mut unlocked_map = event_sink_map.write().await;
                match unlocked_map.entry(key) {
                    hash_map::Entry::Occupied(entry) => {
                        old_sink_ctx = entry.into_mut().get().await.ok();
                    }
                    hash_map::Entry::Vacant(entry) => {
                        info!("Starting session {:?} for {:?}", id, key);
                        let (map_updater, rx) = watch::channel(None);
                        entry.insert(EventSinkEntry::new(rx));
                        break (map_updater, None, None);
                    }
                }
            }
        }
    };

    if let Some(future) = handoff_complete {
        future.notified().await;
    }

    let event_pipe = old_event_pipe.unwrap_or_else(|| super::events::make_event_pipe(key));
    let event_sink_ctx = EventSinkCtx::new(id, state, event_pipe.0.clone());

    // We have everything we need to update the event sink map.  Broadcast it and block on
    // completion of the previous session.  If we can't broadcast, event sources won't be able to
    // publish events to our sink, so we need to bubble the error up to tear down our task.
    map_updater
        .broadcast(Some(event_sink_ctx))
        .map_err(|_| LifecycleError::MapUpdaterClosed)?;

    Ok((event_pipe, map_updater))
}

#[async_trait]
impl<T: State> Connect for T
where
    Session<T>: Handler,
{
    async fn start_session(stream: TcpStream) -> anyhow::Result<()> {
        let addr = stream
            .peer_addr()
            .expect("connected streams should have a peer address");
        info!("Peer address: {}", addr);

        let mut state = None;
        let accept_cb = |req: &Request, resp| {
            <T as State>::from_request(req)
                .map(|s| {
                    state.replace(s);
                    resp
                })
                .map_err(|e| {
                    error!("{}", e);
                    use http::StatusCode;
                    http::Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(None)
                        .unwrap()
                })
        };

        let ws = tokio_tungstenite::accept_hdr_async(stream, accept_cb)
            .await
            .context("Handshake error")?;

        let state: T = state.unwrap();
        info!("New WebSocket connection: {}", addr);

        let id = SessionId::next();
        let (msg_sink, msg_stream) = ws.split();
        let (event_pipe, map_updater) = handoff_or_make_pipe(id, &state).await?;
        let (event_sink, event_stream) = event_pipe;

        let notify = Arc::new(Notify::new());
        let mut session: Session<T> = Session {
            id,
            state,
            notify,
            msg_sink,
            event_sink,
        };

        // TODO do this with a message to differentiate between handoff and open
        if let Err(e) = session.handle_open().await {
            error!("Error in handle_open for {:?}: {}", id, e);
        }

        let fut = session.process_events(msg_stream, event_stream, map_updater);

        Ok(fut.await?)
    }
}
