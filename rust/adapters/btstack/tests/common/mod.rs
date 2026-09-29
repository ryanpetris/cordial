#![allow(dead_code)]
#[cfg(all(feature = "ffi", feature = "classic"))]
pub mod shutdown;
use cordial_btstack::transport::{Io, Packet};
use core::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};
use embassy_futures::block_on;
pub fn take(io: &Io) -> Option<Packet> {
    let mut next = pin!(io.next_outbound());
    block_on(poll_fn(|cx| {
        Poll::Ready(match next.as_mut().poll(cx) {
            Poll::Ready(p) => Some(p),
            Poll::Pending => None,
        })
    }))
}
pub fn receive(io: &Io, bytes: &[u8]) {
    let mut packet = Packet::empty();
    packet.kind = 4;
    packet.len = bytes.len() as u16;
    packet.data_mut().copy_from_slice(bytes);
    block_on(io.received(packet));
}

pub fn reply(io: &Io, opcode: u16, sequence: u8) {
    let mut parameters = match opcode {
        0x1001 => vec![0, 9, 0, 0, 9, 0xff, 0xff, 0, 0],
        0x1002 => {
            let mut b = vec![0xff; 65];
            b[0] = 0;
            b
        }
        0x1003 => vec![0, 0xff, 0xff, 0xff, 0xff, 0xdf, 0xff, 0xff, 0xff],
        0x1005 => vec![0, 0xfb, 0x03, 0, 4, 0, 0, 0],
        0x1009 => vec![0, 7, 6, 5, 4, 3, 2],
        0x2002 => vec![0, 0xfb, 0, 4],
        0x2003 | 0x201c => vec![0; 9],
        0x200f | 0x202a => vec![0, 8],
        0x2018 => vec![0, sequence, 2, 3, 4, 5, 6, 7, 8],
        0x0c14 => vec![0; 249],
        _ => vec![0],
    };
    let mut complete = vec![
        0x0e,
        (3 + parameters.len()) as u8,
        1,
        opcode as u8,
        (opcode >> 8) as u8,
    ];
    complete.append(&mut parameters);
    receive(io, &complete);
}
