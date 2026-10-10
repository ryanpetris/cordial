//! Downloaded adapter records as JSON. Each saved file on the adapter holds one message from
//! `proto/storage.proto`; a download of a file whose path names its message is converted here,
//! with the standard protobuf JSON mapping and the schema's field names, and saved with `.json` in
//! place of `.pb`. The adapter sends the bytes as they are saved.
use cordial_protocol::storage as saved;
use std::path::{Path, PathBuf};

/// The saved records, by the message each file holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Record {
    Format,
    Identity,
    Adapter,
    Sequence,
    Device,
    Settings,
    Layout,
    Profile,
    Rules,
}

/// Why a download was saved without converting it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unconverted {
    /// The path names no saved record.
    Unknown,
    /// The file is empty, so it holds no record.
    Empty,
    /// The file does not decode as its record.
    Undecodable,
    /// The record holds an enum value this build has no name for, which protobuf JSON cannot
    /// show, such as one a newer firmware saved.
    Newer,
}
impl Unconverted {
    /// The reason, as a lowercase clause.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Unknown => "the file isn't a known record",
            Self::Empty => "the file is empty",
            Self::Undecodable => "the file doesn't decode as its record",
            Self::Newer => "the file holds values this version of Cordial doesn't know",
        }
    }
}

/// The record an adapter path holds, from the paths in `docs/storage-format.md`.
pub fn record(path: &str) -> Option<Record> {
    let root = match path {
        "/format.pb" => Some(Record::Format),
        "/identity.pb" => Some(Record::Identity),
        "/adapter.pb" => Some(Record::Adapter),
        "/sequence.pb" => Some(Record::Sequence),
        _ => None,
    };
    if root.is_some() {
        return root;
    }
    let mut parts = path.strip_prefix('/')?.split('/');
    let (directory, id, name) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match (directory, name) {
        ("devices", "device.pb") => Some(Record::Device),
        ("devices", "settings.pb") => Some(Record::Settings),
        ("devices", "layout.pb") => Some(Record::Layout),
        ("profiles", "profile.pb") => Some(Record::Profile),
        ("profiles", "rules.pb") => Some(Record::Rules),
        _ => None,
    }
}

fn json<M: prost::Message + Default + serde::Serialize>(
    bytes: &[u8],
) -> Result<Vec<u8>, Unconverted> {
    let message = M::decode(bytes).map_err(|_| Unconverted::Undecodable)?;
    let mut text = serde_json::to_vec_pretty(&message).map_err(|_| Unconverted::Newer)?;
    text.push(b'\n');
    Ok(text)
}

/// The record in `bytes` as pretty-printed protobuf JSON.
pub fn to_json(record: Record, bytes: &[u8]) -> Result<Vec<u8>, Unconverted> {
    if bytes.is_empty() {
        return Err(Unconverted::Empty);
    }
    match record {
        Record::Format => json::<saved::Format>(bytes),
        Record::Identity => json::<saved::Identity>(bytes),
        Record::Adapter => json::<saved::Adapter>(bytes),
        Record::Sequence => json::<saved::Sequence>(bytes),
        Record::Device => json::<saved::Device>(bytes),
        Record::Settings => json::<saved::Settings>(bytes),
        Record::Layout => json::<saved::Layout>(bytes),
        Record::Profile => json::<saved::Profile>(bytes),
        Record::Rules => json::<saved::Rules>(bytes),
    }
}

fn has_extension(local: &Path, extension: &str) -> bool {
    local.extension().is_some_and(|e| e == extension)
}

/// Where a converted download is saved: `local` with `.json` in place of `.pb`.
pub fn json_destination(local: &Path) -> PathBuf {
    if has_extension(local, "pb") {
        local.with_extension("json")
    } else {
        local.to_path_buf()
    }
}

/// Where an unconverted download of `path` is saved: `local`, with `.pb` in place of `.json` for an
/// adapter file named `.pb`, so the bytes keep their original name.
pub fn raw_destination(path: &str, local: &Path) -> PathBuf {
    if path.ends_with(".pb") && has_extension(local, "json") {
        local.with_extension("pb")
    } else {
        local.to_path_buf()
    }
}

/// Where a download of `path` to `local` is expected to be saved before its bytes arrive: as JSON
/// for a known record unless `raw`, otherwise as it is.
pub fn destination(path: &str, local: &Path, raw: bool) -> PathBuf {
    if !raw && record(path).is_some() {
        json_destination(local)
    } else if raw {
        local.to_path_buf()
    } else {
        raw_destination(path, local)
    }
}

/// What a download of `path` saves and where: the JSON of a known record, or else the bytes as
/// they are with the reason.
pub fn convert(
    path: &str,
    local: &Path,
    bytes: Vec<u8>,
) -> (PathBuf, Vec<u8>, Option<Unconverted>) {
    let converted = record(path)
        .ok_or(Unconverted::Unknown)
        .and_then(|record| to_json(record, &bytes));
    match converted {
        Ok(text) => (json_destination(local), text, None),
        Err(reason) => (raw_destination(path, local), bytes, Some(reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn paths_name_their_records() {
        for (path, expected) in [
            ("/format.pb", Some(Record::Format)),
            ("/identity.pb", Some(Record::Identity)),
            ("/adapter.pb", Some(Record::Adapter)),
            ("/sequence.pb", Some(Record::Sequence)),
            ("/devices/12/device.pb", Some(Record::Device)),
            ("/devices/12/settings.pb", Some(Record::Settings)),
            ("/devices/12/layout.pb", Some(Record::Layout)),
            ("/profiles/3/profile.pb", Some(Record::Profile)),
            ("/profiles/3/rules.pb", Some(Record::Rules)),
            ("/devices/12/rules.pb", None),
            ("/profiles/3/device.pb", None),
            ("/devices/x/device.pb", None),
            ("/devices//device.pb", None),
            ("/devices/1/device.pb/x", None),
            ("/devices/1", None),
            ("devices/1/device.pb", None),
            ("/format.json", None),
            ("/", None),
        ] {
            assert_eq!(record(path), expected, "{path}");
        }
    }

    #[test]
    fn known_records_become_protobuf_json() {
        let layout = saved::Layout {
            maps: vec![vec![5, 1, 9, 6]],
            reports: vec![saved::Report {
                service: 0,
                r#type: saved::ReportType::Input.into(),
                id: 1,
                value_handle: 16,
                properties: 18,
                cccd_handle: 17,
            }],
            database_hash: Vec::new(),
        };
        let (local, text, unconverted) = convert(
            "/devices/7/layout.pb",
            Path::new("out/layout.pb"),
            layout.encode_to_vec(),
        );
        assert_eq!(local, Path::new("out/layout.json"));
        assert_eq!(unconverted, None);
        let value: serde_json::Value = serde_json::from_slice(&text).unwrap();
        // Bytes are base64 and enums are named, with the schema's field names.
        assert_eq!(value["maps"][0], "BQEJBg==");
        assert_eq!(value["reports"][0]["type"], "REPORT_TYPE_INPUT");
        assert_eq!(value["reports"][0]["value_handle"], 16);
        // The JSON reads back as the same message.
        assert_eq!(
            serde_json::from_slice::<saved::Layout>(&text).unwrap(),
            layout
        );
        let sequence = saved::Sequence {
            next_device: 3,
            next_profile: 1,
        };
        let (_, text, _) = convert(
            "/sequence.pb",
            Path::new("sequence.pb"),
            sequence.encode_to_vec(),
        );
        // 64-bit integers are strings in protobuf JSON.
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({"next_device": "3", "next_profile": "1"})
        );
    }

    #[test]
    fn other_files_keep_their_bytes_and_original_name() {
        let bytes = vec![0xff];
        assert_eq!(
            convert(
                "/devices/7/device.pb",
                Path::new("device.json"),
                bytes.clone()
            ),
            (
                PathBuf::from("device.pb"),
                bytes.clone(),
                Some(Unconverted::Undecodable)
            )
        );
        assert_eq!(
            convert("/profiles/2/rules.pb", Path::new("rules.pb"), Vec::new()),
            (
                PathBuf::from("rules.pb"),
                Vec::new(),
                Some(Unconverted::Empty)
            )
        );
        // An enum value without a name in this build has no protobuf JSON form.
        let newer = saved::Profile {
            name: "Work".into(),
            roles: vec![99],
        }
        .encode_to_vec();
        assert_eq!(
            convert(
                "/profiles/2/profile.pb",
                Path::new("profile.pb"),
                newer.clone()
            ),
            (PathBuf::from("profile.pb"), newer, Some(Unconverted::Newer))
        );
        assert_eq!(
            convert("/notes.txt", Path::new("notes.txt"), bytes.clone()),
            (
                PathBuf::from("notes.txt"),
                bytes,
                Some(Unconverted::Unknown)
            )
        );
    }

    #[test]
    fn destinations_follow_the_conversion() {
        let local = Path::new("dir/device.pb");
        assert_eq!(
            destination("/devices/1/device.pb", local, false),
            Path::new("dir/device.json")
        );
        assert_eq!(destination("/devices/1/device.pb", local, true), local);
        assert_eq!(
            destination("/devices/1/device.pb", Path::new("copy"), false),
            Path::new("copy")
        );
        assert_eq!(
            destination("/other.pb", Path::new("other.json"), false),
            Path::new("other.pb")
        );
        assert_eq!(
            destination("/other.pb", Path::new("other.json"), true),
            Path::new("other.json")
        );
    }
}
