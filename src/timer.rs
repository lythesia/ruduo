use std::time::{SystemTime, UNIX_EPOCH};

use slab::Slab;

use crate::trace;

const EMPTY: usize = usize::MAX;

// T is marker(or key) of one Timeout entity
pub struct Timer<T> {
    // Size of each tick in ms (aka, tick unit)
    tick_ms: u64,
    // Slab of timeout entries (token, interval)
    entries: Slab<Entry<T>>,
    // Timeout wheel. Each tick, the timer will look at the next slot for
    // timeouts that match the current tick.
    wheel: Vec<usize>,
    // Tick 0's time in ms
    start_ms: u64,
    // The current tick
    tick: u64,
    // The next (key) entry to possibly timeout
    next: usize,
    // Masks the target tick to get the slot
    mask: u64,
}

impl<T> Timer<T> {
    pub fn new(tick_ms: u64, slots: usize, cap: usize) -> Timer<T> {
        let slots = slots.next_power_of_two();
        let cap = cap.next_power_of_two();
        assert!(cap < usize::MAX, "capacity cannot reach usize::max!");

        Timer {
            tick_ms,
            entries: Slab::with_capacity(cap),
            wheel: std::iter::repeat(EMPTY).take(slots).collect(),
            start_ms: 0,
            tick: 0,
            next: EMPTY,
            mask: (slots as u64) - 1,
        }
    }

    // call only once
    pub fn setup(&mut self) {
        self.start_ms = now_ms();
    }

    pub fn timeout_ms(&mut self, delay: u64, interval: u64, token: T) -> TimerResult {
        let at = now_ms() + std::cmp::max(0, delay);
        self.timeout_ms_at(at, interval, token)
    }

    pub fn timeout_ms_at(&mut self, at: u64, interval: u64, token: T) -> TimerResult {
        let at = at - self.start_ms;
        let mut tick = (at + self.tick_ms - 1) / self.tick_ms;
        // always advance 1 tick
        if tick <= self.tick {
            tick += 1;
        }

        self.insert(token, tick, interval)
    }

    // for cancel
    pub fn clear(&mut self, timeout: Timeout) -> bool {
        // sanity check
        if let Some(e) = self.entries.get(timeout.key) {
            if e.link.tick != timeout.tick {
                return false; // possible? which means you cannot remove timeout by arbitrary-made `timeout`
            }
        }

        match self.entries.try_remove(timeout.key) {
            Some(e) => {
                self.unlink(&e.link, timeout.key);
                true
            },
            _ => false,
        }
    }

    fn slot_for(&self, tick: u64) -> usize {
        (tick & self.mask) as usize // simply modulo
    }

    fn insert(&mut self, token: T, tick: u64, interval: u64) -> TimerResult {
        let slot = self.slot_for(tick); // curr pos in wheel
        let curr = self.wheel[slot];    // entry key of entries indicating by `slot`

        if self.entries.len() == self.entries.capacity() {
            return Err(TimerError::overflow());
        }

        // insert to entry with `curr` pos indicating to wheel
        let key = self.entries.insert(Entry::new(token, interval, tick, curr));
        // curr already has links
        if curr != EMPTY {
            self.entries[curr].link.prev = key;
        }
        // update wheel
        self.wheel[slot] = key;

        Ok(Timeout { key, tick })
    }

    // remove `link` of `key` entry in `entries`
    // link: {
    //   tick,
    //   prev,
    //   next,
    // }
    fn unlink(&mut self, link: &EntryLink, key: usize) {
        let slot = self.slot_for(link.tick);

        // `link` is first node of links at wheel[slot]
        // [slot] => `link` -> link_x -> link_y -> ..
        if link.prev == EMPTY {
            self.wheel[slot] = link.next;
        }
        // `link` is middle node
        // [slot] => .. -> link_x -> `link` -> link_y -> ..
        else {
            self.entries[link.prev].link.next = link.next;
        }

        // `link` is not last node
        if link.next != EMPTY {
            self.entries[link.next].link.prev = link.prev; // both ok for EMPTY or real prev node
            // `link` is timeout at next wheel slot
            if key == self.next {
                self.next = link.next;
            }
        }
        // `link` is last node AND is timeout at next wheel slot
        else if key == self.next {
            self.next = EMPTY;
        }
    }

    fn ms_to_tick(&self, ms: u64) -> u64 {
        (ms - self.start_ms) / self.tick_ms
    }

    pub fn now_tick(&self) -> u64 {
        self.ms_to_tick(now_ms())
    }

    pub fn tick_to(&mut self, now_tick: u64) -> Option<(T, u64)> {
        trace!("tick_to; now={}; tick={}", now_tick, self.tick);

        while self.tick <= now_tick {
            let curr = self.next; // key of next entry
            trace!("ticking({}/{}); curr={:?}", self.tick, now_tick, curr);

            if curr == EMPTY {
                self.tick += 1;
                self.next = self.wheel[self.slot_for(self.tick)];
            } else {
                let link = self.entries[curr].link;
                if link.tick <= self.tick {
                    // trigger current link
                    self.unlink(&link, curr);
                    // remove entry and return token
                    let entry = self.entries.remove(curr);
                    return Some((entry.token, entry.interval));
                } else {
                    // link not order by time, so need to iter all links(modulo %curr)
                    self.next = link.next;
                }
            }
        }
        None
    }
    // Number of ms remaining until the next tick
    pub fn next_tick_in_ms(&self) -> Option<u64> {
        if self.entries.len() == 0 {
            return None;
        }

        let now = now_ms();
        let nxt = self.start_ms + (self.tick + 1) * self.tick_ms;

        if nxt <= now {
            return Some(0);
        }

        Some(nxt - now)
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug)]
pub struct Timeout {
    pub key: usize, // key in slab
    pub tick: u64,  // for sanity check
}

pub type TimerResult = Result<Timeout, TimerError>;

#[derive(Debug)]
pub struct TimerError {
    kind: TimerErrorKind,
    desc: &'static str,
}

impl TimerError {
    fn overflow() -> TimerError {
        TimerError {
            kind: TimerErrorKind::TimerOverflow,
            desc: "too many timer entries"
        }
    }
}

#[derive(Debug)]
pub enum TimerErrorKind {
    TimerOverflow,
}

struct Entry<T> {
    token: T,
    interval: u64,
    link: EntryLink,
}
#[derive(Copy, Clone, Debug)]
struct EntryLink {
    tick: u64,
    prev: usize,
    next: usize,
}

impl<T> Entry<T> {
    fn new(token: T, interval: u64, tick: u64, next: usize) -> Entry<T> {
        Entry {
            token: token,
            interval,
            link: EntryLink {
                tick: tick,
                prev: EMPTY,
                next: next,
            },
        }
    }
}

fn now_ms() -> u64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    assert!(ms <= (u64::MAX as u128), "now_ms() overflow!");
    ms as u64
}

#[cfg(test)]
mod test {
    use super::Timer;

    const TICK: u64 = 100;
    const SLOTS: usize = 16;

    fn timer() -> Timer<&'static str> {
        Timer::new(TICK, SLOTS, 32)
    }

    #[test]
    pub fn test_timeout_next_tick() {
        let mut t = timer();
        let mut tick;

        t.timeout_ms_at(100, 0, "a").unwrap();

        tick = t.ms_to_tick(50);
        assert_eq!(None, t.tick_to(tick));

        tick = t.ms_to_tick(100);
        assert_eq!(Some(("a", 0)), t.tick_to(tick));
        assert_eq!(None, t.tick_to(tick));

        tick = t.ms_to_tick(150);
        assert_eq!(None, t.tick_to(tick));

        tick = t.ms_to_tick(200);
        assert_eq!(None, t.tick_to(tick));

        assert_eq!(t.count(), 0);
    }

    #[test]
    pub fn test_clearing_timeout() {
        let mut t = timer();
        let mut tick;

        let to = t.timeout_ms_at(100, 0, "a").unwrap();
        assert!(t.clear(to));

        tick = t.ms_to_tick(100);
        assert_eq!(None, t.tick_to(tick));

        tick = t.ms_to_tick(200);
        assert_eq!(None, t.tick_to(tick));

        assert_eq!(t.count(), 0);
    }

    #[test]
    pub fn test_multiple_timeouts_same_tick() {
        let mut t = timer();
        let mut tick;

        t.timeout_ms_at(100,0, "a").unwrap();
        t.timeout_ms_at(100, 0, "b").unwrap();

        let mut rcv = vec![];

        tick = t.ms_to_tick(100);
        rcv.push(t.tick_to(tick).unwrap());
        rcv.push(t.tick_to(tick).unwrap());

        assert_eq!(None, t.tick_to(tick));

        rcv.sort();
        assert!(rcv == [("a", 0), ("b", 0)], "actual={:?}", rcv);

        tick = t.ms_to_tick(200);
        assert_eq!(None, t.tick_to(tick));

        assert_eq!(t.count(), 0);
    }

    #[test]
    pub fn test_catching_up() {
        let mut t = timer();

        t.timeout_ms_at(110, 0, "a").unwrap();
        t.timeout_ms_at(220, 0, "b").unwrap();
        t.timeout_ms_at(230, 0, "c").unwrap();
        t.timeout_ms_at(440, 0, "d").unwrap();

        let tick = t.ms_to_tick(600);
        assert_eq!(Some(("a", 0)), t.tick_to(tick));
        assert_eq!(Some(("c", 0)), t.tick_to(tick));
        assert_eq!(Some(("b", 0)), t.tick_to(tick));
        assert_eq!(Some(("d", 0)), t.tick_to(tick));
        assert_eq!(None, t.tick_to(tick));
    }

    #[test]
    pub fn test_timeout_hash_collision() {
        let mut t = timer();
        let mut tick;

        t.timeout_ms_at(100, 0, "a").unwrap();
        t.timeout_ms_at(100 + TICK * SLOTS as u64, 0, "b").unwrap();

        tick = t.ms_to_tick(100);
        assert_eq!(Some(("a", 0)), t.tick_to(tick));
        assert_eq!(1, t.count());

        tick = t.ms_to_tick(200);
        assert_eq!(None, t.tick_to(tick));
        assert_eq!(1, t.count());

        tick = t.ms_to_tick(100 + TICK * SLOTS as u64);
        assert_eq!(Some(("b", 0)), t.tick_to(tick));
        assert_eq!(0, t.count());
    }

    #[test]
    pub fn test_clearing_timeout_between_triggers() {
        let mut t = timer();
        let mut tick;

        let a = t.timeout_ms_at(100, 0, "a").unwrap();
        let _ = t.timeout_ms_at(100, 0, "b").unwrap();
        let _ = t.timeout_ms_at(200, 0, "c").unwrap();

        tick = t.ms_to_tick(100);
        assert_eq!(Some(("b", 0)), t.tick_to(tick));
        assert_eq!(2, t.count());

        t.clear(a);
        assert_eq!(1, t.count());

        assert_eq!(None, t.tick_to(tick));

        tick = t.ms_to_tick(200);
        assert_eq!(Some(("c", 0)), t.tick_to(tick));
        assert_eq!(0, t.count());
    }
}