//! Saved HID layouts. Each bonded device's file is an optional cache that lets
//! a later connection admit input without rediscovering the device. A missing,
//! unreadable or unusable file means the backend discovers the device instead.
use crate::bluetooth::{DatabaseHash, Layout, LayoutReport, ReportMap, ReportType};
use crate::model::identifiers::Transport;
use crate::storage::{self, Error, RecordKey, RecordStore, record_key};
use alloc::vec::Vec;
use cordial_protocol::storage as saved;
use prost::Message;

fn key(device: u64) -> RecordKey {
    record_key(5, device)
}

fn saved_report(report: &LayoutReport) -> saved::Report {
    saved::Report {
        service: report.service.into(),
        r#type: match report.kind {
            ReportType::Input => saved::ReportType::Input,
            ReportType::Output => saved::ReportType::Output,
            ReportType::Feature => saved::ReportType::Feature,
        }
        .into(),
        id: report.id.into(),
        value_handle: report.value.into(),
        properties: report.properties.into(),
        cccd_handle: report.cccd.into(),
    }
}
fn report(report: &saved::Report) -> Result<LayoutReport, Error> {
    Ok(LayoutReport {
        service: storage::narrow(report.service)?,
        kind: match saved::ReportType::try_from(report.r#type) {
            Ok(saved::ReportType::Input) => ReportType::Input,
            Ok(saved::ReportType::Output) => ReportType::Output,
            Ok(saved::ReportType::Feature) => ReportType::Feature,
            _ => return Err(Error::Corrupt),
        },
        id: storage::narrow(report.id)?,
        value: storage::narrow(report.value_handle)?,
        properties: storage::narrow(report.properties)?,
        cccd: storage::narrow(report.cccd_handle)?,
    })
}

/// The saved form of `layout`. Its report maps are written straight from the layout, followed by
/// the rest of the message.
pub fn encode(layout: &Layout) -> Result<Vec<u8>, Error> {
    let rest = saved::Layout {
        maps: Vec::new(),
        reports: layout.reports.iter().map(saved_report).collect(),
        database_hash: layout.hash.map_or_else(Vec::new, |hash| hash.0.into()),
    };
    let maps: usize = layout
        .maps
        .iter()
        .map(|map| prost::encoding::bytes::encoded_len(1, &map.0))
        .sum();
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(maps + rest.encoded_len())
        .map_err(|_| Error::Unavailable)?;
    for map in &layout.maps {
        prost::encoding::bytes::encode(1, &map.0, &mut bytes);
    }
    rest.encode(&mut bytes).map_err(|_| Error::TooLarge)?;
    Ok(bytes)
}
/// The layout a saved file holds, not yet checked for use.
pub fn decode(bytes: &[u8]) -> Result<Layout, Error> {
    let saved: saved::Layout = storage::decode(bytes)?;
    Ok(Layout {
        maps: saved.maps.into_iter().map(ReportMap).collect(),
        reports: saved.reports.iter().map(report).collect::<Result<_, _>>()?,
        hash: match saved.database_hash.as_slice() {
            [] => None,
            hash => Some(DatabaseHash(storage::array(hash)?)),
        },
    })
}

/// The device's saved layout when it is usable for `transport`. A file that
/// does not decode or is unusable is removed.
pub async fn load<S: RecordStore>(
    store: &mut S,
    device: u64,
    transport: Transport,
) -> Option<Layout> {
    let bytes = store.load_owned(key(device)).await.ok()??;
    match decode(&bytes) {
        Ok(layout) if layout.valid(transport) => Some(layout),
        _ => {
            let _ = store.remove(key(device)).await;
            None
        }
    }
}

/// Saves a usable layout, keeping the space reserved for maintenance and for
/// pairing another device free. The store skips writing bytes equal to the
/// saved file. A layout that is not saved removes the saved one, which no
/// longer describes the device. Returns whether the layout was saved.
pub async fn save<S: RecordStore>(
    store: &mut S,
    device: u64,
    transport: Transport,
    layout: &Layout,
) -> bool {
    if layout.valid(transport)
        && let Ok(bytes) = encode(layout)
        && store.available().await.is_ok_and(|available| {
            available >= crate::bonds::MAINTENANCE_BYTES + crate::bonds::PAIR_BYTES + bytes.len()
        })
        && store.save(key(device), &bytes).await.is_ok()
    {
        return true;
    }
    remove(store, device).await;
    false
}

/// A fingerprint of a layout's report maps. Layouts with equal maps differ at
/// most in their report characteristics, which the backend routes by itself.
pub fn maps(layout: &Layout) -> u64 {
    // FNV-1a over each map's length and bytes.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for map in &layout.maps {
        for byte in (map.0.len() as u32).to_le_bytes().iter().chain(&map.0) {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

pub async fn remove<S: RecordStore>(store: &mut S, device: u64) {
    let _ = store.remove(key(device)).await;
}
