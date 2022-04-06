use std::{any::Any, net::SocketAddr, os::unix::prelude::AsRawFd, io::ErrorKind};

use mio::{Interest, net::TcpStream, Token, Registry};

use crate::{event_loop::{Notifier}, buffer::Buffer, error};

pub(crate) trait BufState {
    fn is_reading(&self) -> bool;
    fn is_writing(&self) -> bool;
}

impl BufState for Option<Interest> {
    fn is_reading(&self) -> bool {
        self.map_or(false, |e| e.is_readable())
    }

    fn is_writing(&self) -> bool {
        self.map_or(false, |e| e.is_writable())
    }
}

trait State {
    fn entry(&mut self, ctrl: &mut EventLoopCtrl);
    fn exit(&mut self, ctrl: &mut EventLoopCtrl);

    fn handle_read(self: Box<Self>, ctrl: &mut EventLoopCtrl) -> Box<dyn State>;
}

trait Transition<T: State>: State + Into<T> {
    fn transition(self, ctrl: &mut EventLoopCtrl) -> T {
        let mut o = self;
        o.exit(ctrl);
        let mut n = o.into();
        n.entry(ctrl);
        n
    }
}

type ConnCallback<T> = Box<dyn Fn(&mut T, &mut EventLoopCtrl) -> Box<dyn State>>;

struct Minimal {
    stream: TcpStream,
    token: Token,
    local_addr: SocketAddr,
    peer_addr: SocketAddr,
    interests: Option<Interest>,
    context: Option<Box<dyn Any>>,
}
impl Minimal {
    fn enable_io(&mut self, mode: Interest, registry: &Registry) {
        let old = self.interests.clone();
        let token = self.token;
        let new = match old {
            Some(x) => {
                let y = x.add(mode);
                registry.reregister(&mut self.stream, token, y)
                .expect(&format!("faield to reregister stream({:?})", token));
                Some(y)
            },
            _ => {
                let y = mode;
                registry.register(&mut self.stream, token, y)
                .expect(&format!("faield to register stream({:?})", token));
                Some(y)
            },
        };
        self.interests = new;
    }

    fn disable_io(&mut self, mode: Interest, registry: &Registry) {
        let token = self.token;
        let old = self.interests.clone();
        let new = old.and_then(|i| {
            match i.remove(mode) {
                Some(y) => {
                    registry.reregister(&mut self.stream, token, y)
                    .expect(&format!("faield to reregister stream({:?})", token));
                    Some(y)
                },
                _ => {
                    registry.deregister(&mut self.stream)
                    .expect(&format!("faield to deregister stream({:?})", token));
                    None
                },
            }
        });
        self.interests = new;
    }
}
struct Connecting {
    min: Minimal,
    // todo: retry policy
    // local_notifier: Notifier<?>,
}
impl Connecting {
    fn new(stream: TcpStream, local_addr: SocketAddr, peer_addr: SocketAddr) -> Self {
        let fd = stream.as_raw_fd();
        assert!(fd > 0, "invalid fd for socket {:?}!", stream);
        let token = Token(fd as usize);
        Self {
            min: Minimal {
                stream,
                token,
                local_addr,
                peer_addr,
                interests: None,
                context: None,
            }
        }
    }
}
impl State for Connecting {
    fn entry(&mut self, ctrl: &mut EventLoopCtrl) {
        let mut min = self.min;
        min.enable_io(Interest::READABLE, ctrl.registry);
    }
    fn exit(&mut self, ctrl: &mut EventLoopCtrl) {
    }
    fn handle_read(self: Box<Self>, ctrl: &mut EventLoopCtrl) -> Box<dyn State> {
        match self.min.stream.peer_addr() {
            Ok(peer_addr) => {
                Box::new((*self).transition(ctrl))
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected || 
                e.raw_os_error() == Some(libc::EINPROGRESS) => {
                    // next round
                    self
                },
            Err(e) => {
                // fail and retry
                error!("{:?} connecting err: {}", self.min.stream, e);
                // todo: handle_error?
                self
            },
        }
    }
}
impl Into<Connected> for Connecting {
    fn into(self) -> Connected {
        Connected {
            min: self.min,
            input: Buffer::new(),
            output: Buffer::new(),
            on_read_complete: None,
            on_write_complete: None,
            on_close: None,
        }
    }
}
impl Transition<Connected> for Connecting {}

trait WithCallback: State {
    fn with_callback(self: Box<Self>, ctrl: &mut EventLoopCtrl,
        fp: &mut Option<ConnCallback<Self>>) -> Box<dyn State> where Self: Sized + 'static {
        if let Some(f) = fp.take() {
            let n = f(&mut *self, ctrl);
            *fp = Some(f);
            n
        } else {
            self
        }
    }
}
struct Connected {
    min: Minimal,
    input: Buffer,
    output: Buffer,
    on_read_complete: Option<ConnCallback<Self>>,
    on_write_complete: Option<ConnCallback<Self>>,
    on_close: Option<ConnCallback<Self>>,
}
impl Connected {
    fn read(&mut self) -> std::io::Result<usize> {
        let io = &mut self.min.stream;
        self.input.read_from(io)
    }

}
impl State for Connected {
    fn entry(&mut self, ctrl: &mut EventLoopCtrl) {}
    fn exit(&mut self, ctrl: &mut EventLoopCtrl) {}
    fn handle_read(self: Box<Self>, ctrl: &mut EventLoopCtrl) -> Box<dyn State> {
        loop {
            match self.read() {
                Ok(n) => {
                    if n > 0 {
                        continue;
                    } else {
                        let n: Disconnected = (*self).transition(ctrl);
                        return Box::new(n);
                        // self.notifier().notify(TcpConnMessage::Removed(self.token()));
                        // 4. loop_notify.send tcpconn.conn_destroyed (defensive pg, not iff)
                    }
                },
                Err(e) => {
                    if e.kind() == ErrorKind::WouldBlock {
                        let fp = &mut self.on_read_complete;
                        return self.with_callback(ctrl, fp);
                    } else {
                        error!("{:?} read err: {}", self.min.stream, e);
                        return self;
                    }
                }
            }
        }
    }
}
impl WithCallback for Connected {}
impl Into<Disconnected> for Connected {}
impl Transition<Disconnected> for Connected {}

struct Disconnected {
    min: Minimal,
    interest: Option<Interest>,
}
impl State for Disconnected {
    fn entry(&mut self, ctrl: &mut EventLoopCtrl) {
        self.min.disable_io(Interest::READABLE | Interest::WRITABLE, ctrl.registry);
    }
    fn exit(&mut self, ctrl: &mut EventLoopCtrl) {
    }
}