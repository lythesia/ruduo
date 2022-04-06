use std::net::SocketAddr;

use mio::{Token, net::{TcpListener, TcpStream}, Interest, event::{Source, Event}, Registry};
use slab::Slab;

use crate::{channel::Channel, event_loop::{Handle, EventLoop}};

pub trait Acceptor: Sized {
    fn ltoken(&self) -> Token;
    fn listener(&self) -> &TcpListener;

    fn enable(&mut self, registry: &Registry) {
        let s = self.source();
        let tok = self.token();
        registry.register(s, self.tok, Interest::READABLE)
        .expect(&format!("failed to register listener({:?})", tok));
    }

    fn on_connection(&self, stream: TcpStream, peer_addr: SocketAddr);
}

// impl Acceptor {
    // pub fn new(token: Token, addr: SocketAddr) -> std::io::Result<Self> {
    //     let mut listener = TcpListener::bind(addr)?;
    //     //registry.register(&mut listener, token, Interest::READABLE)?;

    //     Ok(Self {
    //         token,
    //         listener,
    //     })
    // }
// }

impl<T> Channel for T where T: Acceptor {
    fn token(&self) -> Token {
        self.ltoken()
    }

    fn source(&mut self) -> &mut dyn Source {
        &mut self.listener
    }

    fn interests(&self) -> Option<Interest> {
        Some(Interest::READABLE)
    }

    fn handle_read(&mut self) {
        // because of edge-trigger
        loop {
            match self.listener.accept() {
                Ok((stream, peer_addr)) => {
                    // how to invoke server logic?
                    // self.on_connection(stream, peer_addr, |s, a| {
                    //     if let Err(e) = s.shutdown(std::net::Shutdown::Both) {
                    //         if e.kind() != std::io::ErrorKind::NotConnected {
                    //             eprint!("failed to close conn {}: {}", a, e);
                    //         }
                    //     }
                    // });
                    self.on_connection(stream, peer_addr);
                },
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => self.handle_error(&e),
            }
        }
    }
}

// deregister on drop, but acceptor contains no registry..
// impl Drop

pub struct Acceptors<A: Acceptor> {
    acceptors: Slab<A>,
}

impl<A: Acceptor> Acceptors<A> {
    pub fn new(cap: usize) -> Self {
        Self {
            acceptors: Slab::with_capacity(cap),
        }
    }

    pub fn listen(&mut self, addr: SocketAddr, registry: &Registry) -> std::io::Result<()> {
        if self.acceptors.len() == self.acceptors.capacity() {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "max listeners reached!"));
        }

        let entry = self.acceptors.vacant_entry();
        let token = Token(entry.key());
        let mut acc = Acceptor::new(token, addr)?;
        acc.enable(registry);
        entry.insert(acc);

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum AcceptorMessage {
    NewListener(SocketAddr),
}
// unsafe impl Send for AcceptorMessage {}
impl<A: Acceptor> Handle for Acceptors<A> {
    type AnyMsg = AcceptorMessage;

    fn handle_io_event(&mut self, event: &Event, event_loop: &mut EventLoop<Self>) {
        let token = event.token();

        if let Some(acc) = self.acceptors.get_mut(token.0) {
            if event.is_readable() {
                acc.handle_read();
            }
        }
    }

    fn handle_notify(&mut self, msg: Self::AnyMsg, event_loop: &mut EventLoop<Self>) {
        let registry = event_loop.registry();
        match msg {
            AcceptorMessage::NewListener(addr) => {
                if let Err(e) = self.listen(addr.clone(), registry) {
                    eprint!("fail to listen at {}: {}", addr, e);
                }
            },
        }
    }

    fn graceful_shutdown(&mut self, event_loop: &mut EventLoop<Self>) {
        let registry = event_loop.registry();
        for mut acc in self.acceptors.drain() {
            registry.deregister(acc.source()).expect("failed to deregister");
        }
        event_loop.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_acceptors() {
        let (mut el, notifier) = EventLoop::<Acceptors>::new().unwrap();
        let mut accs = Acceptors::new(10);

        let addr = SocketAddr::from_str("127.0.0.1:8080").unwrap();
        let addr_clone = addr.clone();

        let msg = AcceptorMessage::NewListener(addr);
        notifier.notify(msg);

        let notifier_clone = notifier.clone();
        let h = std::thread::spawn(move || {
            for _ in 0..4 {
                use std::net::TcpStream;
                let _ = TcpStream::connect(addr_clone.clone()).unwrap();
            }
            notifier_clone.dead_letter();
        });

        el.run(&mut accs).unwrap();
        h.join().unwrap();
    }
}