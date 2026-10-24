//! Desktop HID reports and the boot keyboard representation of held keys.
use crate::*;
use embassy_futures::select::{Either3, select3};
use embassy_time::{Duration, Instant, Timer};
use hid::RequestHandler;

fn boot_report(keys: &[u8]) -> [u8; 8] {
    let mut report = [0; 8];
    report[0] = keys[28];
    let mut count = 0;
    for code in (1..224).chain(232..256) {
        if keys[code / 8] & (1 << (code % 8)) != 0 {
            if count == 6 {
                report[2..].fill(1);
                break;
            }
            report[2 + count] = code as u8;
            count += 1;
        }
    }
    report
}

pub struct ReportHandler<'a>(pub &'a Io);
impl hid::RequestHandler for ReportHandler<'_> {
    fn get_report(&mut self, id: hid::ReportId, buf: &mut [u8]) -> Option<usize> {
        if self.0.status().boot_keyboard {
            return match id {
                hid::ReportId::In(0) if buf.len() >= 8 => {
                    self.0.reports.lock(|reports| {
                        buf[..8].copy_from_slice(&boot_report(&reports.borrow()[0]));
                    });
                    Some(8)
                }
                hid::ReportId::Out(0) if !buf.is_empty() => {
                    buf[0] = self.0.status().leds;
                    Some(1)
                }
                _ => None,
            };
        }
        match id {
            hid::ReportId::Out(1) if !buf.is_empty() => {
                let size = buf.len().min(2);
                buf[..size].copy_from_slice(&[1, self.0.status().leds][..size]);
                Some(size)
            }
            hid::ReportId::In(id @ 1..=10) if !buf.is_empty() => {
                buf[0] = id;
                let size = [32, 10, 16, 68, 5, 9, 6, 34, 8, 1][id as usize - 1].min(buf.len() - 1);
                self.0.reports.lock(|r| {
                    buf[1..1 + size].copy_from_slice(&r.borrow()[id as usize - 1][..size])
                });
                if id == 2 && size > 2 {
                    buf[3..1 + size].fill(0);
                } else if (4..=6).contains(&id) {
                    buf[1..1 + size].fill(0);
                }
                Some(1 + size)
            }
            _ => None,
        }
    }
    fn set_report(&mut self, id: hid::ReportId, data: &[u8]) -> OutResponse {
        if self.0.status().boot_keyboard && id == hid::ReportId::Out(0) && data.len() == 1 {
            self.0.update(|s| s.leds = data[0] & 0x1f);
            OutResponse::Accepted
        } else if !self.0.status().boot_keyboard
            && id == hid::ReportId::Out(1)
            && data.len() == 2
            && data[0] == 1
        {
            self.0.update(|s| s.leds = data[1] & 0x1f);
            OutResponse::Accepted
        } else {
            OutResponse::Rejected
        }
    }

    fn get_protocol(&self) -> hid::HidProtocolMode {
        if self.0.status().boot_keyboard {
            hid::HidProtocolMode::Boot
        } else {
            hid::HidProtocolMode::Report
        }
    }

    fn set_protocol(&mut self, protocol: hid::HidProtocolMode) -> OutResponse {
        self.0
            .update(|s| s.boot_keyboard = protocol == hid::HidProtocolMode::Boot);
        self.0.hid_settings.signal(());
        OutResponse::Accepted
    }

    fn get_idle_ms(&mut self, id: Option<hid::ReportId>) -> Option<u32> {
        let index = match id {
            None => {
                return self.0.hid_idle.lock(|idle| {
                    let rates = idle.get();
                    (self.0.status().boot_keyboard || rates.iter().all(|rate| *rate == rates[0]))
                        .then_some(rates[0])
                });
            }
            Some(hid::ReportId::In(id @ 1..=10)) if !self.0.status().boot_keyboard => {
                id as usize - 1
            }
            _ => return None,
        };
        Some(self.0.hid_idle.lock(|idle| idle.get()[index]))
    }

    fn set_idle_ms(&mut self, id: Option<hid::ReportId>, duration_ms: u32) {
        let boot = self.0.status().boot_keyboard;
        self.0.hid_idle.lock(|idle| {
            let mut rates = idle.get();
            for (index, rate) in rates.iter_mut().enumerate() {
                if (boot && index == 0 && id.is_none())
                    || (!boot && (id.is_none() || id == Some(hid::ReportId::In(index as u8 + 1))))
                {
                    *rate = duration_ms;
                }
            }
            idle.set(rates);
        });
        self.0.hid_settings.signal(());
    }
}

async fn wait_until(at: Option<Instant>) {
    match at {
        Some(at) => Timer::at(at).await,
        None => core::future::pending().await,
    }
}

pub(crate) async fn run<'d, D: Driver<'d>>(mut hid: hid::HidWriter<'d, D, 69>, io: &Io) {
    let mut status_rx = io.status.receiver().unwrap();
    let mut sent_at = [Instant::now(); 10];
    let mut sent_idle = false;
    loop {
        let status = io.status();
        let rates = io.hid_idle.lock(core::cell::Cell::get);
        let due = rates
            .iter()
            .enumerate()
            .filter(|(index, rate)| {
                status.input_ready() && **rate != u32::MAX && (!status.boot_keyboard || *index == 0)
            })
            .map(|(index, rate)| {
                (
                    index,
                    sent_at[index] + Duration::from_millis((*rate).into()),
                )
            })
            .min_by_key(|(_, at)| *at);
        // Alternate overdue refreshes with queued input so neither can starve.
        let queued = sent_idle.then(|| io.hid_tx.try_receive().ok()).flatten();
        let packet = if queued.is_some() {
            queued
        } else {
            match select3(
                io.hid_settings.wait(),
                wait_until(due.map(|(_, at)| at)),
                io.hid_tx.receive(),
            )
            .await
            {
                Either3::First(()) => continue,
                Either3::Second(()) => None,
                Either3::Third(packet) => Some(packet),
            }
        };
        sent_idle = packet.is_none();
        let status = io.status();
        let mut bytes = [0; 69];
        let (index, length, generation) = if let Some(packet) = &packet {
            let index = packet.bytes[0] as usize - 1;
            let length = if status.boot_keyboard {
                if index == 0 {
                    bytes[..8].copy_from_slice(&boot_report(&packet.bytes[1..]));
                    8
                } else {
                    // Other collections have no representation in boot protocol.
                    0
                }
            } else {
                bytes[..packet.length].copy_from_slice(&packet.bytes[..packet.length]);
                packet.length
            };
            (index, length, packet.generation)
        } else {
            let index = due.unwrap().0;
            let id = if status.boot_keyboard {
                0
            } else {
                index as u8 + 1
            };
            let Some(length) = ReportHandler(io).get_report(hid::ReportId::In(id), &mut bytes)
            else {
                continue;
            };
            (index, length, status.generation)
        };
        let success = status.input_ready()
            && status.generation == generation
            && (length == 0
                || matches!(
                    select(
                        hid.write(&bytes[..length]),
                        status_rx.changed_and(|s| s.generation != generation
                            || s.boot_keyboard != status.boot_keyboard)
                    )
                    .await,
                    Either::First(Ok(()))
                ));
        let success = success
            && io.status().generation == generation
            && io.status().boot_keyboard == status.boot_keyboard;
        if success {
            sent_at[index] = Instant::now();
            if let Some(packet) = &packet {
                io.reports.lock(|reports| {
                    let mut reports = reports.borrow_mut();
                    let target = &mut reports[index];
                    let data = &packet.bytes[1..packet.length];
                    if matches!(packet.bytes[0], 7 | 8) {
                        for (old, value) in target[..data.len()]
                            .as_chunks_mut::<2>()
                            .0
                            .iter_mut()
                            .zip(data.as_chunks::<2>().0.iter())
                        {
                            if *value != [0xff, 0xff] {
                                old.copy_from_slice(value);
                            }
                        }
                    } else {
                        target[..data.len()].copy_from_slice(data);
                    }
                });
            }
        }
        if let Some(packet) = packet {
            io.hid_done
                .send(HidDone {
                    generation: packet.generation,
                    sequence: packet.sequence,
                    success,
                })
                .await;
            io.changed.signal(());
        } else if !success
            && io.status().generation == generation
            && io.status().boot_keyboard == status.boot_keyboard
        {
            status_rx.changed().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boot_rollover_keeps_modifiers_and_recovers_after_release() {
        let mut keys = [0; 32];
        keys[28] = 0xff;
        for code in 4..=9 {
            keys[code / 8] |= 1 << (code % 8);
        }
        assert_eq!(boot_report(&keys), [255, 0, 4, 5, 6, 7, 8, 9]);
        keys[1] |= 1 << 2;
        assert_eq!(boot_report(&keys), [255, 0, 1, 1, 1, 1, 1, 1]);
        keys[0] = 0;
        assert_eq!(boot_report(&keys), [255, 0, 8, 9, 10, 0, 0, 0]);
        keys.fill(0);
        assert_eq!(boot_report(&keys), [0; 8]);
    }
}
