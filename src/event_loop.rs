use std::{time::Duration, fmt::Debug};
use std::sync::Arc;

use crossbeam_channel::{Sender, Receiver, TryRecvError};
use mio::Registry;
use mio::{Poll, Events, event::Event, Token, Waker};

use crate::timer::{Timer, Timeout, TimerResult};
use crate::{error, trace};

pub struct EventLoopConfig {
    poll_timeout: Option<Duration>,
    poll_max_events: usize,
    timer_tick_ms: u64,
    timer_wheel_size: usize,
    timer_cap: usize,
}

const K_POLL_TIMEOUT: u64 = 10000;
const K_POLL_MAXEVENTS: usize = 1024;
const K_TIMER_TICK_MS: u64 = 100;
const K_TIMER_WHEEL_SIZE: usize = 1024;
const K_TIMER_CAP: usize = 65536;
impl Default for EventLoopConfig {
    fn default() -> Self {
        EventLoopConfig {
            poll_timeout: Some(Duration::from_millis(K_POLL_TIMEOUT)),
            poll_max_events: K_POLL_MAXEVENTS,
            timer_tick_ms: K_TIMER_TICK_MS,
            timer_wheel_size: K_TIMER_WHEEL_SIZE,
            timer_cap: K_TIMER_CAP,
        }
    }
}

/// Handle holds state tracking Channels
pub trait Handle: Sized {
    type AnyMsg: Debug + Send;
    type Timeout: Clone + Send;
    // i.e.
    // CHAN_SEND(token, data)
    // CONNECT(addr)
    // LISTEN(addr)
    // STOP
    // ..

    fn handle_io_event(&mut self, event: &Event, event_loop: &mut EventLoop<Self>);
    fn handle_notify(&mut self, msg: Self::AnyMsg, event_loop: &mut EventLoop<Self>);
    fn refresh_timeout(&mut self, mark: Self::Timeout, timeout: Timeout);
    fn handle_timeout(&mut self, mark: Self::Timeout, repeat: bool, event_loop: &mut EventLoop<Self>);
    fn graceful_shutdown(&mut self, event_loop: &mut EventLoop<Self>);
}

const K_WAKE_TOKEN: Token = Token(usize::MAX);
pub struct EventLoop<H: Handle> {
    config: EventLoopConfig,
    poll: Poll,
    timer: Timer<H::Timeout>,
    rx: Receiver<Message<H::AnyMsg>>,
    running: bool,
}

pub struct EventLoopCtrl<'a, T: Clone + Send> {
    pub registry: &'a Registry,
    pub timer: &'a mut Timer<T>,
}

impl<H: Handle> EventLoop<H> {
    pub fn new() -> std::io::Result<(Self, Notifier<H::AnyMsg>)> {
        let config = EventLoopConfig::default();
        Self::new_configured(config)
    }

    pub fn new_configured(config: EventLoopConfig) -> std::io::Result<(Self, Notifier<H::AnyMsg>)> {
        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), K_WAKE_TOKEN)?);
        let mut timer = Timer::new(config.timer_tick_ms, config.timer_wheel_size, config.timer_cap);
        let (tx, rx) = crossbeam_channel::unbounded();
        let notifier = Notifier { waker, tx };

        timer.setup();
        Ok((Self {
            config,
            poll,
            timer,
            rx,
            running: false,
        }, notifier))
    }

    pub fn ctrl(&mut self) -> EventLoopCtrl<H::Timeout> {
        EventLoopCtrl {
            registry: self.poll.registry(),
            timer: &mut self.timer,
        }
    }

    pub fn registry(&self) -> &Registry {
        self.poll.registry()
    }

    fn process_timers(&mut self, handle: &mut H) {
        let now_tick = self.timer.now_tick();
        loop {
            match self.timer.tick_to(now_tick) {
                Some((mark, interval)) =>  {
                    let repeat = interval > 0;
                    if repeat {
                        let timeout = self.schedule_after(interval, mark.clone()).unwrap();
                        // update timeout (repeat task)
                        handle.refresh_timeout(mark.clone(), timeout);
                    }
                    handle.handle_timeout(mark, repeat, self);
                },
                _ => {
                    trace!("no timer..");
                    return;
                },
            }
        }
    }

    fn step(&mut self, events: &mut Events, handle: &mut H) -> std::io::Result<()> {
        let next_tick = self.timer.next_tick_in_ms()
            .map(|ms| Duration::from_millis(std::cmp::min(ms, usize::MAX as u64)));

        let timeout = if self.rx.is_empty() {
            self.config.poll_timeout
        } else {
            Some(Duration::from_millis(0))
        };

        let timeout = match (timeout, next_tick) {
            (Some(a), Some(b)) => Some(std::cmp::min(a, b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            _ => None,
        };

        self.poll.poll(events, timeout)?;

        // trace!("drain io events..");
        for event in events.iter() {
            match event.token() {
                K_WAKE_TOKEN => { trace!("EventLoop wakeup"); },
                _ => handle.handle_io_event(event, self), // handle_error?
            }
        }

        // trace!("drain messages..");
        loop {
            match self.rx.try_recv() {
                Ok(msg) => {
                    match msg {
                        Message::Stop => {
                            handle.graceful_shutdown(self);
                            break;
                        }
                        Message::Any(msg) => handle.handle_notify(msg, self),
                    }
                },
                Err(e) => match e {
                    TryRecvError::Empty => break,
                    TryRecvError::Disconnected => {
                        // notifier dropped, end it
                        trace!("EventLoop Notifier broken, shutting down..");
                        self.shutdown();
                        break;
                    },
                },
            }
        }

        // trace!("pop timers..");
        self.process_timers(handle);

        Ok(())
    }

    pub fn run(&mut self, handle: &mut H) -> std::io::Result<()> {
        self.running = true;

        let mut events = Events::with_capacity(self.config.poll_max_events);
        while self.running {
            self.step(&mut events, handle)?;
        }

        trace!("EventLoop stop."); // logging
        Ok(())
    }

    // what if H::Timeout is closure: FnOnce(&mut Handle, &mut event_loop) + Send + Clone
    // how to get conn from H::Timeout? must add Token
    pub fn schedule_after(&mut self, delay_ms: u64, token: H::Timeout) -> TimerResult {
        self.timer.timeout_ms(delay_ms, 0, token)
    }

    pub fn schedule_at(&mut self, at_ms: u64, token: H::Timeout) -> TimerResult {
        self.timer.timeout_ms_at(at_ms, 0, token)
    }

    pub fn schedule_every(&mut self, delay_ms: u64, interval_ms: u64, token: H::Timeout) -> TimerResult {
        self.timer.timeout_ms(delay_ms, interval_ms, token)
    }

    pub fn cancel(&mut self, timeout: Timeout) -> bool {
        self.timer.clear(timeout)
    }

    // if so, it's only allowed to shutdown in loop thread, i.e: in handle's operations..
    pub fn shutdown(&mut self) {
        self.running = false;
    }
}

#[derive(Debug)]
pub enum Message<M> {
    Stop,
    Any(M),
}

pub struct Notifier<M: Send + Debug>  {
    // give sth like loop ID?
    waker: Arc<Waker>,
    tx: Sender<Message<M>>,
}
impl<M: Send + Debug> Clone for Notifier<M> {
    fn clone(&self) -> Self {
        Self { waker: self.waker.clone(), tx: self.tx.clone() }
    }
}

impl<M: Send + Debug> Notifier<M> {
    pub fn wake(&self) {
        if let Err(e) = self.waker.wake() {
            panic!("failed to wake EventLoop: {}", e);
        }
    }

    pub fn notify(&self, msg: M) {
        if let Err(e) = self.tx.send(Message::Any(msg)) {
            panic!("failed to notify EventLoop with message: {:?}", e.0);
        }
        self.wake();
    }

    pub fn dead_letter(&self) {
        if let Err(_) = self.tx.send(Message::Stop) {
            error!("failed to send dead letter to EventLoop");
        }
        self.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    struct H;
    impl Handle for H {
        type AnyMsg = String;
        type Timeout = usize;
        fn handle_io_event(&mut self, _: &Event, _: &mut EventLoop<Self>) {}
        fn handle_notify(&mut self, msg: Self::AnyMsg, event_loop: &mut EventLoop<Self>) {
            if msg == "STOP".to_string() {
                event_loop.shutdown();
            }
        }
        fn handle_timeout(&mut self, _: Self::Timeout, _: bool, _: &mut EventLoop<Self>) {}
        fn refresh_timeout(&mut self, mark: Self::Timeout, timeout: Timeout) {}
        fn graceful_shutdown(&mut self, event_loop: &mut EventLoop<Self>) {
            event_loop.shutdown();
        }
    }

    #[test]
    fn test_wake() {
        let (mut el, notifier) = EventLoop::<H>::new().unwrap();
        let n = notifier.clone();
        let h = thread::spawn(move || {
            thread::sleep(Duration::from_secs(2));
            n.wake();
            // n cannot be dropped here..
            thread::sleep(Duration::from_secs(2));
            // n drops..
        });
        drop(notifier); // though we drop waker here, still we can wakeup loop
        println!("drop outer waker");
        el.run(&mut H).unwrap();

        h.join().unwrap();
    }

    #[test]
    fn test_wake_and_stop() {

        let (mut el, notifier) = EventLoop::<H>::new().unwrap();
        let n = notifier.clone();
        let h = thread::spawn(move || {
            thread::sleep(Duration::from_secs(2));
            n.notify(String::from("STOP"));
            // notifier cannot be dropped here..
        });
        el.run(&mut H).unwrap();

        h.join().unwrap();
    }
}