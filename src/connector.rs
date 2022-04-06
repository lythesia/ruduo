use std::{net::SocketAddr, sync::{Arc, Mutex}};

use mio::{net::TcpStream, Registry};

use crate::{tcp_connection::{TcpConnection, TcpConnMessage}, event_loop::Handle, event_loop_thread_pool::EventLoopThreadPool, tcp_server::{Dispatcher, TcpServerConfig, Shared}, debug};

/*
Call TcpStream::connect
Register the returned stream with at least read interest.
Wait for a (readable) event.
Check TcpStream::peer_addr. If it returns libc::EINPROGRESS or ErrorKind::NotConnected
it means the stream is not yet connected, go back to step 3.
If it returns an address it means the stream is connected, go to step 5.
If another error is returned something whent wrong.
Now the stream can be used.
*/

// is connector's connection need to access others(akka, needs `inner`)?
pub struct ConnectorService<T>
where T: TcpConnection + 'static {
    config: TcpServerConfig,
    pool: EventLoopThreadPool<Dispatcher<T>>,
    shared_inner: Arc<Mutex<Shared<<Dispatcher<T> as Handle>::AnyMsg>>>,
}

impl<T> ConnectorService<T>
where T: TcpConnection + 'static {
    pub fn new(pool_size: usize, max_connections_per: usize) -> Self {
        let pool_name = String::from("ConnectorThread");
        let config = TcpServerConfig {
            pool_size,
            pool_name: pool_name.clone(), 
            max_connections_per,
        };

        Self {
            config,
            pool: EventLoopThreadPool::new(pool_size, pool_name),
            shared_inner: Arc::new(Mutex::new(Shared::new())),
        }
    }

    pub fn start(&mut self) -> std::io::Result<()> {
        let cap = self.config.max_connections_per;
        let shared = &self.shared_inner;

        self.pool.start(|seq, noti| {
            Dispatcher::new(cap, seq*cap, &noti, shared)
        })?;

        Ok(())
    }

    pub fn connect(&self, addr: &str, registry: &Registry,
        f: Box<dyn FnOnce(&mut T) + Sync + Send>) -> std::io::Result<()> {
        let addr: SocketAddr = addr.parse().map_err(|_|
            std::io::Error::new(std::io::ErrorKind::InvalidInput,
                format!("invalid socket address: {}", addr)))?;

        let stream = TcpStream::connect(addr.clone())?;

        let n = self.pool.next_loop();
        // FIXME: try to register stream at once stream is returned, so that we
        // wont lose readiness event, so it should be:
        // let stream = ..;
        // let mut conn = construct(stream);
        // conn.enable_io(readable, registry);
        // add to local dispatcher (thread_local?)
        n.notify(TcpConnMessage::Connecting(stream, addr, f));
        // match stream.peer_addr() {
        //     // it should not be so fast..
        //     Ok(peer_addr) => {
        //         debug!("connected to {}", peer_addr);
        //         n.notify(TcpConnMessage::Established(stream, peer_addr, f));
        //     },
        //     Err(e) if e.kind() == std::io::ErrorKind::NotConnected || e.raw_os_error() == Some(libc::EINPROGRESS) => {
        //         debug!("try peer_name() to {} next round", addr);
        //     },
        //     Err(e) => return Err(e), // todo: retry
        // }

        Ok(())
    }
}