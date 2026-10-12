use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use asyncband::{
    event::ManualResetEvent,
    mpsc::{self, BoundedReceiver},
};
use futures_util::{FutureExt, pin_mut, select_biased};

use super::{Mailbox, MailboxInner, Name};
use crate::{Actor, actor::Delivering};

/// State shared between a mailbox and its receiver, outside the message queue.
#[derive(Debug, Default)]
pub(super) struct Control {
    /// Set once a stop has been requested.
    pub(super) stop: ManualResetEvent,
    /// Set once the receiver has been dropped.
    pub(super) closed: AtomicBool,
}

pub(crate) struct Receiver<A: Actor> {
    messages: BoundedReceiver<Delivering<A>>,
    control: Arc<Control>,
}

impl<A: Actor> Receiver<A> {
    pub(crate) async fn recv(&mut self) -> MailboxEvent<A> {
        let stop = self.control.stop.wait().fuse();
        let message = self.messages.recv().fuse();
        pin_mut!(stop, message);

        select_biased! {
            _ = stop => MailboxEvent::Stop,
            message = message => match message {
                Ok(message) => MailboxEvent::Message(message),
                Err(_) => MailboxEvent::Stop,
            },
        }
    }
}

impl<A: Actor> Drop for Receiver<A> {
    fn drop(&mut self) {
        self.control.closed.store(true, Ordering::Release);
    }
}

pub(crate) enum MailboxEvent<A: Actor> {
    Message(Delivering<A>),
    Stop,
}

pub(crate) fn make_mailbox<A: Actor>(
    name: Option<Name>,
    capacity: NonZeroUsize,
) -> (Mailbox<A>, Receiver<A>) {
    let (message_tx, message_rx) = mpsc::bounded(capacity.get());
    let control = Arc::new(Control::default());
    let inner = Arc::new(MailboxInner {
        name,
        messages: message_tx,
        control: control.clone(),
        stopping: AtomicBool::new(false),
        capacity,
    });

    (
        Mailbox { inner },
        Receiver {
            messages: message_rx,
            control,
        },
    )
}
