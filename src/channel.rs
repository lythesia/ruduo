use mio::Registry;

use crate::event_loop::EventLoopCtrl;

pub trait Channel {
    type To: Clone + Send;
    fn handle_read(&mut self, ctrl: &mut EventLoopCtrl<Self::To>) {}
    fn handle_write(&mut self, ctrl: &mut EventLoopCtrl<Self::To>) {}
    // fn handle_close(&mut self, registry: &Registry) {}
    fn handle_error(&mut self, err: &std::io::Error) {}
}