use std::collections::hash_map;
use std::fmt;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::SinkExt;
use futures::Stream;
use log::debug;
use log::warn;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

use crate::sync::UnboundedSink;

use super::id::SessionId;
use super::map;
use super::LifecycleError;
use super::State;

pub enum Event<T: State> {
    Event(T::Message),
    Message(Message),
    Handoff(oneshot::Sender<Option<HandoffPacket<T>>>),
}

impl<T: State> fmt::Debug for Event<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Event(msg) => f.debug_tuple("Event").field(msg).finish(),
            Event::Message(msg) => f.debug_tuple("Message").field(msg).finish(),
            Event::Handoff(_) => f.debug_tuple("Handoff").field(&"<channel>").finish(),
        }
    }
}

pub type EventSink<T> = UnboundedSink<Event<T>>;
pub type EventPipe<T> = (EventSink<T>, EventStream<T>);

pub struct EventStream<T: State> {
    key: T::Key,
    stream: mpsc::UnboundedReceiver<Event<T>>,
}

impl<T: State> Stream for EventStream<T> {
    type Item = Event<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let stream = unsafe { self.map_unchecked_mut(|this| &mut this.stream) };
        stream.poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.size_hint()
    }
}

impl<T: State> Drop for EventStream<T> {
    fn drop(&mut self) {
        tokio::task::block_in_place(|| {
            futures::executor::block_on(async move {
                let event_sink_map = map::get_event_sink_map::<T>().await;
                let ctx = {
                    let mut unlocked_map = event_sink_map.write().await;
                    match unlocked_map.entry(self.key) {
                        hash_map::Entry::Occupied(entry) => Some(entry.remove()),
                        hash_map::Entry::Vacant(_) => {
                            warn!("Event sink missing from map during session cleanup");
                            None
                        }
                    }
                };
                ctx.expect("Missing event sink in map during cleanup");
                debug!("Dropped event sink {:?}", self.key);
            })
        });
    }
}

pub fn make_event_pipe<T: State>(key: T::Key) -> EventPipe<T> {
    let (sink, stream) = mpsc::unbounded_channel();
    let sink = UnboundedSink::new(sink);
    let stream = EventStream { key, stream };
    (sink, stream)
}

pub struct EventSinkCtx<T: State> {
    pub id: SessionId,
    pub key: T::Key,
    pub event_sink: EventSink<T>,
    phantom: PhantomData<fn(T)>,
}

impl<T: State> Clone for EventSinkCtx<T> {
    fn clone(&self) -> Self {
        EventSinkCtx {
            id: self.id,
            key: self.key,
            event_sink: self.event_sink.clone(),
            phantom: self.phantom,
        }
    }
}

impl<T: State> EventSinkCtx<T> {
    pub fn new(id: SessionId, state: &T, event_sink: EventSink<T>) -> Self {
        EventSinkCtx {
            id,
            key: state.get_key(),
            event_sink,
            phantom: PhantomData,
        }
    }

    pub async fn handoff_session(&self) -> Result<HandoffPacket<T>, LifecycleError> {
        let (sender, receiver) = oneshot::channel();
        let mut sink = self.event_sink.clone();
        sink.send(Event::Handoff(sender))
            .await
            .map_err(|_| LifecycleError::EventSinkClosed)?;
        receiver
            .await
            .ok()
            .flatten()
            .ok_or(LifecycleError::HandoffCancelled)
    }
}

pub struct HandoffPacket<T: State> {
    pub event_stream: EventStream<T>,
    pub map_updater: watch::Sender<Option<EventSinkCtx<T>>>,
    pub handoff_complete: Arc<Notify>,
}
