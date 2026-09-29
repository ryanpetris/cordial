//! A stand-in controller for interface tests.
use crate::{
    controller::{Command, SessionId, State, Ticket},
    ui::Backend,
};
use cordial_protocol::identifiers::CandidateId;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Open(String),
    Close,
    Run(String),
    Hide(String),
    List,
}

#[derive(Clone, Default)]
pub struct Fake {
    pub state: Rc<RefCell<Option<State>>>,
    pub calls: Rc<RefCell<Vec<Call>>>,
    pub next: Rc<Cell<u64>>,
    /// Cancellations handed to cancellable commands, in order.
    pub cancels: Rc<RefCell<Vec<crate::client::Cancellation>>>,
}
impl Backend for Fake {
    fn open(&self, port: String, _keep_unready: bool) -> SessionId {
        self.calls.borrow_mut().push(Call::Open(port));
        self.next.set(self.next.get() + 1);
        self.next.get()
    }
    fn close(&self) {
        self.calls.borrow_mut().push(Call::Close);
    }
    fn run(&self, command: Command) -> Ticket {
        self.calls
            .borrow_mut()
            .push(Call::Run(format!("{command:?}")));
        self.next.set(self.next.get() + 1);
        self.next.get()
    }
    fn run_cancellable(&self, command: Command, cancel: crate::client::Cancellation) -> Ticket {
        self.cancels.borrow_mut().push(cancel);
        self.run(command)
    }
    fn state(&self) -> Option<State> {
        self.state.borrow().clone()
    }
    fn hide_candidate(&self, id: &CandidateId) {
        self.calls.borrow_mut().push(Call::Hide(id.0.clone()));
    }
    fn list_ports(&self) {
        self.calls.borrow_mut().push(Call::List);
    }
}
