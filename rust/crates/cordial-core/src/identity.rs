//! Common adapter roots. Bluetooth keys use most-significant byte first; the
//! adapters reverse native little-endian keys at their boundaries.
use crate::storage::{self, Error, RecordStore};
use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit},
};
use cordial_protocol::storage as saved;
pub const KEY: [u8; 9] = [3, 0, 0, 0, 0, 0, 0, 0, 0];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity(pub [u8; 56]);
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
        let Some(record) = store.load_owned(KEY).await? else {
            return Ok(None);
        };
        let saved: saved::Identity = storage::decode(&record)?;
        let mut bytes = [0; 56];
        bytes[0] = 7;
        bytes[2..8].copy_from_slice(&storage::array::<6>(&saved.address)?);
        bytes[8..24].copy_from_slice(&storage::array::<16>(&saved.ir)?);
        bytes[24..40].copy_from_slice(&storage::array::<16>(&saved.er)?);
        bytes[40..56].copy_from_slice(&storage::array::<16>(&saved.irk)?);
        Self::decode(bytes).map(Some)
    }
    pub async fn initialize<S: RecordStore>(
        store: &mut S,
        address: [u8; 6],
        mut random: impl FnMut() -> u64,
    ) -> Result<Self, Error> {
        storage::open(store).await?;
        if let Some(id) = Self::load(store).await? {
            if id.0[2..8] != address {
                return Err(Error::Layout);
            }
            storage::initialized(store).await?;
            return Ok(id);
        }
        if storage::format(store).await?.initialized {
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
                &storage::encode(&saved::Identity {
                    address: address.into(),
                    ir: bytes[8..24].into(),
                    er: bytes[24..40].into(),
                    irk: irk.into(),
                })?,
            )
            .await?;
        storage::initialized(store).await?;
        Ok(Self(bytes))
    }
}
