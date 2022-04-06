use std::{collections::HashMap, sync::{Arc, Mutex, Weak}, io::Write};

use async_logging::{AsyncLogger, Level};
use mio::{Token, net::TcpStream, Interest, Registry};
use ruduo::{tcp_connection::{TcpConnection, TcpConnMessage}, buffer::Buffer, event_loop::{Notifier, EventLoop, EventLoopCtrl}, tcp_server::TcpServer, service::Service, logger};

struct ServerInner {
    users: HashMap<Token, Notifier<TcpConnMessage<UserConnection>>>,
}
impl ServerInner {
    fn new() -> Self {
        Self { users: HashMap::new() }
    }
}

struct ChatServer {
    server: TcpServer<UserConnection, Self>,
    server_inner: Arc<Mutex<ServerInner>>,
    keeper: Notifier<()>,
}

impl ChatServer {
    fn new(addr: String, keeper: Notifier<()>) -> std::io::Result<Self> {
        let inner = Arc::new(Mutex::new(ServerInner::new()));

        Ok(Self {
            server: TcpServer::new(addr, 5, String::from("ChatServerThread"), 10,  keeper.clone())?,
            server_inner: inner,
            keeper,
        })
    }

    fn run(&mut self, event_loop: &mut EventLoop<Self>) -> std::io::Result<()> {
        self.server.start(event_loop.registry())?;
        event_loop.run(self)
    }
}

impl Service for ChatServer {
    type Message = ();
    type Connection = UserConnection;

    fn server(&self) -> &TcpServer<UserConnection, Self> {
        &self.server
    }

    fn connection_init_callback(&self) -> Box<dyn FnOnce(&mut Self::Connection) + Sync + Send> {
        let inner = self.server_inner.clone();
        Box::new(move |c| {
            c.server_inner = Arc::downgrade(&inner);
        })
    }

    fn handle_message(&mut self, _: Self::Message, _: &mut EventLoop<Self>) {
    }

    fn shutdown(&mut self, event_loop: &mut EventLoop<Self>) {
        self.server.shutdown(event_loop.registry());
    }
}

struct UserConnection {
    token: Token,
    stream: TcpStream,
    input: Buffer,
    output: Buffer,
    interests: Option<Interest>,
    notifier: Notifier<TcpConnMessage<Self>>,

    server_inner: Weak<Mutex<ServerInner>>,
}

impl UserConnection {
    fn broadcast(&mut self, data: Vec<u8>) {
        if let Some(server_inner) = self.server_inner.upgrade() {
            let h = &*server_inner.lock().unwrap();
            for (token, peer) in &h.users {
                if *token != self.token {
                    peer.notify(TcpConnMessage::Write(*token, data.clone()));
                }
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TimeoutQuit(Token, usize);

impl TcpConnection for UserConnection {
    type Timeout = TimeoutQuit;

    fn token(&self) -> Token {
        self.token
    }

    fn io(&self) -> &TcpStream {
        &self.stream
    }

    fn io_mut(&mut self) -> &mut TcpStream {
        &mut self.stream
    }

    fn input(&mut self) -> &mut Buffer {
        &mut self.input
    }

    fn output(&mut self) -> &mut Buffer {
        &mut self.output
    }

    fn interests(&mut self) -> &mut Option<Interest> {
        &mut self.interests
    }

    fn notifier(&self) -> Notifier<TcpConnMessage<Self>> {
        self.notifier.clone()
    }

    fn make(token: Token, stream: TcpStream, notifier: &Notifier<TcpConnMessage<Self>>) -> Self {
        Self {
            token,
            stream,
            input: Buffer::new(),
            output: Buffer::new(),
            interests: None,
            notifier: notifier.clone(),
            server_inner: Weak::new(),
        }
    }

    fn on_established(&mut self, ctrl: &mut EventLoopCtrl<Self::Timeout>) {
        self.enable_io(Interest::READABLE, ctrl.registry);
        let msg = format!("User({}) coming!\n", self.stream.peer_addr().unwrap()).as_bytes().to_vec();
        self.broadcast(msg);
        if let Some(server_inner) = self.server_inner.upgrade() {
            let h = &mut *server_inner.lock().unwrap();
            h.users.insert(self.token, self.notifier());
        }
        self.notifier.notify(TcpConnMessage::AddTimer(
            self.token,
            1000,
            TimeoutQuit(self.token, 0),
            Box::new(|c, _| {
                let msg = format!("heartbeat from {}\n", c.stream.peer_addr().unwrap()).as_bytes().to_vec();
                c.broadcast(msg);
            })
        ));
    }

    fn do_read(&mut self) -> std::io::Result<usize> {
        self.input.read_from(&mut self.stream)
    }

    fn do_write(&mut self) -> std::io::Result<usize> {
        let buf = self.output.peek();
        self.stream.write(buf)
    }

    fn do_write_with(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.stream.write(data)
    }

    fn on_read_complete(&mut self, _: &mut EventLoopCtrl<Self::Timeout>) {
        let msg = format!("User({}): {}\n", self.stream.peer_addr().unwrap(), self.input.retrieve_all_as_string());
        let data = msg.as_bytes().to_vec();
        // broadcast
        self.broadcast(data);
    }

    fn on_write_complete(&mut self, ctrl: &mut EventLoopCtrl<Self::Timeout>) {
        self.disable_io(Interest::WRITABLE, ctrl.registry);
    }

    fn on_close(&mut self) {
        let msg = format!("User({:?}) leaving!\n", self.stream.peer_addr().unwrap());
        self.broadcast(msg.as_bytes().to_vec());
    }
}

fn main() {
    let mut log = AsyncLogger::new(
        "/tmp/ruduo-chat".into(),
        4096,
        2
    ).unwrap();
    log.set_log_level(Level::Trace);

    logger::set_logger(log);
    logger::start();

    let (mut el, keeper) = EventLoop::new().unwrap();
    // let keeper2 = keeper.clone();
    // ctrlc::set_handler(move || {
    //     keeper2.dead_letter();
    // }).expect("failed to set ctrl-c handler");

    let mut server = ChatServer::new("127.0.0.1:8080".into(), keeper).unwrap();
    server.run(&mut el).unwrap();

    logger::stop();
}