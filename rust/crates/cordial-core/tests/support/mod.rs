use cordial_core::model::{errors::ErrorCode as Error, identifiers::Transport, link::PromptMethod};
use cordial_core::{
    bluetooth::{Bluetooth, Capabilities, Descriptor, Layout, ReportType},
    devices::{Peer, Policies, Policy},
    link::{LinkId, ServiceId, WriteId},
    manager::Manager,
    storage::{self, RecordKey, RecordStore},
};
use embassy_futures::block_on;
use std::{collections::BTreeMap, string::String};

#[derive(Default)]
pub struct Store {
    pub records: BTreeMap<RecordKey, Vec<u8>>,
    pub files: BTreeMap<String, Vec<u8>>,
    pub generation: u64,
    pub fail: bool,
    pub fail_remove: Option<RecordKey>,
    pub fail_save: Option<(RecordKey, bool)>,
    pub available: Option<usize>,
    pub capacity: Option<usize>,
    /// Allocation unit for records counted against `capacity`.
    pub block: Option<usize>,
    /// Record reads so far.
    pub loads: usize,
}
impl RecordStore for Store {
    async fn generation(&mut self) -> Result<u64, storage::Error> {
        Ok(self.generation)
    }
    async fn file_entry(
        &mut self,
        path: &str,
        index: usize,
    ) -> Result<Option<storage::FileEntry>, storage::Error> {
        if path != "/" {
            return Err(storage::Error::Bounds);
        }
        Ok(self
            .files
            .iter()
            .nth(index)
            .map(|(name, bytes)| storage::FileEntry {
                name: name.trim_start_matches('/').into(),
                kind: storage::FileType::File,
                size: bytes.len(),
            }))
    }
    async fn file_read(
        &mut self,
        path: &str,
        offset: u32,
        bytes: &mut [u8],
    ) -> Result<usize, storage::Error> {
        let file = self.files.get(path).ok_or(storage::Error::Io)?;
        let remaining = file.get(offset as usize..).ok_or(storage::Error::Bounds)?;
        let n = bytes.len().min(remaining.len());
        bytes[..n].copy_from_slice(&remaining[..n]);
        Ok(n)
    }

    async fn keys(&mut self) -> Result<Vec<RecordKey>, storage::Error> {
        if self.fail {
            return Err(storage::Error::Io);
        }
        Ok(self.records.keys().copied().collect())
    }
    async fn available(&mut self) -> Result<usize, storage::Error> {
        if self.fail {
            return Err(storage::Error::Io);
        }
        Ok(self
            .capacity
            .map(|n| {
                let block = self.block.unwrap_or(1);
                n.saturating_sub(
                    self.records
                        .values()
                        .map(|r| r.len().div_ceil(block) * block)
                        .sum::<usize>(),
                )
            })
            .unwrap_or(self.available.unwrap_or(65536)))
    }

    async fn load(
        &mut self,
        key: RecordKey,
        value: &mut [u8],
    ) -> Result<Option<usize>, storage::Error> {
        self.loads += 1;
        if self.fail {
            return Err(storage::Error::Io);
        }
        if self
            .records
            .get(&key)
            .is_some_and(|bytes| bytes.len() > value.len())
        {
            return Err(storage::Error::TooLarge);
        }
        Ok(self.records.get(&key).map(|bytes| {
            value[..bytes.len()].copy_from_slice(bytes);
            bytes.len()
        }))
    }
    async fn save(&mut self, key: RecordKey, value: &[u8]) -> Result<(), storage::Error> {
        if self.fail {
            return Err(storage::Error::Io);
        }
        if self.fail_save == Some((key, false)) {
            return Err(storage::Error::Io);
        }
        self.generation += 1;
        self.records.insert(key, value.to_vec());
        // The store resolves a post-commit I/O failure against authoritative data.
        Ok(())
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), storage::Error> {
        if self.fail || self.fail_remove == Some(key) {
            return Err(storage::Error::Io);
        }
        self.generation += 1;
        self.records.remove(&key);
        Ok(())
    }
}
#[derive(Default)]
pub struct Radio {
    pub reconnect: Vec<Peer>,
    pub info_refreshes: Vec<LinkId>,
    pub reject_info_refresh: bool,
    pub scans: Vec<(u64, bool, bool)>,
    pub bonds: Vec<Peer>,
    pub connects: Vec<(LinkId, bool)>,
    /// The saved layout supplied with each entry of `connects`.
    pub layouts: Vec<Option<Layout>>,
    pub closes: Vec<LinkId>,
    pub adopted: Vec<LinkId>,
    pub writes: Vec<(WriteId, Vec<u8>)>,
    pub transports: Option<Capabilities>,
    pub reject_adoption: bool,
    pub reject_forget: bool,
    pub reject_inventory: bool,
    pub addresses: Vec<Peer>,
    pub forgotten: Vec<Peer>,
    pub incoming: Vec<Option<LinkId>>,
    pub reject_connect: Option<Error>,
    /// Every transport enabled or disabled, in order.
    pub applied: Vec<(Transport, bool)>,
    pub reject_transport: Option<Error>,
}
impl Bluetooth for Radio {
    fn refresh_info(&mut self, link: LinkId) -> Result<(), Error> {
        if self.reject_info_refresh {
            return Err(Error::Busy);
        }
        self.info_refreshes.push(link);
        Ok(())
    }
    fn bond_capacity(&self, t: Transport) -> usize {
        if self.capabilities().supports(t) {
            8
        } else {
            0
        }
    }
    fn capabilities(&self) -> Capabilities {
        self.transports.unwrap_or(Capabilities {
            classic: true,
            ble: true,
            ble_scan_and_connect: false,
        })
    }
    fn set_transport(&mut self, transport: Transport, enabled: bool) -> Result<(), Error> {
        if let Some(error) = self.reject_transport {
            return Err(error);
        }
        self.applied.push((transport, enabled));
        Ok(())
    }
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error> {
        self.reconnect = peers.to_vec();
        Ok(())
    }
    fn scan(&mut self, id: u64, classic: bool, ble: bool) -> Result<(), Error> {
        self.scans.push((id, classic, ble));
        Ok(())
    }
    fn connect(
        &mut self,
        link: LinkId,
        peer: Peer,
        pairing: bool,
        layout: Option<&Layout>,
    ) -> Result<(), Error> {
        if let Some(error) = self.reject_connect {
            return Err(error);
        }
        self.addresses.push(peer);
        self.connects.push((link, pairing));
        self.layouts.push(layout.cloned());
        Ok(())
    }
    fn incoming(
        &mut self,
        _: u32,
        accept: Option<LinkId>,
        layout: Option<&Layout>,
    ) -> Result<(), Error> {
        self.incoming.push(accept);
        if let Some(id) = accept {
            self.connects.push((id, false));
            self.layouts.push(layout.cloned());
        } else {
            assert!(layout.is_none(), "a layout for a refused link");
        }
        Ok(())
    }
    fn disconnect(&mut self, link: LinkId) {
        self.closes.push(link);
    }
    fn adopt(&mut self, link: LinkId) -> Result<(), Error> {
        if self.reject_adoption {
            return Err(Error::ConnectionFailed);
        }
        self.adopted.push(link);
        Ok(())
    }
    fn pair_reply(
        &mut self,
        _: LinkId,
        _: PromptMethod,
        _: bool,
        _: Option<&str>,
    ) -> Result<(), Error> {
        Ok(())
    }
    fn can_write(&self, _: LinkId) -> bool {
        true
    }
    fn write(
        &mut self,
        id: WriteId,
        _: ServiceId,
        _: ReportType,
        _: Option<u8>,
        payload: &[u8],
    ) -> Result<(), Error> {
        self.writes.push((id, payload.to_vec()));
        Ok(())
    }
    fn read(
        &mut self,
        _: WriteId,
        _: ServiceId,
        _: ReportType,
        _: Option<u8>,
    ) -> Result<(), Error> {
        panic!("unexpected report read")
    }
    async fn import_bond(&mut self, bond: &cordial_core::bonds::Bond) -> Result<(), Error> {
        if !self.bonds.contains(&bond.identity) {
            self.bonds.push(bond.identity);
        }
        Ok(())
    }
    async fn export_bond(&mut self, identity: Peer) -> Result<cordial_core::bonds::Bond, Error> {
        Ok(bond(1, identity))
    }
    async fn bonds(&mut self) -> Result<Vec<Peer>, Error> {
        if self.reject_inventory {
            return Err(Error::StorageFailed);
        }
        Ok(self.bonds.clone())
    }
    async fn forget(&mut self, peer: Peer) -> Result<(), Error> {
        if self.reject_forget {
            return Err(Error::StorageFailed);
        }
        self.forgotten.push(peer);
        self.bonds.retain(|p| *p != peer);
        Ok(())
    }
}
pub fn peer(n: u8) -> Peer {
    Peer {
        address: [n; 6],
        random: false,
        transport: Transport::Classic,
    }
}
pub fn descriptor() -> Vec<Descriptor> {
    vec![
        Descriptor::from_slice(
            ServiceId(7),
            &[
                5, 1, 9, 6, 0xa1, 1, 5, 7, 0x19, 4, 0x29, 11, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8,
                0x81, 2, 0xc0,
            ],
        )
        .unwrap(),
    ]
}
pub fn setup() -> (Manager, Store, Radio) {
    let mut store = Store::default();
    block_on(storage::open(&mut store)).unwrap();
    all_transports(&mut store);
    // A saved device that finished setup with HID++ on.
    let mut policy = Policy::paired(77, peer(1), b"Keyboard");
    policy.hidpp_enabled = true;
    policy.setup_pending = false;
    policy.bond = 77;
    block_on(cordial_core::bonds::commit(
        &mut store,
        &policy,
        &bond(77, peer(1)),
    ))
    .unwrap();
    block_on(Policies { store: &mut store }.save(0, &policy)).unwrap();
    store
        .records
        .insert(storage::record_key(7, 0), b"77".to_vec());
    let mut radio = Radio {
        bonds: vec![peer(1)],
        ..Radio::default()
    };
    let mut manager = Manager::default();
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    manager.radio_ready = true;
    (manager, store, radio)
}

pub fn bond(owner: u64, identity: Peer) -> cordial_core::bonds::Bond {
    use cordial_core::bonds::*;
    Bond {
        owner,
        identity,
        complete: true,
        keys: if identity.transport == Transport::Classic {
            Keys::Classic {
                key: [42; 16],
                kind: 4,
            }
        } else {
            Keys::Ble {
                local: Security::default(),
                peer: Security {
                    flags: 1,
                    key_size: 16,
                    ltk: [42; 16],
                    ..Security::default()
                },
            }
        },
    }
}

/// Saves the adapter preference with every transport enabled.
pub fn all_transports(store: &mut Store) {
    let mut transports = cordial_core::devices::Transports::NONE;
    for transport in cordial_core::devices::Transports::ALL {
        transports.set(transport, true);
    }
    block_on(
        Policies { store }.save_adapter(&cordial_core::devices::AdapterPreference {
            transports,
            ..Default::default()
        }),
    )
    .unwrap();
}
