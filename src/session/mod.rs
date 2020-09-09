use std::fmt;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use futures::prelude::*;

use anyhow::anyhow;
use async_trait::async_trait;
use futures::select_biased;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use log::error;
use log::info;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::pin;
use tokio::sync::watch;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::error::Error as WsError;
use tokio_tungstenite::tungstenite::handshake::server::Request;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::WebSocketStream;

use self::events::Event;
use self::events::EventSink;
use self::events::EventSinkCtx;
use self::events::EventStream;
use self::events::HandoffPacket;
use self::id::SessionId;

mod conn;
mod events;
mod id;
mod map;

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("unable to send event to session")]
    EventSinkClosed,
    #[error("unable to send update to event sink map")]
    MapUpdaterClosed,
    #[error("unable to send closing notifier to takeover session")]
    HandoffChannelClosed,
    #[error("unable to handoff due to error in old session")]
    HandoffCancelled,
}

type TcpWebSocketStream = WebSocketStream<TcpStream>;

pub trait State: 'static + fmt::Debug + Send + Sync + Sized {
    type Key: 'static + Copy + fmt::Debug + Eq + Ord + Hash + Send + Sync + Unpin;
    type Message: 'static + fmt::Debug + Send + Sync;

    fn from_request(request: &Request) -> anyhow::Result<Self>;
    fn get_key(&self) -> Self::Key;
}

pub use conn::Connect;

#[async_trait]
pub trait Handler: 'static + Sized {
    async fn handle_open(&mut self);
    async fn handle_close(&mut self);
    async fn handle_message(&mut self, msg: Message);
}

// trait State: StateInner + Handler {}
// impl<T: StateInner + Handler> State for T {}

type MapUpdater<T> = watch::Sender<Option<EventSinkCtx<T>>>;

pub struct Session<T: State>
where
    Self: Handler,
{
    pub id: SessionId,
    pub state: T,
    notify: Arc<Notify>,
    msg_sink: SplitSink<TcpWebSocketStream, Message>,
    event_sink: EventSink<T>,
}

impl<T: State> Session<T>
where
    Self: Handler,
{
    async fn process_events(
        mut self,
        msg_stream: SplitStream<TcpWebSocketStream>,
        event_stream: EventStream<T>,
        map_updater: MapUpdater<T>,
    ) -> anyhow::Result<()> {
        let mut msg_stream = msg_stream.fuse();
        let mut event_stream = event_stream.fuse();

        loop {
            pin! {
                let next_message = msg_stream.next();
                let next_event = event_stream.next();
            }

            select_biased! {
                res = next_message => match res {
                    None => return Ok(()),
                    Some(msg) => {
                        self.event_sink
                            .send(Event::Message(msg?))
                            .await?;
                    }
                },

                res = next_event => match res {
                    None => return Err(anyhow!("Event stream ended prematurely")),
                    Some(evt) => match evt {
                        Event::Message(msg) => {
                            self.handle_message(msg).await;
                        },
                        Event::Event(e) => {
                            let _ = e;
                            unimplemented!()
                        },
                        Event::Handoff(chan) => {
                            // First, make anyone that asks for this event sink wait for handoff.
                            let event_stream = event_stream.into_inner();
                            let handoff_complete = Arc::clone(&self.notify);
                            let packet = map_updater.broadcast(None)
                                .map(move |_| HandoffPacket {
                                    event_stream,
                                    map_updater,
                                    handoff_complete,
                                });

                            let (packet, err) = match packet {
                                Ok(packet) => (Some(packet), Ok(())),
                                Err(_) => (None, Err(LifecycleError::MapUpdaterClosed)),
                            };

                            err.and(chan.send(packet).map_err(|_| LifecycleError::HandoffChannelClosed))?;
                            return Ok(());
                        },
                    },
                },
            }
        }
    }
}

impl<T: State> Drop for Session<T>
where
    Self: Handler,
{
    fn drop(&mut self) {
        self.notify.notify();
        tokio::task::block_in_place(|| futures::executor::block_on(self.handle_close()));
        info!(
            "Shut down session {:?} for {:?}",
            self.id,
            self.state.get_key()
        );
    }
}

impl<T: State> Sink<Message> for Session<T>
where
    Self: Handler,
{
    type Error = WsError;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> Poll<Result<(), Self::Error>> {
        let sink = unsafe { self.map_unchecked_mut(|s| &mut s.msg_sink) };
        Sink::poll_ready(sink, cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        let sink = unsafe { self.map_unchecked_mut(|s| &mut s.msg_sink) };
        Sink::start_send(sink, item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> Poll<Result<(), Self::Error>> {
        let sink = unsafe { self.map_unchecked_mut(|s| &mut s.msg_sink) };
        Sink::poll_flush(sink, cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> Poll<Result<(), Self::Error>> {
        let sink = unsafe { self.map_unchecked_mut(|s| &mut s.msg_sink) };
        Sink::poll_close(sink, cx)
    }
}
