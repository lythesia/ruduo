use std::fmt::Debug;

use crate::{event_loop::{Handle, EventLoop}, tcp_server::TcpServer, tcp_connection::{TcpConnection, TcpConnMessage}, error};
use crate::tcp_server::K_ACCEPT_TOKEN;

// user should define type Message and fn handle_message
pub trait Service: Sized {
    type Message: Debug + Send;
    type Connection: TcpConnection + 'static;

    fn server(&self) -> &TcpServer<Self::Connection, Self>;
    fn handle_message(&mut self, msg: Self::Message, event_loop: &mut EventLoop<Self>);
    fn connection_init_callback(&self) -> Box<dyn FnOnce(&mut Self::Connection) + Sync + Send>;
    fn shutdown(&mut self, event_loop: &mut EventLoop<Self>);
}

impl<S> Handle for S where S: Service {
    type AnyMsg = S::Message;
    type Timeout = ();

    fn handle_io_event(&mut self, event: &mio::event::Event, _: &mut EventLoop<Self>) {
        match event.token() {
            K_ACCEPT_TOKEN => {
                if event.is_readable() {
                    let server = self.server();
                    loop {
                        match server.listener().accept() {
                            Ok((stream, peer_addr)) => {
                                let next = server.get_pool_next();
                                let callback = self.connection_init_callback();
                                next.notify(TcpConnMessage::Established(stream, peer_addr, callback));
                                // or server.on_established(stream, peer_addr, self.connection_init_callback())?
                                // if fsm: how to combine Server::connection_init_callback() with Connected::entry()?
                                
                            },
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(e) => {
                                error!("accept error: {}", e);
                                server.notifier().dead_letter();
                            },
                        }
                    }
                }

            },
            tok => error!("unknown event with {:?}", tok),
        }
    }

    fn handle_notify(&mut self, msg: Self::AnyMsg, event_loop: &mut EventLoop<Self>) {
        self.handle_message(msg, event_loop);
    }

    fn handle_timeout(&mut self, timeout: Self::Timeout, repeat: bool, event_loop: &mut EventLoop<Self>) {
    }

    fn refresh_timeout(&mut self, mark: Self::Timeout, timeout: crate::timer::Timeout) {
    }

    fn graceful_shutdown(&mut self, event_loop: &mut EventLoop<Self>) {
        self.shutdown(event_loop);
    }
}