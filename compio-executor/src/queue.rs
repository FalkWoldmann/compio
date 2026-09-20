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
    is_hot: bool,
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

    pub fn take(&self, key: TaskId) -> Option<Task> {
        unsafe {
            self.with_inner(|inner| {
                inner
                    .map
                    .get_mut(key)
                    .map(|item| item.task.take().expect("Task has already been taken"))
            })
        }
    }

    pub fn reset(&self, key: TaskId, task: Task) {
        unsafe {
            self.with_inner(|inner| {
                let place = inner.map.get_mut(key).expect("Invalid key");
                debug_assert!(place.task.is_none(), "Task was not taken");
                place.task = Some(task);
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
                        is_hot: true,
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

    pub fn make_cold(&self, key: TaskId) {
        unsafe { self.with_inner(|inner| inner.make_cold(key)) }
    }

    pub fn next_hot(&self, key: TaskId) -> Option<TaskId> {
        unsafe {
            self.with_inner(|inner| {
                inner.map.get(key).and_then(|item| {
                    debug_assert!(item.is_hot);
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
                let is_hot = inner.map.get(key)?.is_hot;

                if is_hot {
                    inner.unlink::<HOT>(key);
                } else {
                    inner.unlink::<COLD>(key);
                };

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
        item.is_hot = HOT;

        if let Some(tail_key) = old_tail
            && let Some(tail_item) = self.map.get_mut(tail_key)
        {
            tail_item.next = Some(key);
        }
    }

    fn unlink<const HOT: QueueMarker>(&mut self, key: TaskId) {
        let list = if HOT { &mut self.hot } else { &mut self.cold };

        let (prev, next) = {
            let item = self.map.get(key).expect("item exists");
            debug_assert_eq!(item.is_hot, HOT);
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

    /// Move a task to the tail of the other queue.
    ///
    /// This is the per-wake hot path, so it touches `key`'s slot exactly once:
    /// the old links are read and the new ones written under a single
    /// `get_mut`, instead of the `get` + `unlink` + `link_tail` sequence
    /// looking the same key up three times. Neighbours still need their own
    /// lookups because `SlotMap` hands out one mutable borrow at a time.
    fn relink<const TO_HOT: QueueMarker>(&mut self, key: TaskId) {
        let new_tail = if TO_HOT {
            self.hot.tail
        } else {
            self.cold.tail
        };

        // `key` is in the source list and `new_tail` in the destination, so
        // they can never alias.
        let (prev, next) = {
            let Some(item) = self.map.get_mut(key) else {
                return;
            };
            if item.is_hot == TO_HOT {
                return;
            }
            let old = (item.prev, item.next);
            item.prev = new_tail;
            item.next = None;
            item.is_hot = TO_HOT;
            old
        };

        // Detach from the source list.
        let from = if TO_HOT {
            &mut self.cold
        } else {
            &mut self.hot
        };
        if from.head == Some(key) {
            from.head = next;
        }
        if from.tail == Some(key) {
            from.tail = prev;
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

        // Attach to the tail of the destination list.
        if let Some(tail_key) = new_tail
            && let Some(tail_item) = self.map.get_mut(tail_key)
        {
            tail_item.next = Some(key);
        }
        let to = if TO_HOT {
            &mut self.hot
        } else {
            &mut self.cold
        };
        to.tail = Some(key);
        if to.head.is_none() {
            to.head = Some(key);
        }
    }

    fn make_hot(&mut self, key: TaskId) {
        self.relink::<HOT>(key);
    }

    fn make_cold(&mut self, key: TaskId) {
        self.relink::<COLD>(key);
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

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    /// Insert a link-only item. The list logic never touches `task`, so `None`
    /// is enough to exercise it without constructing a real future.
    fn push_cold(inner: &mut Inner) -> TaskId {
        let key = inner.map.insert(Item {
            prev: None,
            next: None,
            task: None,
            is_hot: false,
        });
        inner.link_tail::<COLD>(key);
        key
    }

    /// Walk a list front-to-back, checking that `prev`/`next` are mutual, that
    /// every entry carries the expected `is_hot`, and that it terminates.
    fn walk(inner: &Inner, hot: bool) -> Vec<TaskId> {
        let list = if hot { &inner.hot } else { &inner.cold };
        let mut seen = Vec::new();
        let mut curr = list.head;
        let mut prev = None;

        while let Some(key) = curr {
            assert!(
                !seen.contains(&key),
                "cycle in {} list at {key:?}",
                if hot { "hot" } else { "cold" }
            );
            let item = inner.map.get(key).expect("linked key must be in the map");
            assert_eq!(item.is_hot, hot, "{key:?} linked into the wrong list");
            assert_eq!(item.prev, prev, "{key:?} has a broken prev link");
            seen.push(key);
            prev = Some(key);
            curr = item.next;
        }

        assert_eq!(list.tail, prev, "tail does not match the last walked node");
        if list.head.is_none() {
            assert!(list.tail.is_none(), "empty list must have no tail");
        }
        seen
    }

    /// Every item in the map belongs to exactly one list, and the two walks
    /// together account for the whole map.
    fn check(inner: &Inner) -> (Vec<TaskId>, Vec<TaskId>) {
        let hot = walk(inner, true);
        let cold = walk(inner, false);
        assert_eq!(
            hot.len() + cold.len(),
            inner.map.len(),
            "every map entry must be linked into exactly one list"
        );
        for key in &hot {
            assert!(!cold.contains(key), "{key:?} is in both lists");
        }
        (hot, cold)
    }

    #[test]
    fn link_tail_preserves_order() {
        let mut inner = Inner::new(4);
        let keys: Vec<_> = (0..4).map(|_| push_cold(&mut inner)).collect();

        let (hot, cold) = check(&inner);
        assert!(hot.is_empty());
        assert_eq!(cold, keys, "cold list must preserve insertion order");
    }

    #[test]
    fn make_hot_moves_between_lists() {
        let mut inner = Inner::new(3);
        let keys: Vec<_> = (0..3).map(|_| push_cold(&mut inner)).collect();

        // Move the middle one first: exercises unlink with both neighbours.
        inner.make_hot(keys[1]);
        let (hot, cold) = check(&inner);
        assert_eq!(hot, vec![keys[1]]);
        assert_eq!(cold, vec![keys[0], keys[2]]);

        // Head, then tail.
        inner.make_hot(keys[0]);
        inner.make_hot(keys[2]);
        let (hot, cold) = check(&inner);
        assert_eq!(hot, vec![keys[1], keys[0], keys[2]]);
        assert!(cold.is_empty());
    }

    #[test]
    fn make_hot_is_idempotent() {
        let mut inner = Inner::new(2);
        let a = push_cold(&mut inner);
        let b = push_cold(&mut inner);

        inner.make_hot(a);
        let before = check(&inner);
        inner.make_hot(a);
        assert_eq!(check(&inner), before, "re-heating must not relink");
        assert_eq!(walk(&inner, false), vec![b]);
    }

    #[test]
    fn make_cold_returns_to_the_back() {
        let mut inner = Inner::new(3);
        let keys: Vec<_> = (0..3).map(|_| push_cold(&mut inner)).collect();
        for k in &keys {
            inner.make_hot(*k);
        }
        assert_eq!(walk(&inner, true), keys);

        inner.make_cold(keys[0]);
        let (hot, cold) = check(&inner);
        assert_eq!(hot, vec![keys[1], keys[2]]);
        assert_eq!(cold, vec![keys[0]]);
    }

    #[test]
    fn round_trips_keep_the_lists_consistent() {
        let mut inner = Inner::new(6);
        let keys: Vec<_> = (0..6).map(|_| push_cold(&mut inner)).collect();

        // A deterministic but irregular interleaving of moves.
        for round in 0..4 {
            for (i, k) in keys.iter().enumerate() {
                if (i + round) % 3 == 0 {
                    inner.make_hot(*k);
                } else if inner.map[*k].is_hot {
                    inner.make_cold(*k);
                }
                check(&inner);
            }
        }

        let (hot, cold) = check(&inner);
        assert_eq!(hot.len() + cold.len(), 6);
    }

    #[test]
    fn single_element_list_head_and_tail_agree() {
        let mut inner = Inner::new(1);
        let a = push_cold(&mut inner);
        assert_eq!(inner.cold.head, Some(a));
        assert_eq!(inner.cold.tail, Some(a));

        inner.make_hot(a);
        check(&inner);
        assert_eq!(inner.cold.head, None);
        assert_eq!(inner.cold.tail, None);
        assert_eq!(inner.hot.head, Some(a));
        assert_eq!(inner.hot.tail, Some(a));
    }
}
