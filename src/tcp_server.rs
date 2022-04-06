use std::{collections::HashMap, sync::{Arc, Mutex}, net::SocketAddr};
use std::fmt::Debug;

use mio::{Token, net::{TcpListener, TcpStream}, Interest, Registry};
use slab::Slab;

use crate::{tcp_connection::{TcpConnection, TcpConnMessage, BufState}, event_loop::{Notifier, Handle, EventLoop, EventLoopCtrl}, channel::Channel, event_loop_thread_pool::EventLoopThreadPool, debug, error, trace, timer::Timeout};

#[derive(Clone)]
struct ServerInner<M: Send + Debug> {
    shared: Arc<Mutex<Shared<M>>>,
}

impl<M: Send + Debug> ServerInner<M> {
    fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared::new())),
        }
    }
}

pub struct TcpServerConfig {
    pub pool_size: usize,
    pub pool_name: String,
    pub max_connections_per: usize,
}

pub(crate) const K_ACCEPT_TOKEN: Token = Token(0);
pub struct TcpServer<T, H>
where T: TcpConnection + 'static, H: Handle,
{
    config: TcpServerConfig,
    listener: TcpListener,
    pool: EventLoopThreadPool<Dispatcher<T>>,
    inner: ServerInner<<Dispatcher<T> as Handle>::AnyMsg>,
    notifier: Notifier<H::AnyMsg>,
}

impl<T, H> TcpServer<T, H>
where T: TcpConnection + 'static, H: Handle,
{
    pub fn new(addr: String, pool_size: usize, pool_name: String, max_connections_per: usize,
            notifier: Notifier<H::AnyMsg>) -> std::io::Result<Self> {
        let addr: SocketAddr = (&addr).parse().map_err(|_|
            std::io::Error::new(std::io::ErrorKind::InvalidInput,
                format!("invalid socket address: {}", addr)))?;
        
        let config = TcpServerConfig {
            pool_size,
            pool_name,
            max_connections_per,
        };
        
        Self::with_config(addr, config, notifier)
    }

    fn with_config(addr: SocketAddr, config: TcpServerConfig,
        notifier: Notifier<H::AnyMsg>) -> std::io::Result<Self> {
        let inner = ServerInner::new();
        debug!("listening at {}", addr);
        let listener = TcpListener::bind(addr)?;

        let size = config.pool_size;
        let pool_name = config.pool_name.clone();
        Ok(Self {
            config,
            listener,
            pool: EventLoopThreadPool::new(size, pool_name),
            inner,
            notifier,
        })
    }

    pub(crate) fn listener(&self) -> &TcpListener {
        &self.listener
    }

    pub(crate) fn get_pool_next(&self) -> Notifier<TcpConnMessage<T>> {
        self.pool.next_loop()
    }

    pub fn notifier(&self) -> &Notifier<H::AnyMsg> {
        &self.notifier
    }

    pub fn start(&mut self, registry: &Registry) -> std::io::Result<()> {
        let cap = self.config.max_connections_per;
        let shared = &self.inner.shared;

        self.pool.start(|seq, noti| {
            Dispatcher::new(cap, seq*cap, &noti, shared)
        })?;

        registry.register(&mut self.listener, K_ACCEPT_TOKEN, Interest::READABLE)
        .expect("failed to register listener");

        Ok(())
    }

    pub fn shutdown(&mut self, registry: &Registry) {
        registry.deregister(&mut self.listener).expect("failed to deregister listener");
        let shared = &mut *self.inner.shared.lock().unwrap();
        for noti in shared.conn_to_loop.values() {
            noti.dead_letter();
        }
    }
}

pub(crate) struct Dispatcher<T: TcpConnection> {
    local_connections: Slab<T>, // it can turn into dyn TcpConnection
    pendings: HashMap<Token, Box<dyn FnOnce(&mut T) + Sync + Send>>, // but what about this?
    // Box<dyn FnOnce(&mut dyn TcpConnection) + Sync + Send>
    // timers: Slab<(Timeout, Box<dyn FnOnce(&mut T, &Registry) + Send>)>,
    timers: HashMap<T::Timeout, (Timeout, Token, Box<dyn FnOnce(&mut T, &Registry) + Send>)>,
    timers2: HashMap<T::Timeout, (Timeout, Token, Box<dyn Fn(&mut T, &Registry) + Send>)>,
    offset: usize,
    local_notifier: Notifier<<Self as Handle>::AnyMsg>,
    shared: Arc<Mutex<Shared<<Self as Handle>::AnyMsg>>>, // shared is related with handle
}

unsafe impl<T> Send for Dispatcher<T> where T: TcpConnection {}

impl<T: TcpConnection> Dispatcher<T> {
    pub fn new(cap: usize, offset: usize, noti: &Notifier<<Self as Handle>::AnyMsg>, shared: &Arc<Mutex<Shared<<Self as Handle>::AnyMsg>>>) -> Self {
        Self {
            local_connections: Slab::with_capacity(cap),
            timers: HashMap::new(),
            timers2: HashMap::new(),
            pendings: HashMap::new(),
            offset,
            local_notifier: noti.clone(),
            shared: shared.clone(),
        }
    }

    fn restore_key(&self, token: Token) -> usize {
        token.0 - self.offset
    }

    fn add_connection(&mut self, stream: TcpStream, peer_addr: SocketAddr, ctrl: &mut EventLoopCtrl<T::Timeout>,
        cb: Box<dyn FnOnce(&mut T) + Sync + Send>) {
        if self.local_connections.len() == self.local_connections.capacity() {
            error!("max connections per loop reached! rejecting {}", peer_addr);
            stream.shutdown(std::net::Shutdown::Both).expect(&format!("failed to shutdown stream from {}", peer_addr));
        }

        debug!("connection with {} established", peer_addr);
        let entry = self.local_connections.vacant_entry(); // each local_conns should start from different token..
        let token = Token(self.offset + entry.key());
        let mut tcpconn = T::make(token, stream, &self.local_notifier);
        cb(&mut tcpconn);
        tcpconn.on_established(ctrl);
        entry.insert(tcpconn);

        {
            let shared = &mut *self.shared.lock().unwrap();
            shared.add_connection(token, self.local_notifier.clone());
        }
    }


    fn add_connecting(&mut self, stream: TcpStream, addr: SocketAddr, registry: &Registry,
        cb: Box<dyn FnOnce(&mut T) + Sync + Send>) {
        // mark1
        if self.local_connections.len() == self.local_connections.capacity() {
            error!("max connections per loop reached! stop connecting {}", addr);
            stream.shutdown(std::net::Shutdown::Both).expect(&format!("failed to shutdown stream connecting to {}", addr));
        }

        trace!("add connecting to {}", addr);
        let entry = self.local_connections.vacant_entry();       
        let token = Token(self.offset + entry.key());
        // register readiness
        let mut stream = stream;
        // mark2: what if connection became est between mark1&mark2?
        registry.register(&mut stream, token, Interest::READABLE).expect(&format!("failed to register connecting {:?}", token));
        let tcpconn = T::make(token, stream, &self.local_notifier);
        entry.insert(tcpconn);
        // mark as pending
        self.pendings.insert(token, cb); // call cb lately when really connected

        {
            let shared = &mut *self.shared.lock().unwrap();
            shared.add_connection(token, self.local_notifier.clone());
        }
    }

    fn remove_connection(&mut self, token: Token) {
        let tcpconn = self.local_connections.remove(self.restore_key(token));
        {
            let shared = &mut *self.shared.lock().unwrap();
            shared.remove_connection(token);
        }
        // todo: why unwrap may fail? (try to make peer_addr as field)
        // in connector thread, connectin already shutdown before on_close, so peer_addr() returns Err
        debug!("connection from {} disconnected", tcpconn.io().peer_addr().unwrap());
    }


    fn with_connection<F: FnOnce(&mut T)>(&mut self, token: Token, f: F) {
        if let Some(tcpconn) = self.local_connections.get_mut(self.restore_key(token)) {
            f(tcpconn);
        }
    }
}

impl<T> Handle for Dispatcher<T> where T: TcpConnection {
    type AnyMsg = TcpConnMessage<T>;
    type Timeout = T::Timeout;

    fn handle_io_event(&mut self, event: &mio::event::Event, event_loop: &mut EventLoop<Self>) {
        if let Some(tcpconn) = self.local_connections.get_mut(self.restore_key(event.token())) {
            if event.is_readable() {
                // todo: pull this to fn `connecting_to_connected`?
                if self.pendings.contains_key(&event.token()) {
                    // check peer_addr
                    match tcpconn.io().peer_addr() {
                        Ok(peer_addr) => {
                            debug!("connected to {}", peer_addr);
                            // invoke setup callback
                            let f = self.pendings.remove(&event.token()).unwrap();
                            f(tcpconn);
                            tcpconn.on_established(&mut event_loop.ctrl());
                        },
                        Err(e) if e.kind() == std::io::ErrorKind::NotConnected ||
                            e.raw_os_error() == Some(libc::EINPROGRESS) => {
                            // wait for next read event
                        },
                        Err(e) => {
                            error!("connecting err: {}", e);
                        }
                    }
                } else {
                    tcpconn.handle_read(&mut event_loop.ctrl());
                }
            }
            if event.is_writable() {
                tcpconn.handle_write(&mut event_loop.ctrl());
            }
        }
    }

    fn handle_notify(&mut self, msg: Self::AnyMsg, event_loop: &mut EventLoop<Self>) {
        match msg {
            TcpConnMessage::Established(stream, peer_addr, cb) => {
                self.add_connection(stream, peer_addr, &mut event_loop.ctrl(), cb);
            },
            TcpConnMessage::Connecting(stream, addr, cb) => {
                // give chance to check
                match stream.peer_addr() {
                    Ok(peer_addr) => {
                        self.add_connection(stream, peer_addr, &mut event_loop.ctrl(), cb);
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::NotConnected || e.raw_os_error() == Some(libc::EINPROGRESS) => {
                        self.add_connecting(stream, addr, event_loop.registry(), cb);
                    },
                    Err(e) => {
                        error!("connecting err: {}", e);
                        /* connector retry logic
                        event_loop.run_after(seconds, {..})
                        */
                    },
                }
            },
            TcpConnMessage::Removed(token) => {
                self.remove_connection(token);
            },
            TcpConnMessage::Write(token, data) => {
                if let Some(tcpconn) = self.local_connections.get_mut(self.restore_key(token)) {
                    let s = String::from_utf8_lossy(&data[..]);
                    debug!("conn(#{:?}) write: {}", token, s);
                    tcpconn.send(data.as_slice(), event_loop.registry());
                }
            },
            TcpConnMessage::WriteComplete(token) => {
                if let Some(tcpconn) = self.local_connections.get_mut(self.restore_key(token)) {
                    tcpconn.on_write_complete(&mut event_loop.ctrl());
                }
            },
            TcpConnMessage::Enable(token, mode) => {
                if let Some(tcpconn) = self.local_connections.get_mut(self.restore_key(token)) {
                    let registry = event_loop.registry();
                    tcpconn.enable_io(mode, registry);
                }
            },
            TcpConnMessage::Disable(token, mode) => {
                if let Some(tcpconn) = self.local_connections.get_mut(self.restore_key(token)) {
                    let registry = event_loop.registry();
                    tcpconn.disable_io(mode, registry);
                }
            },
            TcpConnMessage::Shutdown(token, how) => {
                self.with_connection(token, move |c| {
                    if !c.interests().is_writing() {
                        c.io().shutdown(how).expect("failed to shutdown stream");
                    }
                });
            },
            TcpConnMessage::SetContext(token, f) => {
                self.with_connection(token, move |c| {
                    f(c);
                });
            },
            TcpConnMessage::AddTimer(token, delay, mark, f) => {
                match event_loop.schedule_after(delay, mark.clone()) {
                    Ok(r) => {
                        let v = (r, token, f);
                        self.timers.insert(mark, v);
                    },
                    Err(e) => error!("failed to add timer: {:?}", e),
                }
            },
            TcpConnMessage::AddTimer2(token, delay, interval, mark, f) => {
                trace!("add timer: {:?}", token);
                match event_loop.schedule_every(delay, interval, mark.clone()) {
                    Ok(r) => {
                        let v = (r, token, f);
                        self.timers2.insert(mark, v);
                    },
                    Err(e) => error!("failed to add timer2: {:?}", e),
                }
            },
            TcpConnMessage::CancelTimer(token, mark) => {
                if let Some((timeout, _, _)) = self.timers.remove(&mark) {
                    event_loop.cancel(timeout);
                    trace!("Timer for Connection(#{:?}) canceled", token);
                }
                else if let Some((timeout, _, _)) = self.timers2.remove(&mark) {
                    event_loop.cancel(timeout);
                    trace!("Timer2 for Connection(#{:?}) canceled", token);
                }
            },
        }
    }

    // one timeout token == (one conn + one timer)
    // timeout => which conn
    fn handle_timeout(&mut self, mark: Self::Timeout, repeat: bool, event_loop: &mut EventLoop<Self>) {
        if !repeat {
            if let Some((_, token, f)) = self.timers.remove(&mark) {
            trace!("handle timer: {:?}", token);
                self.with_connection(token, |c| {
                    f(c, event_loop.registry());
                });
            }
        } else {
            if let Some((timeout, token, f)) = self.timers2.remove(&mark) {
                self.with_connection(token, |c| {
                    f(c, event_loop.registry());
                });
                self.timers2.insert(mark, (timeout, token, f));
            }
        }
    }

    fn refresh_timeout(&mut self, mark: Self::Timeout, timeout: Timeout) {
        if let Some((t, _, _)) = self.timers.get_mut(&mark) {
            *t = timeout;
        }
    }

    fn graceful_shutdown(&mut self, event_loop: &mut EventLoop<Self>) {
        let registry = event_loop.registry();
        for (_, tcpconn) in &mut self.local_connections {
            // deregister(no longer handling events)
            tcpconn.disable_io(Interest::READABLE|Interest::WRITABLE, registry);
            // tcpconn.handle_close(registry);
        }
    }
}

pub struct Shared<M: Send + Debug> {
    conn_to_loop: HashMap<Token, Notifier<M>>,
}

impl<M: Send + Debug> Shared<M> {
    pub fn new() -> Self {
        Self {
            conn_to_loop: HashMap::new(),
        }
    }

    pub fn get_loop_notifier(&self, token: Token) -> Option<&Notifier<M>> {
        self.conn_to_loop.get(&token)
    }

    pub fn add_connection(&mut self, token: Token, noti: Notifier<M>) {
        if self.conn_to_loop.contains_key(&token) {
            panic!("Connection(#{:?}) already dispatched!", token);
        }

        self.conn_to_loop.insert(token, noti);
    }

    pub fn remove_connection(&mut self, token: Token) -> Option<Notifier<M>> {
        self.conn_to_loop.remove(&token)
    }
}