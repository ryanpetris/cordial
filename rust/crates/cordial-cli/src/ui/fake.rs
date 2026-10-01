//! A stand-in controller for interface tests.
use crate::{
    controller::{Command, SessionId, State, Ticket},
    ui::Backend,
};
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
}
impl Backend for Fake {
    fn open(&self, port: String) -> SessionId {
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
    fn state(&self) -> Option<State> {
        self.state.borrow().clone()
    }
    fn hide_candidate(&self, id: &str) {
        self.calls.borrow_mut().push(Call::Hide(id.to_owned()));
    }
    fn list_ports(&self) {
        self.calls.borrow_mut().push(Call::List);
    }
}
