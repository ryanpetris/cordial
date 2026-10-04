use crate::model::identifiers::HostPlatform;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Translation {
    pub key: u8,
    pub consumer: u16,
    pub modifiers: u8,
    pub system: u16,
}
#[derive(Clone, Copy, Debug)]
pub struct Control {
    pub id: u16,
    platforms: [Translation; 3],
}
const fn t(key: u8, consumer: u16, modifiers: u8) -> Translation {
    Translation {
        key,
        consumer,
        modifiers,
        system: 0,
    }
}
const fn all(id: u16, key: u8, consumer: u16) -> Control {
    Control {
        id,
        platforms: [t(key, consumer, 0); 3],
    }
}
/// Controls with platform normalization independent of native report coverage.
pub const NORMALIZED_CONTROLS: usize = 17;

/// Standard HID usages and chords. No application-launch or text-entry sequences.
pub const CONTROLS: &[Control] = &[
    all(0x00c7, 0, 0x0070),
    all(0x00c8, 0, 0x006f),
    all(0x00e4, 0, 0x00b6),
    all(0x00e5, 0, 0x00cd),
    all(0x00e6, 0, 0x00b5),
    all(0x00e7, 0, 0x00e2),
    all(0x00e8, 0, 0x00ea),
    all(0x00e9, 0, 0x00e9),
    all(0x000a, 0, 0x0192),
    all(0x00ec, 0x50, 0),
    all(0x00eb, 0x4f, 0),
    Control {
        id: 0xe0,
        platforms: [t(0, 0, 8), t(0x2b, 0, 8), t(0, 0x029f, 0)],
    },
    Control {
        id: 0xe1,
        platforms: [t(4, 0, 8), t(4, 0, 8), t(0, 0x02a0, 0)],
    },
    Control {
        id: 0x6e,
        platforms: [t(0, 0, 0), t(7, 0, 8), t(0, 0x029f, 8)],
    },
    Control {
        id: 0x6f,
        platforms: [t(0, 0x019e, 0), t(0x0f, 0, 8), t(0x14, 0, 9)],
    },
    Control {
        id: 0xbf,
        platforms: [t(0x46, 0, 0), t(0x46, 0, 0), t(0x20, 0, 10)],
    },
    Control {
        id: 0xea,
        platforms: [t(0x65, 0, 0), t(0x65, 0, 0), t(0x28, 0, 1)],
    },
    all(0x0001, 0, 0x00e9),
    all(0x0002, 0, 0x00ea),
    all(0x0003, 0, 0x00e2),
    all(0x0004, 0, 0x00cd),
    all(0x0005, 0, 0x00b5),
    all(0x0006, 0, 0x00b6),
    all(0x0007, 0, 0x00b7),
    all(0x000b, 0, 0x018e),
    all(0x000c, 0, 0x0203),
    all(0x000d, 0, 0x00b8),
    all(0x000e, 0, 0x018a),
    all(0x000f, 0, 0x0095),
    all(0x0010, 0x3a, 0),
    all(0x0011, 0, 0x0184),
    all(0x0012, 0, 0x0186),
    all(0x0013, 0, 0x0188),
    all(0x0015, 0, 0x021a),
    all(0x0017, 0, 0x0279),
    all(0x0019, 0, 0x0208),
    all(0x001b, 0, 0x0207),
    all(0x0020, 0, 0x022a),
    all(0x0022, 0, 0x0223),
    all(0x0024, 0, 0x0205),
    all(0x0026, 0, 0x0206),
    all(0x0028, 0, 0x0183),
    all(0x0032, 0, 0x0194),
    all(0x003b, 0, 0x00b2),
    all(0x003c, 0, 0x0227),
    all(0x003e, 0, 0x0221),
    all(0x003f, 0, 0x00b9),
    Control {
        id: 0x0040,
        platforms: [Translation {
            system: 0x82,
            key: 0,
            consumer: 0,
            modifiers: 0,
        }; 3],
    },
    all(0x0041, 0, 0x0226),
    all(0x0044, 0, 0x022d),
    all(0x0047, 0, 0x022e),
    all(0x004b, 0, 0x0230),
    all(0x004c, 0x46, 0),
    all(0x004d, 0x48, 0),
    all(0x004e, 0x47, 0),
    all(0x004f, 0x65, 0),
    all(0x0054, 0, 0x0224),
    all(0x0057, 0, 0x0225),
    all(0x00c0, 0x4e, 0),
    all(0x00c1, 0x4b, 0),
    all(0x0118, 0x4a, 0),
    all(0x0119, 0x4d, 0),
];
const _: () = assert!(CONTROLS.len() <= 128);

impl Control {
    pub fn translation(self, platform: HostPlatform) -> Option<Translation> {
        let value = self.platforms[match platform {
            HostPlatform::Linux => 0,
            HostPlatform::Windows => 1,
            HostPlatform::Mac => 2,
        }];
        (value != Translation::default()).then_some(value)
    }
}
pub fn control(id: u16, platform: HostPlatform) -> Option<(usize, Translation)> {
    CONTROLS.iter().enumerate().find_map(|(i, control)| {
        if control.id == id {
            control.translation(platform).map(|t| (i, t))
        } else {
            None
        }
    })
}
