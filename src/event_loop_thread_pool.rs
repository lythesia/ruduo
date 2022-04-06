use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::event_loop::{EventLoop, Notifier, Handle};
use crate::{error, trace};

pub struct EventLoopThread<H: Handle + Send + 'static> {
    handle: Option<thread::JoinHandle<()>>,
    notifier: Notifier<H::AnyMsg>,
}

impl<H: Handle + Send + 'static> EventLoopThread<H> {
    pub fn spawn<F>(name: String, seq: usize, f: F) -> std::io::Result<Self>
    where F: Fn(usize, Notifier<H::AnyMsg>) -> H {
        let (mut el, notifier) = EventLoop::<H>::new()?;
        let mut handle = f(seq, notifier.clone());

        let h = thread::Builder::new()
            .name(name)
            .spawn(move || {
            trace!("thread spawned");
            if let Err(e) = el.run(&mut handle) {
                error!("failed to spawn EventLoopThread: {}", e);
            }
        }).unwrap();

        Ok(Self { handle: Some(h), notifier })
    }

    pub fn notifier(&self) -> Notifier<H::AnyMsg> {
        self.notifier.clone()
    }
}

impl<H: Handle + Send> Drop for EventLoopThread<H> {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            trace!("EventLoopThread dropped");
            self.notifier.dead_letter(); // it maybe already disconnected
            h.join().unwrap();
        }
    }
}

pub struct EventLoopThreadPool<H: Handle + Send + 'static> {
    pool: Vec<EventLoopThread<H>>,
    pool_name: String,
    size: usize,
    next: AtomicUsize,
}

impl<H: Handle + Send + 'static> Default for EventLoopThreadPool<H> {
    fn default() -> Self {
        Self {
            pool: vec![],
            pool_name: String::from("EventLoopThread"),
            size: 0,
            next: AtomicUsize::new(0),
        }
    }
}

impl <H: Handle + Send> EventLoopThreadPool<H> {
    pub fn new(size: usize, pool_name: String) -> Self {
        if size == 0 {
            panic!("empty pool not allowed!");
        }

        Self {
            pool: Vec::with_capacity(size),
            pool_name,
            size,
            next: AtomicUsize::new(0),
        }
    }

    pub fn set_pool_size(&mut self, size: usize) {
        assert!(self.pool.is_empty(), "pool already running!");
        self.size = size;
        self.pool = Vec::with_capacity(size);
    }

    pub fn start<F>(&mut self, f: F) -> std::io::Result<()>
    where F: Fn(usize, Notifier<H::AnyMsg>) -> H + Clone {
        for i in 0..self.size {
            let name = format!("{}-#{}", self.pool_name, i);
            let thr = EventLoopThread::spawn(name, i, f.clone())?;
            self.pool.push(thr);
        }
        Ok(())
    }

    pub fn next_loop(&self) -> Notifier<H::AnyMsg> {
        assert!(!self.pool.is_empty(), "pool is empty!");

        let n = self.next.fetch_add(1, Ordering::Relaxed) % self.size;

        self.pool[n].notifier()
    }
}