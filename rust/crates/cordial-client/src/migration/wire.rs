//! Allocation-free checks before Prost allocates inventory entries.
use prost::encoding::{DecodeContext, WireType, decode_key, decode_varint, skip_field};

fn fields(mut bytes: &[u8], mut visit: impl FnMut(u32, &[u8]) -> Option<()>) -> Option<()> {
    while !bytes.is_empty() {
        let (tag, wire) = decode_key(&mut bytes).ok()?;
        if wire == WireType::LengthDelimited {
            let n = usize::try_from(decode_varint(&mut bytes).ok()?).ok()?;
            let value = bytes.get(..n)?;
            bytes = bytes.get(n..)?;
            visit(tag, value)?;
        } else {
            skip_field(wire, tag, &mut bytes, DecodeContext::default()).ok()?;
        }
    }
    Some(())
}

pub(crate) fn check_file_lists(bytes: &[u8]) -> Result<(), ()> {
    let mut entries = 0usize;
    fields(bytes, |tag, response| {
        if tag == 1 {
            fields(response, |tag, list| {
                if tag == 10 {
                    fields(list, |tag, entry| {
                        if tag == 1 {
                            entries = entries.checked_add(1)?;
                            if entries > 2 * cordial_protocol::STORAGE_MIGRATION_MAX_FILES {
                                return None;
                            }
                            fields(entry, |tag, name| {
                                (tag != 1
                                    || name.len() <= cordial_protocol::STORAGE_MIGRATION_PATH_BYTES)
                                    .then_some(())
                            })?;
                        }
                        Some(())
                    })?;
                }
                Some(())
            })?;
        }
        Some(())
    })
    .ok_or(())
}

/// Migration has no event consumer. Ignore events before decoding their unbounded repeated
/// payloads, and bound all non-list response envelopes independently of the general client.
pub(crate) fn migration_response(bytes: &[u8]) -> Result<bool, ()> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(());
    }
    let mut response_seen = false;
    let mut event_seen = false;
    let mut nonlist_bytes = 0usize;
    fields(bytes, |tag, response| {
        if tag == 2 {
            event_seen = true;
        }
        if tag == 1 {
            response_seen = true;
            fields(response, |tag, value| {
                if tag != 10 {
                    nonlist_bytes = nonlist_bytes.checked_add(value.len())?;
                    if nonlist_bytes > 4096 || !matches!(tag, 1 | 2 | 11) {
                        return None;
                    }
                }
                Some(())
            })?;
        }
        Some(())
    })
    .ok_or(())?;
    Ok(response_seen && !event_seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_protocol as p;
    use prost::Message;

    fn list(entries: &[u8]) -> Vec<u8> {
        let mut response = vec![];
        prost::encoding::bytes::encode(10, &entries.to_vec(), &mut response);
        let mut message = vec![];
        prost::encoding::bytes::encode(1, &response, &mut message);
        message
    }

    #[test]
    fn inventory_cap_counts_entries_across_merged_envelopes_before_decoding() {
        let limit = 2 * p::STORAGE_MIGRATION_MAX_FILES;
        // Empty repeated messages amplify most when decoded into generated FileEntry values.
        let mut bytes = list(&[0x0a, 0].repeat(limit));
        assert_eq!(check_file_lists(&bytes), Ok(()));
        bytes.extend(list(&[0x0a, 0]));
        assert_eq!(check_file_lists(&bytes), Err(()));
    }

    #[test]
    fn path_lengths_are_checked_before_file_entry_allocation() {
        let message = p::Message {
            kind: Some(p::message::Kind::Response(p::Response {
                result: Some(p::response::Result::Files(p::FileList {
                    entries: vec![p::FileEntry {
                        name: "x".repeat(p::STORAGE_MIGRATION_PATH_BYTES + 1),
                        ..Default::default()
                    }],
                })),
            })),
        }
        .encode_to_vec();
        assert_eq!(check_file_lists(&message), Err(()));
    }

    #[test]
    fn unsolicited_events_are_skipped_without_decoding_their_payloads() {
        let mut bytes = vec![];
        prost::encoding::bytes::encode(2, &vec![0xff; 100_000], &mut bytes);
        assert_eq!(migration_response(&bytes), Ok(false));
        assert_eq!(migration_response(&[0x0a, 0]), Ok(true));
    }
}
