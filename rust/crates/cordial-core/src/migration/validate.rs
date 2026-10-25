//! Borrowed semantic checks. Repeated compatibility entries never accumulate in a decoder.
use super::*;
use crate::model::{
    hidpp::{FeatureId, FeatureRevision},
    identifiers::Transport,
    settings::{SettingKey, SettingType},
};
use prost::Message;

fn narrow<T: TryFrom<u64>>(value: impl Into<u64>) -> Result<T, Error> {
    T::try_from(value.into()).map_err(|_| Error::Corrupt)
}
fn check(valid: bool) -> Result<(), Error> {
    if valid { Ok(()) } else { Err(Error::Corrupt) }
}
fn fields(mut bytes: &[u8]) -> impl Iterator<Item = Result<wire::Field<'_>, Error>> {
    core::iter::from_fn(move || {
        if bytes.is_empty() {
            return None;
        }
        let result = wire::field(&mut bytes);
        if result.is_err() {
            bytes = &[];
        }
        Some(result)
    })
}
fn get(bytes: &[u8], tag: u32) -> Result<Option<&[u8]>, Error> {
    for field in fields(bytes) {
        let field = field?;
        if field.tag == tag {
            return Ok(Some(field.contents));
        }
    }
    Ok(None)
}
fn required(bytes: &[u8], tag: u32) -> Result<&[u8], Error> {
    get(bytes, tag)?.ok_or(Error::Corrupt)
}
fn number(bytes: &[u8], tag: u32) -> Result<u64, Error> {
    get(bytes, tag)?.map_or(Ok(0), |mut value| {
        prost::encoding::decode_varint(&mut value).map_err(|_| Error::Corrupt)
    })
}
fn text(bytes: &[u8], tag: u32) -> Result<&str, Error> {
    core::str::from_utf8(get(bytes, tag)?.unwrap_or_default()).map_err(|_| Error::Corrupt)
}
fn array<const N: usize>(bytes: &[u8], tag: u32) -> Result<[u8; N], Error> {
    storage::array(required(bytes, tag)?)
}
fn scalars(bytes: &[u8], tag: u32) -> impl Iterator<Item = Result<u64, Error>> {
    fields(bytes)
        .filter_map(move |f| match f {
            Ok(f) if f.tag == tag => Some(Ok(f.contents)),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .flat_map(|result| {
            let mut bytes = result.unwrap_or_default();
            let mut error = result.err();
            core::iter::from_fn(move || {
                if let Some(e) = error.take() {
                    return Some(Err(e));
                }
                if bytes.is_empty() {
                    return None;
                }
                let result = prost::encoding::decode_varint(&mut bytes).map_err(|_| Error::Corrupt);
                if result.is_err() {
                    bytes = &[];
                }
                Some(result)
            })
        })
}
/// Only scalar-only generated messages are decoded here; none owns heap storage.
fn leaf<M: Message + Default + Copy>(bytes: &[u8]) -> Result<M, Error> {
    M::decode(bytes).map_err(|_| Error::Corrupt)
}

pub(super) fn sequence(bytes: &[u8]) -> Result<saved::Sequence, Error> {
    let value: saved::Sequence = leaf(bytes)?;
    let max = u64::from(u32::MAX) + 1;
    check((1..=max).contains(&value.next_device) && (1..=max).contains(&value.next_profile))?;
    Ok(value)
}
fn adapter(bytes: &[u8]) -> Result<(), Error> {
    if get(bytes, 2)?.is_some() {
        let name = text(bytes, 2)?;
        check(crate::model::adapter_name(name) == Some(name))?;
    }
    let mut transports = 0u8;
    let mut interfaces = [None; crate::interfaces::Interface::ALL.len()];
    let mut count = 0;
    for field in fields(bytes) {
        let f = field?;
        match f.tag {
            4 => {
                let p: saved::TransportPreference = leaf(f.contents)?;
                if devices::from_saved_transport(p.transport).is_some() {
                    let bit = 1 << p.transport;
                    check(transports & bit == 0)?;
                    transports |= bit;
                }
            }
            5 => {
                if let Some(p) =
                    crate::interfaces::InterfacePreference::from_saved(leaf(f.contents)?)
                {
                    check(count < interfaces.len())?;
                    check(
                        p.profile
                            .is_none_or(|id| (1..=u64::from(u32::MAX)).contains(&id)),
                    )?;
                    check(!p.enabled || p.profile.is_some())?;
                    for previous in interfaces[..count].iter().flatten() {
                        let previous: &crate::interfaces::InterfacePreference = previous;
                        check(
                            previous.interface != p.interface
                                && !(previous.enabled
                                    && p.enabled
                                    && p.interface.conflicts().contains(&previous.interface)),
                        )?;
                    }
                    interfaces[count] = Some(p);
                    count += 1;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
fn identity(bytes: &[u8]) -> Result<(), Error> {
    let mut identity = [0; 56];
    identity[0] = 7;
    identity[2..8].copy_from_slice(&array::<6>(bytes, 2)?);
    identity[8..24].copy_from_slice(&array::<16>(bytes, 3)?);
    identity[24..40].copy_from_slice(&array::<16>(bytes, 4)?);
    identity[40..].copy_from_slice(&array::<16>(bytes, 5)?);
    Identity::decode(identity).map(|_| ())
}
fn peer(bytes: &[u8]) -> Result<devices::Peer, Error> {
    Ok(devices::Peer {
        address: array(bytes, 1)?,
        random: number(bytes, 2)? != 0,
        transport: devices::from_saved_transport(number(bytes, 3)? as i32).ok_or(Error::Corrupt)?,
    })
}
fn security(bytes: &[u8]) -> Result<bonds::Security, Error> {
    Ok(bonds::Security {
        flags: narrow(number(bytes, 1)?)?,
        key_size: narrow(number(bytes, 2)?)?,
        ediv: narrow(number(bytes, 3)?)?,
        rand: array(bytes, 4)?,
        ltk: array(bytes, 5)?,
        irk: array(bytes, 6)?,
        csrk: array(bytes, 7)?,
        counter: narrow(number(bytes, 8)?)?,
    })
}
pub(super) fn device(bytes: &[u8], id: u64) -> Result<Transport, Error> {
    let policy = required(bytes, 2)?;
    let owner = number(policy, 1)?;
    let peer = peer(required(policy, 2)?)?;
    let name = text(policy, 3)?;
    check(owner == id && name.len() <= 128 && !name.bytes().any(|b| b < 0x20 || b == 0x7f))?;
    check(peer.transport != Transport::Classic || !peer.random)?;
    let (mut integration, mut output) = (false, false);
    for field in fields(policy) {
        let f = field?;
        match f.tag {
            8 => {
                let p: saved::IntegrationPreference = leaf(f.contents)?;
                if p.integration == saved::Integration::Hidpp as i32 {
                    check(!integration)?;
                    integration = true;
                }
            }
            11 if crate::outputs::Preference::from_saved(leaf(f.contents)?).is_some() => {
                check(!output)?;
                output = true;
            }
            _ => {}
        }
    }
    for (i, profile) in scalars(policy, 10).enumerate() {
        check(i < profiles::MAX_LAYERS && (1..=u64::from(u32::MAX)).contains(&profile?))?;
    }
    let bond = required(bytes, 3)?;
    let keys = if let Some(classic) = get(bond, 3)? {
        bonds::Keys::Classic {
            key: array(classic, 1)?,
            kind: narrow(number(classic, 2)?)?,
        }
    } else {
        let ble = required(bond, 4)?;
        bonds::Keys::Ble {
            local: security(required(ble, 1)?)?,
            peer: security(required(ble, 2)?)?,
        }
    };
    let bond = bonds::Bond {
        owner: number(bond, 1)?,
        identity: self::peer(required(bond, 2)?)?,
        complete: true,
        keys,
    };
    check(bond.valid() && bond.owner == owner && bond.identity == peer)?;
    Ok(peer.transport)
}
fn settings(bytes: &[u8]) -> Result<(), Error> {
    let mut any = false;
    let mut seen = 0u64;
    for field in fields(bytes) {
        let f = field?;
        if f.tag != 2 {
            continue;
        }
        any = true;
        if number(f.contents, 1)? as i32 != saved::Integration::Hidpp as i32 {
            continue;
        }
        let metadata = required(f.contents, 2)?;
        let key = SettingKey::from_name(text(metadata, 1)?).ok_or(Error::Corrupt)?;
        let bit = 1u64 << key as u8;
        check(seen & bit == 0)?;
        seen |= bit;
        preference(key, metadata, narrow(number(f.contents, 3)?)?)?;
    }
    check(any)
}
fn preference(key: SettingKey, bytes: &[u8], value: u16) -> Result<(), Error> {
    let feature = FeatureId(narrow(number(bytes, 2)?)?);
    let revision = FeatureRevision(narrow(number(bytes, 3)?)?);
    let expected_scope = if matches!(
        feature,
        FeatureId::FN_INVERSION_MULTI_HOST | FeatureId::MULTI_PLATFORM
    ) {
        saved::SettingScope::CurrentHost
    } else {
        saved::SettingScope::Device
    };
    check(
        number(bytes, 4)? as i32 == expected_scope as i32
            && key.writable_feature(feature, revision),
    )?;
    let mut largest = 0u16;
    let mut count = 0;
    let mut accepts = false;
    for choice in scalars(bytes, 5) {
        let choice: u16 = narrow(choice?)?;
        count += 1;
        check(count <= crate::settings::MAX_CHOICES)?;
        largest = largest.max(choice);
        match key.kind() {
            SettingType::Integer => check((1..0xe000).contains(&choice))?,
            SettingType::Enum => {
                let index = choice
                    .checked_sub(u16::from(key == SettingKey::WheelMode))
                    .ok_or(Error::Corrupt)?;
                check(usize::from(index) < key.enum_values().len())?;
                check(key != SettingKey::BacklightMode || matches!(index, 1 | 3))?;
            }
            _ => return Err(Error::Corrupt),
        }
        accepts |= choice == value;
    }
    check(!matches!(key, SettingKey::PointerDpi0 | SettingKey::PointerDpi1) || count <= 6)?;
    let range = get(bytes, 6)?;
    if count != 0 {
        check(range.is_none() && accepts)?;
        // Scan the u16 domain in 2048-value windows: at most 32 passes and 256 stack bytes,
        // regardless of the number of choices. Small settings use one window.
        for window in 0..=usize::from(largest) / 2048 {
            let mut seen = [0u64; 32];
            for choice in scalars(bytes, 5) {
                let choice = choice? as usize;
                if choice / 2048 != window {
                    continue;
                }
                let slot = &mut seen[(choice % 2048) / 64];
                let bit = 1 << (choice % 64);
                check(*slot & bit == 0)?;
                *slot |= bit;
            }
        }
        return Ok(());
    }
    match key.kind() {
        SettingType::Bool => check(range.is_none() && value <= 1),
        SettingType::Integer => {
            let r: saved::Range = leaf(range.ok_or(Error::Corrupt)?)?;
            let min: u16 = narrow(r.min)?;
            let max: u16 = narrow(r.max)?;
            let step: u16 = narrow(r.step)?;
            check(min <= max && step != 0 && (max - min).is_multiple_of(step))?;
            check(match key {
                SettingKey::PointerDpi0 | SettingKey::PointerDpi1 => {
                    min != 0 && max < 0xe000 && step <= 0x1fff
                }
                SettingKey::BacklightLevel => min == 0 && max <= 7 && step == 1,
                SettingKey::BacklightDelayHandsIn
                | SettingKey::BacklightDelayHandsOut
                | SettingKey::BacklightDelayPowered => (min, max, step) == (5, 7200, 5),
                SettingKey::PowerAutoOff => (min, max, step) == (0, 15300, 60),
                SettingKey::WheelThreshold => (min, max, step) == (1, 255, 1),
                _ => true,
            })?;
            check(value >= min && value <= max && (value - min).is_multiple_of(step))
        }
        _ => Err(Error::Corrupt),
    }
}
pub(super) fn layout(bytes: &[u8], transport: Transport) -> Result<(), Error> {
    use crate::bluetooth::{LAYOUT_REPORTS, LAYOUT_SERVICES};
    let mut services = 0;
    let mut reports = [saved::Report::default(); LAYOUT_REPORTS];
    let mut count = 0;
    for field in fields(bytes) {
        let f = field?;
        match f.tag {
            2 => {
                services += 1;
                check(
                    services <= LAYOUT_SERVICES
                        && !f.contents.is_empty()
                        && f.contents.len() <= crate::hid::DESCRIPTOR_BYTES,
                )?;
            }
            3 => {
                check(count < reports.len())?;
                let report: saved::Report = leaf(f.contents)?;
                check(
                    (1..=3).contains(&report.r#type)
                        && report.service <= u16::MAX.into()
                        && report.id <= u8::MAX.into()
                        && report.value_handle <= u16::MAX.into()
                        && report.properties <= u8::MAX.into()
                        && report.cccd_handle <= u16::MAX.into(),
                )?;
                reports[count] = report;
                count += 1;
            }
            4 => check(f.contents.is_empty() || f.contents.len() == 16)?,
            _ => {}
        }
    }
    check(services != 0)?;
    if transport == Transport::Classic {
        return check(services == 1 && count == 0);
    }
    let reports = &reports[..count];
    check(
        reports
            .iter()
            .any(|r| r.r#type == saved::ReportType::Input as i32),
    )?;
    for (i, r) in reports.iter().enumerate() {
        check(
            (r.service as usize) < services
                && r.value_handle != 0
                && (r.cccd_handle == 0 || r.cccd_handle > r.value_handle)
                && !reports[..i].iter().any(|other| {
                    (other.service, other.r#type, other.id) == (r.service, r.r#type, r.id)
                }),
        )?;
    }
    Ok(())
}
fn rules(bytes: &[u8]) -> Result<(), Error> {
    let mut previous = None;
    let (mut outputs, mut scales, mut axes) = (0usize, 0usize, 0usize);
    for field in fields(bytes) {
        let f = field?;
        if f.tag != 2 {
            continue;
        }
        let input = narrow(number(f.contents, 1)?)?;
        let collection = narrow(number(f.contents, 2)?)?;
        check(previous.is_none_or(|p| p < (input, collection)))?;
        previous = Some((input, collection));
        let ranges: &[profiles::Range] = if let Some(remap) = get(f.contents, 3)? {
            check(outputs <= u16::MAX.into())?;
            let mut last = None;
            let mut count = 0;
            for output in fields(remap) {
                let output = output?;
                if output.tag != 1 {
                    continue;
                }
                count += 1;
                check(count <= profiles::MAX_REMAP_OUTPUTS)?;
                let o: saved::Output = leaf(output.contents)?;
                check(
                    last.is_none_or(|p| p < (o.usage, o.collection))
                        && profiles::REMAP_OUTPUTS
                            .iter()
                            .any(|r| r.collection == o.collection && r.contains(o.usage)),
                )?;
                last = Some((o.usage, o.collection));
            }
            outputs += count;
            &profiles::REMAP_INPUTS
        } else if let Some(scale) = get(f.contents, 4)? {
            check(scales <= u16::MAX.into())?;
            scales += 1;
            let s: saved::Scale = leaf(scale)?;
            check(
                s.numerator != 0
                    && s.denominator != 0
                    && profiles::gcd(s.numerator.unsigned_abs().into(), s.denominator.into()) == 1,
            )?;
            &profiles::SCALE_INPUTS
        } else {
            check(axes <= u16::MAX.into())?;
            axes += 1;
            let a: saved::Axis = leaf(required(f.contents, 5)?)?;
            check(
                profiles::AXIS_OUTPUTS
                    .iter()
                    .any(|r| r.collection == collection && r.contains(a.destination)),
            )?;
            &profiles::AXIS_INPUTS
        };
        check(
            ranges
                .iter()
                .any(|r| r.collection == collection && r.contains(input)),
        )?;
    }
    check(previous.is_some())
}
pub(super) fn record(key: RecordKey, bytes: &[u8]) -> Result<(), Error> {
    match key[0] {
        1 => adapter(bytes),
        2 => device(bytes, u64::from_be_bytes(key[1..].try_into().unwrap())).map(|_| ()),
        3 => identity(bytes),
        4 => settings(bytes),
        5 => layout(bytes, Transport::Classic).or_else(|_| layout(bytes, Transport::Ble)),
        7 => sequence(bytes).map(|_| ()),
        8 => check(profiles::name_valid(text(bytes, 2)?)),
        9 => rules(bytes),
        _ => Err(Error::Bounds),
    }
}
