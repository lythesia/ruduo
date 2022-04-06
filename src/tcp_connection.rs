use std::{io::{ErrorKind}, net::{Shutdown, SocketAddr}, hash::Hash};

use mio::{net::TcpStream, Token, Interest, Registry};

use crate::{channel::Channel, buffer::Buffer, event_loop::{Notifier, EventLoopCtrl}, error, timer::Timeout};

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

pub enum TcpConnMessage<T: TcpConnection> {
    Established(TcpStream, SocketAddr, Box<dyn FnOnce(&mut T) + Sync + Send>),
    Connecting(TcpStream, SocketAddr, Box<dyn FnOnce(&mut T) + Sync + Send>),
    Removed(Token),
    Write(Token, Vec<u8>),
    WriteComplete(Token),
    Enable(Token, Interest),
    Disable(Token, Interest),
    Shutdown(Token, Shutdown),
    SetContext(Token, Box<dyn FnOnce(&mut T) + Sync + Send>),
    AddTimer(Token, u64, T::Timeout, Box<dyn FnOnce(&mut T, &Registry) + Send>),
    AddTimer2(Token, u64, u64, T::Timeout, Box<dyn Fn(&mut T, &Registry) + Send>),
    CancelTimer(Token, T::Timeout),
}

impl<T: TcpConnection> std::fmt::Debug for TcpConnMessage<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Established(_, addr, _) => f.debug_tuple("Established").field(addr).finish(),
            Self::Connecting(_, addr, _) => f.debug_tuple("Connecting").field(addr).finish(),
            Self::Removed(tok) => f.debug_tuple("Removed").field(tok).finish(),
            Self::Write(tok, dat) => f.debug_tuple("Write").field(tok).field(dat).finish(),
            Self::WriteComplete(tok) => f.debug_tuple("WriteComplete").field(tok).finish(),
            Self::Enable(tok, int) => f.debug_tuple("Enable").field(tok).field(int).finish(),
            Self::Disable(tok, int) => f.debug_tuple("Write").field(tok).field(int).finish(),
            Self::Shutdown(tok, how) => f.debug_tuple("Shutdown").field(tok).field(how).finish(),
            Self::SetContext(tok, _) => f.debug_tuple("SetContext").field(tok).finish(),
            Self::AddTimer(tok, delay, _, _) => f.debug_tuple("AddTimer").field(tok).field(delay).finish(),
            Self::AddTimer2(tok, delay, interval, _, _) => f.debug_tuple("AddTimer2").field(tok).field(delay).field(interval).finish(),
            Self::CancelTimer(tok, _) => f.debug_tuple("CancelTimer").field(tok).finish(),
        }
    }
}

// todo: connection state machine?
pub trait TcpConnection: Sized {
    type Timeout: Clone + Eq + Hash + Send;

    fn token(&self) -> Token;
    fn io(&self) -> &TcpStream;
    fn io_mut(&mut self) -> &mut TcpStream;
    fn input(&mut self) -> &mut Buffer;
    fn output(&mut self) -> &mut Buffer;
    fn interests(&mut self) -> &mut Option<Interest>;
    fn notifier(&self) -> Notifier<TcpConnMessage<Self>>;

    fn make(token: Token, stream: TcpStream, notifier: &Notifier<TcpConnMessage<Self>>) -> Self;

    fn enable_io(&mut self, mode: Interest, registry: &Registry) {
        let old = self.interests().clone();
        let token = self.token();
        let new = match old {
            Some(x) => {
                let y = x.add(mode);
                registry.reregister(self.io_mut(), token, y)
                .expect(&format!("faield to reregister stream({:?})", token));
                Some(y)
            },
            _ => {
                let y = mode;
                registry.register(self.io_mut(), token, y)
                .expect(&format!("faield to register stream({:?})", token));
                Some(y)
            },
        };
        *self.interests() = new;
    }

    fn disable_io(&mut self, mode: Interest, registry: &Registry) {
        let token = self.token();
        let old = self.interests().clone();
        let new = old.and_then(|i| {
            match i.remove(mode) {
                Some(y) => {
                    registry.reregister(self.io_mut(), token, y)
                    .expect(&format!("faield to reregister stream({:?})", token));
                    Some(y)
                },
                _ => {
                    registry.deregister(self.io_mut())
                    .expect(&format!("faield to deregister stream({:?})", token));
                    None
                },
            }
        });
        *self.interests() = new;
    }

    fn do_read(&mut self) -> std::io::Result<usize>; // read to self.input
    fn do_write(&mut self) -> std::io::Result<usize>; // write from self.output.peek
    fn do_write_with(&mut self, data: &[u8]) -> std::io::Result<usize>;

    // todo: 如果干脆全部放到buffer里, 让handle_write处理?
    fn send(&mut self, data: &[u8], registry: &Registry) {
        let int = self.interests().clone();
        let mut written = 0;
        if !int.is_writing() {
            if self.output().readable_bytes() == 0 {
                loop {
                    let buf = &data[written..];
                    match self.do_write_with(buf) {
                        Ok(n) => {
                            if n == buf.len() {
                                // also give chance to poller to avoid infinite write
                                self.notifier().notify(TcpConnMessage::WriteComplete(self.token()));
                                break;
                            } else {
                                written += n;
                                continue;
                            }
                        },
                        Err(e) => {
                            if e.kind() == ErrorKind::WouldBlock {
                                self.output().append(&data[written..]);
                                self.enable_io(Interest::WRITABLE, registry);
                            } else {
                                error!("failed to write: {}, fallback to Buffer::append", e);
                                self.output().append(data);
                                self.enable_io(Interest::WRITABLE, registry);
                            }
                            break;
                        },
                    }
                }
            }
            else {
                self.output().append(data);
                self.enable_io(Interest::WRITABLE, registry);
            }
        }
        else {
            self.output().append(data);
        }
    }

    fn schedule_after(&self, delay_ms: u64, timeout: Self::Timeout,
        f: Box<dyn FnOnce(&mut Self, &Registry) + Send>) {
        self.notifier().notify(TcpConnMessage::AddTimer(self.token(), delay_ms, timeout, f));
        // 1. event_loop.timer.schedule
        // 2. handle.add_timer_task
        // or give one token indicating timer_task (mapping token <=> Timeout)
        // then TcpConnMessage::CancelTimer(token) to cancel if need
        // return handler for cancel?
    }

    fn schedule_every(&self, delay_ms: u64, interval_ms: u64, timeout: Self::Timeout,
        f: Box<dyn Fn(&mut Self, &Registry) + Send>) {
        self.notifier().notify(
            TcpConnMessage::AddTimer2(self.token(), delay_ms, interval_ms, timeout, f));
    }

    fn schedule_cancel(&self, timeout: Self::Timeout) {
        self.notifier().notify(TcpConnMessage::CancelTimer(self.token(), timeout));
    }

    fn shutdown(&self) {
        let stream = self.io();
        stream.shutdown(Shutdown::Write).expect("failed to shutdown stream");
    }
    // fn connection_destroyed(&self);

    // registry => ctrl (include timer_ctrl)
    // 1. ctrl.schedule_timeout(Token(token_of_connection)) => event_loop.timers.insert(Token)
    // 2. event_loop pop Token => handle.handle_timeout(Token)
    // 3. conn = handle.conns.get(Token)
    // 4. conn.on_timeout_event(&mut self, &ctrl)
    // fn on_established(&mut self, registry: &Registry);
    fn on_established(&mut self, ctrl: &mut EventLoopCtrl<Self::Timeout>);
    // fn on_read_complete(&mut self, registry: &Registry);
    fn on_read_complete(&mut self, ctrl: &mut EventLoopCtrl<Self::Timeout>);
    // fn on_write_complete(&mut self, registry: &Registry);
    fn on_write_complete(&mut self, ctrl: &mut EventLoopCtrl<Self::Timeout>);
    fn on_close(&mut self);
    // fn on_timer(&mut self, ctrl: &mut EventLoopCtrl<Self::Timeout>);
}

impl<T> Channel for T where T: TcpConnection {
    type To = T::Timeout;
    fn handle_read(&mut self, ctrl: &mut EventLoopCtrl<T::Timeout>) {
        loop {
            match self.do_read() {
                Ok(n) => {
                    if n > 0 {
                        continue;
                    } else {
                        // passive close connection
                        // 1. deregister
                        // 2. tcpserver reaction to connection state change(optional)
                        // 3. remove from tcpserver (map.erase)
                        self.disable_io(Interest::READABLE|Interest::WRITABLE, ctrl.registry);
                        self.on_close(); // user callback
                        self.notifier().notify(TcpConnMessage::Removed(self.token()));
                        // 4. loop_notify.send tcpconn.conn_destroyed (defensive pg, not iff)
                        break;
                    }
                },
                Err(e) => {
                    if e.kind() == ErrorKind::WouldBlock {
                        // Q: why not notify? because read is passive, there's no way in on_read_complete to continue reading
                        self.on_read_complete(ctrl);
                    } else {
                        self.handle_error(&e);
                    }
                    break;
                }
            }
        }
    }

    fn handle_write(&mut self, ctrl: &mut EventLoopCtrl<T::Timeout>) {
        if self.interests().is_writing() {
            loop {
                match self.do_write() {
                    Ok(n) => {
                        let output = self.output();
                        output.retrieve(n);
                        if output.readable_bytes() == 0 {
                            // once write done, stop writing
                            self.disable_io(Interest::WRITABLE, ctrl.registry);
                            // use notify instead of direct callback invoke to avoid case of infinite write(if user gives that..)
                            self.notifier().notify(TcpConnMessage::WriteComplete(self.token()));
                            break;
                        }
                    },
                    Err(e) => {
                        if e.kind() == ErrorKind::WouldBlock {
                            break;
                        } else {
                            self.handle_error(&e);
                        }
                    }
                }
            }
        }
    }

    fn handle_error(&mut self, err: &std::io::Error) {
        error!("TcpConnection{:?} handle error: {}", self.token(), err);
    }
}
