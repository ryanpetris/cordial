//! USB allocation shared by startup, saved preferences and live connections.
use crate::outputs::Plan;

/// The interfaces actually exposed. Editor preference and exposure are independent.
#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub struct Topology {
    pub desktop: bool,
    pub interfaces: u8,
    pub gamepads: Plan,
}

/// Compiled and platform budgets, excluding endpoint zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    pub interfaces: u8,
    pub handlers: u8,
    pub in_endpoints: u8,
    pub out_endpoints: u8,
    pub config_bytes: u16,
    pub bos_bytes: u16,
    pub msos_bytes: u16,
    pub packet_bytes: u16,
    pub out_packet_bytes: u16,
    pub fifo_words: u16,
    pub hid_slots: u8,
    pub xinput_slots: u8,
}
impl Default for Budget {
    fn default() -> Self {
        Self::PICO
    }
}
impl Budget {
    pub const PICO: Self = Self {
        interfaces: 12,
        handlers: 10,
        in_endpoints: 15,
        out_endpoints: 15,
        config_bytes: 512,
        bos_bytes: 64,
        msos_bytes: 64,
        packet_bytes: 3712,
        out_packet_bytes: u16::MAX,
        fifo_words: u16::MAX,
        hid_slots: 4,
        xinput_slots: 4,
    };
    // The ESP composition has no gamepad backends. Its OTG driver owns five
    // non-control endpoints per direction and 256 bytes of OUT packet storage.
    pub const ESP: Self = Self {
        in_endpoints: 5,
        out_endpoints: 5,
        packet_bytes: u16::MAX,
        out_packet_bytes: 192,
        fifo_words: 256,
        hid_slots: 0,
        xinput_slots: 0,
        ..Self::PICO
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Demand {
    pub desktop: u8,
    pub hid: [u8; 4],
    pub xinput: [u8; 4],
    pub editor: u8,
    pub interfaces: u8,
}
impl Default for Demand {
    fn default() -> Self {
        Self {
            desktop: 0xff,
            hid: [0xe2; 4],
            xinput: [0xe3; 4],
            editor: 0xff,
            interfaces: 0,
        }
    }
}

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub struct Allocation {
    pub topology: Topology,
    pub required_exceeded: bool,
    pub reserved_exceeded: bool,
}

/// Exact descriptor sizes written by the class builders, including each function's IAD.
/// CDC uses two interfaces and three endpoints. XInput shares one IAD and handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cost {
    pub interfaces: u8,
    pub handlers: u8,
    pub in_endpoints: u8,
    pub out_endpoints: u8,
    pub config_bytes: u16,
    pub bos_bytes: u16,
    pub msos_bytes: u16,
    pub packet_bytes: u16,
    pub out_packet_bytes: u16,
    pub fifo_words: u16,
}
impl Topology {
    pub fn cost(self) -> Cost {
        let desktop = u8::from(self.desktop);
        let editor = u8::from(self.interfaces != 0);
        let hid = self.gamepads.hid_gamepads;
        let xinput = self.gamepads.xinput_gamepads;
        let receiver = u8::from(xinput != 0);
        let input = 2 + desktop + hid + xinput + editor;
        let output = 1 + xinput + editor;
        Cost {
            interfaces: 2 + desktop + hid + xinput + editor,
            handlers: 3 + desktop + hid + receiver + editor,
            in_endpoints: input,
            out_endpoints: output,
            config_bytes: 70
                + 33 * u16::from(desktop + hid)
                + 40 * u16::from(editor)
                + 8 * u16::from(receiver)
                + 43 * u16::from(xinput),
            bos_bytes: if receiver != 0 { 33 } else { 5 },
            msos_bytes: if receiver != 0 { 46 } else { 0 },
            // RP endpoints reserve packets rounded to 64 bytes, including CDC notification.
            packet_bytes: 64 * u16::from(input + output),
            // ESP reserves 64 bytes of its OUT buffer for endpoint zero. Its FIFO includes
            // 30 receive words, 32 endpoint-zero words, and at least 16 per IN endpoint.
            out_packet_bytes: 64 + 32 * u16::from(xinput + editor),
            fifo_words: 62 + 16 * u16::from(input) + 16 + 8 * u16::from(xinput + editor),
        }
    }
    pub fn fits(self, budget: Budget) -> bool {
        if self.gamepads.hid_gamepads > budget.hid_slots.min(4)
            || self.gamepads.xinput_gamepads > budget.xinput_slots.min(4)
            || self.interfaces & !3 != 0
            || self.interfaces.count_ones() > 1
        {
            return false;
        }
        let c = self.cost();
        c.interfaces <= budget.interfaces
            && c.handlers <= budget.handlers
            && c.in_endpoints <= budget.in_endpoints
            && c.out_endpoints <= budget.out_endpoints
            && c.config_bytes <= budget.config_bytes
            && c.bos_bytes <= budget.bos_bytes
            && c.msos_bytes <= budget.msos_bytes
            && c.packet_bytes <= budget.packet_bytes
            && c.out_packet_bytes <= budget.out_packet_bytes
            && c.fifo_words <= budget.fifo_words
            && self.gamepads.hid_gamepads <= budget.hid_slots.min(4)
            && self.gamepads.xinput_gamepads <= budget.xinput_slots.min(4)
    }
}

/// Selects lower priorities first, with stable kind and slot ties. Invalid budgets fall back to
/// serial alone: callers must supply at least the platform resources needed by CDC.
pub fn allocate(demand: Demand, budget: Budget) -> Allocation {
    let mut result = Allocation::default();
    let mut candidates = [(0xff, 0u8); 10];
    candidates[0] = (demand.desktop, 0);
    for slot in 0..4 {
        if slot < usize::from(budget.hid_slots.min(4)) || demand.hid[slot] < 0xe0 {
            candidates[1 + slot] = (demand.hid[slot], 1 + slot as u8);
        }
        if slot < usize::from(budget.xinput_slots.min(4)) || demand.xinput[slot] < 0xe0 {
            candidates[5 + slot] = (demand.xinput[slot], 5 + slot as u8);
        }
    }
    candidates[9] = (
        if demand.interfaces != 0 {
            demand.editor
        } else {
            0xff
        },
        9,
    );
    candidates.sort_unstable();
    for (priority, kind) in candidates {
        if priority == 0xff {
            continue;
        }
        let mut proposed = result.topology;
        match kind {
            0 => proposed.desktop = true,
            1..=4 => proposed.gamepads.hid_gamepads += 1,
            5..=8 => proposed.gamepads.xinput_gamepads += 1,
            _ => proposed.interfaces = demand.interfaces & demand.interfaces.wrapping_neg(),
        }
        if proposed.fits(budget) {
            result.topology = proposed;
        } else if priority < 0xc0 {
            result.required_exceeded = true;
        } else if priority < 0xe0 {
            result.reserved_exceeded = true;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_demand_preempts_reservations_and_recovers_deterministically() {
        let budget = Budget {
            interfaces: 4,
            hid_slots: 2,
            xinput_slots: 2,
            ..Budget::PICO
        };
        let mut demand = Demand {
            desktop: 0x81,
            hid: [0x82, 0xc2, 0xff, 0xff],
            xinput: [0x83, 0xc3, 0xff, 0xff],
            editor: 0xc4,
            interfaces: 1,
        };
        let idle = allocate(demand, budget);
        assert!(idle.topology.desktop);
        assert_eq!(
            idle.topology.gamepads,
            Plan {
                hid_gamepads: 1,
                xinput_gamepads: 0
            }
        );
        assert!(idle.required_exceeded && idle.reserved_exceeded);
        demand.xinput[0] = 0x43;
        let active = allocate(demand, budget);
        assert_eq!(
            active.topology.gamepads,
            Plan {
                hid_gamepads: 0,
                xinput_gamepads: 1
            }
        );
        assert!(active.topology.desktop);
        demand.xinput[0] = 0x83;
        assert_eq!(allocate(demand, budget), idle);
    }

    #[test]
    fn every_builder_and_hardware_budget_can_limit_the_same_candidate() {
        let demand = Demand {
            desktop: 0x41,
            hid: [0xff; 4],
            xinput: [0xff; 4],
            ..Default::default()
        };
        let cost = Topology {
            desktop: true,
            ..Default::default()
        }
        .cost();
        for budget in [
            Budget {
                interfaces: cost.interfaces - 1,
                ..Budget::PICO
            },
            Budget {
                handlers: cost.handlers - 1,
                ..Budget::PICO
            },
            Budget {
                in_endpoints: cost.in_endpoints - 1,
                ..Budget::PICO
            },
            Budget {
                config_bytes: cost.config_bytes - 1,
                ..Budget::PICO
            },
            Budget {
                packet_bytes: cost.packet_bytes - 1,
                ..Budget::PICO
            },
            Budget {
                fifo_words: cost.fifo_words - 1,
                ..Budget::PICO
            },
        ] {
            let result = allocate(demand, budget);
            assert_eq!(result.topology, Topology::default());
            assert!(result.required_exceeded);
            assert!(!result.reserved_exceeded);
        }
        let demand = Demand {
            editor: 0xc4,
            interfaces: 1,
            hid: [0xff; 4],
            xinput: [0xff; 4],
            ..Default::default()
        };
        let cost = Topology {
            interfaces: 1,
            ..Default::default()
        }
        .cost();
        for budget in [
            Budget {
                out_endpoints: cost.out_endpoints - 1,
                ..Budget::PICO
            },
            Budget {
                out_packet_bytes: cost.out_packet_bytes - 1,
                ..Budget::PICO
            },
        ] {
            let result = allocate(demand, budget);
            assert_eq!(result.topology, Topology::default());
            assert!(!result.required_exceeded);
            assert!(result.reserved_exceeded);
        }
        let demand = Demand {
            xinput: [0x43, 0xff, 0xff, 0xff],
            hid: [0xff; 4],
            ..Default::default()
        };
        for budget in [
            Budget {
                bos_bytes: 32,
                ..Budget::PICO
            },
            Budget {
                msos_bytes: 45,
                ..Budget::PICO
            },
        ] {
            assert!(allocate(demand, budget).required_exceeded);
        }
    }

    #[test]
    fn unused_extras_are_silent_and_disabled_editors_are_absent() {
        let result = allocate(
            Demand::default(),
            Budget {
                interfaces: 2,
                ..Budget::PICO
            },
        );
        assert_eq!(result, Allocation::default());
        let esp = allocate(
            Demand {
                desktop: 0x41,
                editor: 0xc4,
                interfaces: 2,
                ..Default::default()
            },
            Budget::ESP,
        );
        assert_eq!(
            esp.topology,
            Topology {
                desktop: true,
                interfaces: 2,
                ..Default::default()
            }
        );
        assert!(!esp.required_exceeded && !esp.reserved_exceeded);
        let missing = allocate(
            Demand {
                hid: [0x82, 0xc2, 0xe2, 0xe2],
                ..Default::default()
            },
            Budget::ESP,
        );
        assert!(missing.required_exceeded && missing.reserved_exceeded);
    }
}
