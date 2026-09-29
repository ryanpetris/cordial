//! Common adapter roots. Bluetooth keys use most-significant byte first; the
//! adapters reverse native little-endian keys at their boundaries.
use crate::storage::{Error, RecordStore};
use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit},
};
pub const KEY: [u8; 9] = [3, 0, 0, 0, 0, 0, 0, 0, 0];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity(pub [u8; 56]);
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(with = "crate::hex")]
    address: [u8; 6],
    #[serde(with = "crate::hex")]
    ir: [u8; 16],
    #[serde(with = "crate::hex")]
    er: [u8; 16],
    #[serde(with = "crate::hex")]
    irk: [u8; 16],
}
impl Identity {
    fn irk(ir: &[u8]) -> [u8; 16] {
        let cipher = Aes128::new_from_slice(ir).unwrap();
        let mut block = aes::cipher::generic_array::GenericArray::default();
        block[15] = 1;
        cipher.encrypt_block(&mut block);
        block.into()
    }
    pub fn decode(bytes: [u8; 56]) -> Result<Self, Error> {
        if bytes[0] != 7 || bytes[1] != 0 || Self::irk(&bytes[8..24]) != bytes[40..56] {
            return Err(Error::Corrupt);
        }
        Ok(Self(bytes))
    }
    pub async fn load<S: RecordStore>(store: &mut S) -> Result<Option<Self>, Error> {
        let Some(json) = store.load_owned(KEY).await? else {
            return Ok(None);
        };
        let d: Document = serde_json::from_slice(&json).map_err(|_| Error::Corrupt)?;
        let mut bytes = [0; 56];
        bytes[0] = 7;
        bytes[2..8].copy_from_slice(&d.address);
        bytes[8..24].copy_from_slice(&d.ir);
        bytes[24..40].copy_from_slice(&d.er);
        bytes[40..56].copy_from_slice(&d.irk);
        Self::decode(bytes).map(Some)
    }
    pub async fn initialize<S: RecordStore>(
        store: &mut S,
        address: [u8; 6],
        mut random: impl FnMut() -> u64,
    ) -> Result<Self, Error> {
        crate::storage::open(store).await?;
        if let Some(id) = Self::load(store).await? {
            if id.0[2..8] != address {
                return Err(Error::Layout);
            }
            crate::storage::initialized(store).await?;
            return Ok(id);
        }
        if crate::storage::format(store).await?.initialized {
            return Err(Error::Corrupt);
        }
        // Missing roots in a populated store are a storage fault, not a new
        // adapter identity. Never silently invalidate existing peer identities.
        let mut previous = None;
        while let Some(key) = store.next_key(previous).await? {
            previous = Some(key);
            if key[0] == 2 || key[0] == 7 {
                return Err(Error::Corrupt);
            }
        }
        let mut bytes = [0; 56];
        bytes[0] = 7;
        bytes[2..8].copy_from_slice(&address);
        for chunk in bytes[8..40].as_chunks_mut::<8>().0 {
            chunk.copy_from_slice(&random().to_be_bytes());
        }
        let irk = Self::irk(&bytes[8..24]);
        bytes[40..56].copy_from_slice(&irk);
        store
            .save(
                KEY,
                &crate::storage::json(&Document {
                    address,
                    ir: bytes[8..24].try_into().unwrap(),
                    er: bytes[24..40].try_into().unwrap(),
                    irk,
                })?,
            )
            .await?;
        crate::storage::initialized(store).await?;
        Ok(Self(bytes))
    }
}
