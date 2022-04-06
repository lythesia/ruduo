use std::{sync::{Arc, Weak}, io::Write, net::Shutdown};

use async_logging::{AsyncLogger, Level};
use mio::{Token, net::TcpStream, Interest, Registry};
use ruduo::{tcp_connection::{TcpConnection, TcpConnMessage}, event_loop::{Notifier, EventLoop}, connector::ConnectorService, buffer::Buffer, error, service::Service, tcp_server::TcpServer, debug, logger};

/*
pub struct ServerConnection {
    token: Token,
    stream: TcpStream,
    input: Buffer,
    output: Buffer,
    interests: Option<Interest>,
    notifier: Notifier<TcpConnMessage<Self>>,

    connector: Weak<ConnectorService<ConnectorConnection>>,
    context: Option<Context<ConnectorConnection>>,
}

impl TcpConnection for ServerConnection {
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
            connector: Weak::new(), // when to set connector
            context: None,
        }
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

    fn on_established(&mut self, registry: &Registry) {
        self.enable_io(Interest::READABLE, registry);
    }

    fn on_read_complete(&mut self, registry: &Registry) {
        // already connected
        if let Some(ctx) = self.context.as_ref() {
            // send req to connector_conn
            let msg = self.input.retrieve_all_as_string();
            debug!("@sv recv msg: {}", msg);
            let data = msg.as_bytes().to_vec();
            debug!("@sv send to cc(#{:?})", ctx.token);
            ctx.notifier.notify(TcpConnMessage::Write(ctx.token, data));
        }
        // to connect
        else {
            // parse addr (assume)
            let req_addr = self.input.retrieve_all_as_string();
            // invoke connector_service::connect with setup callback
            let token = self.token;
            let notifier = self.notifier.clone();
            let f: Box<dyn FnOnce(&mut ConnectorConnection) + Sync + Send>  = Box::new(move |c| {
                c.context = Some(Context { token, notifier, });
            });
            // Connecting = connect();  // registered to reactor(but we should add it to dispatcher at advance, or we'll lose event)
                                        // at least we should add to dispatcher at current iteration..
                                        // and it may become connected at any time after calling ::connect()
                                        // so at entry of dealing Connecting, we should check peer_addr again
            // Connecting -> Rc<RefCell<..>>, then self.context = Some(Rc::clone(..));
            // add to local dispatcher, how?
            // 1. change param registry to event_loop + Handle, so we can use Handle's method, that requires TcpConnection<H: Handle>
            // 2. each server_conn holds one connector, each connector holds ref to dispatcher(how?) emmm
            // 3. tunnel map => tunnel holds connector => connector::connect => notify local dispatch with Connecting(..)
            if let Some(cc) = self.connector.upgrade() {
                let addr = req_addr.trim();
                debug!("connecting {}", addr);
                // if we reuse registry here, then connector_conn may register to same reactor,
                // does that matter? or maybe not, but tunnel server as Handle deals ONLY
                // one kind of TcpConnection(ServerConnection)
                // or we should make local_connections not Slab<T> but Slab<Box<dyn T>>? if so, we don't need slab at all..
                // so that we can mix two kinds of T: TcpConnection
                if let Err(e) = cc.connect(req_addr.trim(), registry, f) {
                    error!("connecting {} err: {}", req_addr, e);
                }
            } else {
                error!("connector not set!");
            }
        }
    }

    fn on_write_complete(&mut self, registry: &Registry) {
    }

    fn on_close(&mut self) {
        debug!("server connection #({:?}) closing(shutdown connector_conn first)", self.token);
        // close corresponding connector_conn
        if let Some(ctx) = self.context.take() {
            ctx.notifier.notify(TcpConnMessage::Shutdown(ctx.token, Shutdown::Write));
        }
    }
}

struct Context<T: TcpConnection> {
    token: Token,
    notifier: Notifier<TcpConnMessage<T>>,
}

pub struct ConnectorConnection {
    token: Token,
    stream: TcpStream,
    input: Buffer,
    output: Buffer,
    interests: Option<Interest>,
    notifier: Notifier<TcpConnMessage<Self>>,

    context: Option<Context<ServerConnection>>,
}

impl ConnectorConnection {
}

impl TcpConnection for ConnectorConnection {
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

    fn interests(&mut self) -> &mut Option<mio::Interest> {
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
            context: None,
        }
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

    // if we share same reactor here, we can use rc<refcell<>> to operate server_conn?
    // ctx: Weak<RefCell<..>>, ctx.upgrade().borrow_mut() => &mut ServerConn then ServerConn.context = Rc::clone(self?) and enable_io
    fn on_established(&mut self, registry: &mio::Registry) {
        self.enable_io(Interest::READABLE, registry);
        if let Some(ctx) = self.context.as_ref() {
            // setup server_conn's context
            let token = self.token;
            let notifier = self.notifier.clone();
            let f: Box<dyn FnOnce(&mut ServerConnection) + Sync + Send> = Box::new(move |s| {
                s.context = Some(Context { token, notifier, });
            });
            ctx.notifier.notify(TcpConnMessage::SetContext(ctx.token, f));
            // enable server_conn's reading
            ctx.notifier.notify(TcpConnMessage::Enable(ctx.token, Interest::READABLE));
        }
    }

    fn on_read_complete(&mut self, registry: &mio::Registry) {
        let data = self.input.peek();
        debug!("@cc recv msg: {}", String::from_utf8_lossy(data));
        // write back to server_conn
        if let Some(ctx) = self.context.as_ref() {
            debug!("@cc send to sv(#{:?})", ctx.token);
            ctx.notifier.notify(TcpConnMessage::Write(ctx.token, data.to_vec()));
        } else {
            error!("context not set for Connector Connection(#{:?})", self.token);
        }
        self.input.retrieve_all(); // clear any way if no ctx..
    }

    fn on_write_complete(&mut self, registry: &mio::Registry) {
    }

    fn on_close(&mut self) {
        debug!("connector connection #({:?}) closing", self.token);
    }
}

struct TunnelServer {
    server: TcpServer<ServerConnection, Self>,
    connector: Arc<ConnectorService<ConnectorConnection>>,
    keeper: Notifier<()>,
}

impl TunnelServer {
    fn new(addr: String, keeper: Notifier<()>) -> std::io::Result<Self> {
        Ok(Self {
            server: TcpServer::new(addr, 5, String::from("TunnelServerThread"), 10, keeper.clone())?,
            connector: Arc::new(ConnectorService::new(5, 10)),
            keeper,
        })
    }

    fn run(&mut self, event_loop: &mut EventLoop<Self>) -> std::io::Result<()> {
        // start connector
        if let Some(cc) = Arc::get_mut(&mut self.connector) {
            cc.start()?;
        } else {
            panic!("who holds connector?!");
        }
        // start server
        self.server.start(event_loop.registry())?;
        // start loop
        event_loop.run(self)
    }
}

impl Service for TunnelServer {
    type Message = ();

    type Connection = ServerConnection;

    fn server(&self) -> &TcpServer<Self::Connection, Self> {
        &self.server
    }

    fn handle_message(&mut self, msg: Self::Message, event_loop: &mut EventLoop<Self>) {
    }

    fn connection_init_callback(&self) -> Box<dyn FnOnce(&mut Self::Connection) + Sync + Send> {
        // setup connector
        let cc = self.connector.clone();
        Box::new(move |c| {
            c.connector = Arc::downgrade(&cc);
        })
    }

    fn shutdown(&mut self, event_loop: &mut EventLoop<Self>) {
        self.server.shutdown(event_loop.registry());
    }
}
*/

fn main() {
    let mut log = AsyncLogger::new(
        "/tmp/ruduo-tunnel".into(),
        4096,
        2
    ).unwrap();
    log.set_log_level(Level::Trace);

    logger::set_logger(log);
    logger::start();

    // let (mut el, keeper) = EventLoop::new().unwrap();

    // let mut server = TunnelServer::new("127.0.0.1:8081".into(), keeper).unwrap();
    // server.run(&mut el).unwrap();

    logger::stop();
}