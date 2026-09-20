use std::{fmt::Debug, ptr::NonNull};

use compio_send_wrapper::SendWrapper;
use slotmap::new_key_type;

use crate::{Shared, console::SpawnMeta, task::Task, util::assert_not_impl};

new_key_type! { pub struct TaskId; }

use compio_log::{instrument, trace};
use slotmap::SlotMap;

use crate::UnsafeCell;

/// A single-threaded dual queue (hot and cold) for scheduling tasks.
pub struct TaskQueue {
    inner: UnsafeCell<Inner>,
}

assert_not_impl!(TaskQueue, Send);
assert_not_impl!(TaskQueue, Sync);

impl Debug for TaskQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe {
            self.with_inner(|inner| {
                f.debug_struct("TaskQueue")
                    .field("map", &inner.map)
                    .field("hot", &inner.hot)
                    .field("cold", &inner.cold)
                    .finish()
            })
        }
    }
}

#[derive(Debug)]
struct Inner {
    map: SlotMap<TaskId, Item>,
    hot: List,
    cold: List,
}

#[derive(Debug, Clone, Copy, Default)]
struct List {
    head: Option<TaskId>,
    tail: Option<TaskId>,
}

#[derive(Debug)]
struct Item {
    prev: Option<TaskId>,
    next: Option<TaskId>,
    task: Option<Task>,
    place: Place,
}

/// Where a task sits between polls.
///
/// A task used to be in one of the two lists at all times, including while it
/// was being polled, which put it in the cold one. A wake arriving during its
/// own poll — what every future that yields does — then had to walk it back out
/// of the cold list and onto the tail of the hot one, so a poll-and-self-wake
/// cycle paid for two full list migrations. [`Place::Running`] is the third
/// state that removes one of them: the executor takes the task out of both
/// lists for the duration of the poll, a wake that arrives meanwhile only
/// records that it happened, and the tick links the task back into whichever
/// list that answer names.
///
/// The one visible difference is where a self-woken task lands among the tasks
/// woken during its own poll: it used to go to the hot tail at the moment of
/// the wake, ahead of them, and now goes there when the poll returns, behind
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Linked into the hot list, waiting to be polled.
    Hot,
    /// Linked into the cold list, waiting to be woken.
    Cold,
    /// Held by the executor for a poll, in neither list. `woken` records
    /// whether a wake arrived while it was.
    Running { woken: bool },
}

#[derive(Debug)]
pub struct Iter<'a> {
    queue: &'a TaskQueue,
    curr: Option<TaskId>,
}

type QueueMarker = bool;
const HOT: QueueMarker = true;
const COLD: QueueMarker = false;

impl TaskQueue {
    pub fn new(size: usize) -> Self {
        Self {
            inner: UnsafeCell::new(Inner::new(size)),
        }
    }

    /// Clear the map.
    ///
    /// # Safety
    ///
    /// Must only be called by `Executor`.
    pub unsafe fn clear(&self) {
        instrument!(compio_log::Level::DEBUG, "TaskQueue::clear");

        trace!("Clearing");

        unsafe {
            self.with_inner(|inner| {
                if inner.map.is_empty() {
                    trace!("Map empty, return");
                    return;
                }

                inner.hot.head = None;
                inner.hot.tail = None;
                inner.cold.head = None;
                inner.cold.tail = None;

                for task in inner.map.drain().filter_map(|(_, i)| i.task) {
                    trace!(?task, "Dropping task");

                    task.drop();
                    task.wait_for_scheduling();
                }

                debug_assert!(inner.map.is_empty());
            })
        }
    }

    pub fn has_hot(&self) -> bool {
        self.hot_head().is_some()
    }

    /// Take the task out for a poll: unlink it from the hot list and mark it
    /// [`Place::Running`].
    ///
    /// Unlinking and taking share the lookup of the item, which the two calls
    /// this replaced each paid for separately.
    pub fn start_run(&self, key: TaskId) -> Task {
        unsafe {
            self.with_inner(|inner| {
                inner.unlink::<HOT>(key);
                let item = inner.map.get_mut(key).expect("item exists");
                item.place = Place::Running { woken: false };
                item.task.take().expect("Task has already been taken")
            })
        }
    }

    /// Put a task that returned `Pending` back, into the hot list if it was
    /// woken during its poll and into the cold one otherwise.
    pub fn finish_run(&self, key: TaskId, task: Task) {
        unsafe {
            self.with_inner(|inner| {
                let item = inner.map.get_mut(key).expect("Invalid key");
                debug_assert!(item.task.is_none(), "Task was not taken");
                item.task = Some(task);

                let woken = match item.place {
                    Place::Running { woken } => woken,
                    place => unreachable!("Task was not running: {place:?}"),
                };

                if woken {
                    inner.link_tail::<HOT>(key);
                } else {
                    inner.link_tail::<COLD>(key);
                }
            })
        }
    }

    pub fn insert<F: Future + 'static>(
        &self,
        shared: NonNull<Shared>,
        tracker: SendWrapper<()>,
        future: F,
        meta: SpawnMeta,
    ) -> Task {
        unsafe {
            self.with_inner(|inner| {
                let mut ret = None;
                let key = inner.map.insert_with_key(|key| {
                    let [ptr, r] = Task::new::<F, 2>(key, shared, tracker, future, meta);
                    ret = Some(r);
                    Item {
                        prev: None,
                        next: None,
                        task: Some(ptr),
                        place: Place::Hot,
                    }
                });
                inner.link_tail::<HOT>(key);
                ret.take().expect("Task was not initialized")
            })
        }
    }

    pub fn make_hot(&self, key: TaskId) {
        unsafe { self.with_inner(|inner| inner.make_hot(key)) }
    }

    pub fn next_hot(&self, key: TaskId) -> Option<TaskId> {
        unsafe {
            self.with_inner(|inner| {
                inner.map.get(key).and_then(|item| {
                    debug_assert_eq!(item.place, Place::Hot);
                    item.next
                })
            })
        }
    }

    pub fn hot_head(&self) -> Option<TaskId> {
        unsafe { self.with_inner(|inner| inner.hot.head) }
    }

    pub fn iter_hot(&self) -> Iter<'_> {
        Iter {
            queue: self,
            curr: self.hot_head(),
        }
    }

    pub fn remove(&self, key: TaskId) -> Option<Task> {
        unsafe {
            self.with_inner(|inner| {
                match inner.map.get(key)?.place {
                    Place::Hot => inner.unlink::<HOT>(key),
                    Place::Cold => inner.unlink::<COLD>(key),
                    // `start_run` already unlinked it.
                    Place::Running { .. } => {}
                }

                inner.map.remove(key)?.task
            })
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that no concurrent access to the queue occurs
    /// while this reference is active.
    #[inline(always)]
    unsafe fn with_inner<R, F: FnOnce(&mut Inner) -> R>(&self, f: F) -> R {
        // SAFETY: Caller must ensure no concurrent access to the queue.
        self.inner.with_mut(|inner| f(unsafe { &mut *inner }))
    }
}

impl Inner {
    fn new(size: usize) -> Self {
        Self {
            map: SlotMap::with_capacity_and_key(size),
            hot: List::default(),
            cold: List::default(),
        }
    }

    /// Link a task to the end of a queue
    fn link_tail<const HOT: QueueMarker>(&mut self, key: TaskId) {
        let list = if HOT { &mut self.hot } else { &mut self.cold };
        let old_tail = list.tail;

        list.tail = Some(key);
        if list.head.is_none() {
            list.head = Some(key);
        }

        let item = self.map.get_mut(key).expect("item exists");
        item.prev = old_tail;
        item.next = None;
        item.place = if HOT { Place::Hot } else { Place::Cold };

        if let Some(tail_key) = old_tail
            && let Some(tail_item) = self.map.get_mut(tail_key)
        {
            tail_item.next = Some(key);
        }
    }

    /// Neighbour fix-up half of [`unlink`], for callers that already read
    /// `prev`/`next` and have rewritten the item's own links. Splitting it this
    /// way is what lets the wake path touch `key`'s slot only once.
    fn detach<const HOT: QueueMarker>(
        &mut self,
        key: TaskId,
        prev: Option<TaskId>,
        next: Option<TaskId>,
    ) {
        {
            let list = if HOT { &mut self.hot } else { &mut self.cold };
            if list.head == Some(key) {
                list.head = next;
            }
            if list.tail == Some(key) {
                list.tail = prev;
            }
        }
        if let Some(prev_key) = prev
            && let Some(prev_item) = self.map.get_mut(prev_key)
        {
            prev_item.next = next;
        }
        if let Some(next_key) = next
            && let Some(next_item) = self.map.get_mut(next_key)
        {
            next_item.prev = prev;
        }
    }

    /// Neighbour half of [`link_tail`], for callers that have already written
    /// the item's own links. `old_tail` must be the destination list's tail as
    /// read *before* the item was rewritten.
    fn attach_tail<const HOT: QueueMarker>(&mut self, key: TaskId, old_tail: Option<TaskId>) {
        if let Some(tail_key) = old_tail
            && let Some(tail_item) = self.map.get_mut(tail_key)
        {
            tail_item.next = Some(key);
        }
        let list = if HOT { &mut self.hot } else { &mut self.cold };
        list.tail = Some(key);
        if list.head.is_none() {
            list.head = Some(key);
        }
    }

    fn unlink<const HOT: QueueMarker>(&mut self, key: TaskId) {
        let list = if HOT { &mut self.hot } else { &mut self.cold };

        let (prev, next) = {
            let item = self.map.get(key).expect("item exists");
            debug_assert_eq!(item.place, if HOT { Place::Hot } else { Place::Cold });
            (item.prev, item.next)
        };

        if list.head == Some(key) {
            list.head = next;
        }
        if list.tail == Some(key) {
            list.tail = prev;
        }

        if let Some(prev_key) = prev
            && let Some(prev_item) = self.map.get_mut(prev_key)
        {
            prev_item.next = next;
        }
        if let Some(next_key) = next
            && let Some(next_item) = self.map.get_mut(next_key)
        {
            next_item.prev = prev;
        }
    }

    fn make_hot(&mut self, key: TaskId) {
        let new_tail = self.hot.tail;

        // One lookup of `key`: classify it, and if it really has to move, read
        // its old links and install the new ones in the same borrow.
        let (prev, next) = {
            let Some(item) = self.map.get_mut(key) else {
                return;
            };
            match item.place {
                // Already queued for a poll.
                Place::Hot => return,
                // Being polled right now: it is in neither list, so there is
                // nothing to move. Recording that it was woken is enough, and
                // `finish_run` then links it into the hot list instead of the
                // cold one.
                Place::Running { .. } => {
                    item.place = Place::Running { woken: true };
                    return;
                }
                Place::Cold => {}
            }
            let old = (item.prev, item.next);
            item.prev = new_tail;
            item.next = None;
            item.place = Place::Hot;
            old
        };

        self.detach::<COLD>(key, prev, next);
        self.attach_tail::<HOT>(key, new_tail);
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = TaskId;

    fn next(&mut self) -> Option<Self::Item> {
        let curr = self.curr?;
        self.curr = self.queue.next_hot(curr);
        Some(curr)
    }
}
