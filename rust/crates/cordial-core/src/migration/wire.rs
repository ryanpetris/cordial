//! Frozen outer v1 grammar and strict known-field protobuf checks.
use crate::storage::Error;
use prost::encoding::{DecodeContext, WireType, decode_key, decode_varint, skip_field};

#[derive(Clone, Copy)]
pub(super) enum Schema {
    Root,
    Identity,
    Adapter,
    Sequence,
    Device,
    Settings,
    Layout,
    Profile,
    Rules,
    Transport,
    Interface,
    Policy,
    Peer,
    Integration,
    OutputPreference,
    Bond,
    Classic,
    Ble,
    Security,
    Preference,
    Metadata,
    Range,
    Report,
    Rule,
    Remap,
    Output,
    Scale,
    Axis,
}
#[derive(Clone, Copy)]
enum Kind {
    U32,
    U64,
    Bool,
    Enum,
    Bytes,
    Text,
    Message(Schema),
}
#[derive(Clone, Copy)]
struct Spec {
    tag: u32,
    kind: Kind,
    repeated: bool,
    oneof: bool,
}
impl Spec {
    const fn new(tag: u32, kind: Kind) -> Self {
        Self {
            tag,
            kind,
            repeated: false,
            oneof: false,
        }
    }
    const fn many(mut self) -> Self {
        self.repeated = true;
        self
    }
    const fn oneof(mut self) -> Self {
        self.oneof = true;
        self
    }
}
macro_rules! specs { ($($tag:literal => $kind:expr $(, $flag:ident)?);* $(;)?) => {
    { const SPECS: &[Spec] = &[$(Spec::new($tag, $kind)$(.$flag())?),*]; SPECS }
}; }
fn specs(schema: Schema) -> &'static [Spec] {
    use Kind::*;
    use Schema::*;
    match schema {
        Root => specs![1=>U32; 2=>Bool],
        Identity => specs![1=>U32; 2=>Bytes; 3=>Bytes; 4=>Bytes; 5=>Bytes],
        Adapter => {
            specs![1=>U32; 2=>Text; 3=>Enum; 4=>Message(Transport),many; 5=>Message(Interface),many]
        }
        Sequence => specs![1=>U32; 2=>U64; 3=>U64],
        Device => specs![1=>U32; 2=>Message(Policy); 3=>Message(Bond)],
        Settings => specs![1=>U32; 2=>Message(Preference),many],
        Layout => specs![1=>U32; 2=>Bytes,many; 3=>Message(Report),many; 4=>Bytes],
        Profile => specs![1=>U32; 2=>Text; 3=>Enum,many],
        Rules => specs![1=>U32; 2=>Message(Rule),many],
        Transport => specs![1=>Enum; 2=>Bool],
        Interface => specs![1=>Enum; 2=>Bool; 3=>U64],
        Policy => {
            specs![1=>U64; 2=>Message(Peer); 3=>Text; 4=>Bool; 5=>Bool; 6=>Bool; 7=>Bool; 8=>Message(Integration),many; 9=>Enum,many; 10=>U64,many; 11=>Message(OutputPreference),many]
        }
        Peer => specs![1=>Bytes; 2=>Bool; 3=>Enum],
        Integration => specs![1=>Enum; 2=>Bool],
        OutputPreference => specs![1=>Enum; 2=>Enum],
        Bond => specs![1=>U64; 2=>Message(Peer); 3=>Message(Classic),oneof; 4=>Message(Ble),oneof],
        Classic => specs![1=>Bytes; 2=>U32],
        Ble => specs![1=>Message(Security); 2=>Message(Security)],
        Security => specs![1=>U32; 2=>U32; 3=>U32; 4=>Bytes; 5=>Bytes; 6=>Bytes; 7=>Bytes; 8=>U32],
        Preference => specs![1=>Enum; 2=>Message(Metadata); 3=>U32],
        Metadata => specs![1=>Text; 2=>U32; 3=>U32; 4=>Enum; 5=>U32,many; 6=>Message(Range)],
        Range => specs![1=>U32; 2=>U32; 3=>U32],
        Report => specs![1=>U32; 2=>Enum; 3=>U32; 4=>U32; 5=>U32; 6=>U32],
        Rule => {
            specs![1=>U32; 2=>U32; 3=>Message(Remap),oneof; 4=>Message(Scale),oneof; 5=>Message(Axis),oneof]
        }
        Remap => specs![1=>Message(Output),many],
        Output => specs![1=>U32; 2=>U32],
        Scale => specs![1=>U32; 2=>U32],
        Axis => specs![1=>U32; 2=>Bool],
    }
}

pub(super) struct Field<'a> {
    pub tag: u32,
    pub wire: WireType,
    /// Original bytes after the key, including a length prefix when present.
    pub value: &'a [u8],
    pub contents: &'a [u8],
}
pub(super) fn field<'a>(bytes: &mut &'a [u8]) -> Result<Field<'a>, Error> {
    let (tag, wire) = decode_key(bytes).map_err(|_| Error::Corrupt)?;
    let value = *bytes;
    let contents = if wire == WireType::LengthDelimited {
        let len = usize::try_from(decode_varint(bytes).map_err(|_| Error::Corrupt)?)
            .map_err(|_| Error::Corrupt)?;
        let contents = bytes.get(..len).ok_or(Error::Corrupt)?;
        *bytes = &bytes[len..];
        contents
    } else {
        skip_field(wire, tag, bytes, DecodeContext::default()).map_err(|_| Error::Corrupt)?;
        &value[..value.len() - bytes.len()]
    };
    // An end-group is only valid inside skip_field's matching start-group.
    if wire == WireType::EndGroup {
        return Err(Error::Corrupt);
    }
    Ok(Field {
        tag,
        wire,
        value: &value[..value.len() - bytes.len()],
        contents,
    })
}
fn scalar(kind: Kind, bytes: &mut &[u8]) -> Result<(), Error> {
    let value = decode_varint(bytes).map_err(|_| Error::Corrupt)?;
    match kind {
        Kind::Bool if value > 1 => Err(Error::Corrupt),
        Kind::U32 if value > u64::from(u32::MAX) => Err(Error::Corrupt),
        _ => Ok(()),
    }
}
/// Legacy outer tags are frozen. Unknown nested fields retain their protobuf bytes.
pub(super) fn validate(schema: Schema, mut bytes: &[u8], legacy: bool) -> Result<(), Error> {
    let specs = specs(schema);
    let mut seen = 0u32;
    let mut oneof = false;
    while !bytes.is_empty() {
        let f = field(&mut bytes)?;
        let tag = if legacy {
            f.tag.checked_add(1).ok_or(Error::Corrupt)?
        } else {
            f.tag
        };
        let Some((index, spec)) = specs.iter().enumerate().find(|(_, s)| s.tag == tag) else {
            if legacy {
                return Err(Error::Corrupt);
            }
            continue;
        };
        if (!spec.repeated && seen & (1 << index) != 0) || (spec.oneof && oneof) {
            return Err(Error::Corrupt);
        }
        seen |= 1 << index;
        oneof |= spec.oneof;
        match spec.kind {
            Kind::Bytes | Kind::Text | Kind::Message(_) => {
                if f.wire != WireType::LengthDelimited {
                    return Err(Error::Corrupt);
                }
                match spec.kind {
                    Kind::Text => {
                        core::str::from_utf8(f.contents).map_err(|_| Error::Corrupt)?;
                    }
                    Kind::Message(schema) => validate(schema, f.contents, false)?,
                    _ => {}
                }
            }
            kind => {
                let mut value = f.contents;
                if spec.repeated && f.wire == WireType::LengthDelimited {
                    while !value.is_empty() {
                        scalar(kind, &mut value)?;
                    }
                } else {
                    if f.wire != WireType::Varint {
                        return Err(Error::Corrupt);
                    }
                    scalar(kind, &mut value)?;
                }
            }
        }
    }
    Ok(())
}
