use crate::model::identifiers::HostPlatform;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Translation {
    pub key: u8,
    pub consumer: u16,
    pub modifiers: u8,
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
    }
}
const fn all(id: u16, key: u8, consumer: u16) -> Control {
    Control {
        id,
        platforms: [t(key, consumer, 0); 3],
    }
}
/// Standard HID usages and chords. No application-launch or text-entry sequences.
pub const CONTROLS: [Control; 17] = [
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
];
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
