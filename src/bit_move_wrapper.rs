use serde::{Serialize, Serializer, Deserialize, Deserializer};
pub use pleco::BitMove;
use anyhow::Result;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BitMoveWrapper {
    bit_move: u16
}

impl BitMoveWrapper {
    pub fn new(bit_move: BitMove) -> Result<Self> {
        Ok(BitMoveWrapper{ bit_move: bit_move.get_raw() })
    }
}

impl From<BitMove> for BitMoveWrapper {
    fn from(bit_move: BitMove) -> Self {
        BitMoveWrapper { bit_move: bit_move.get_raw() }
    }
}

impl Into<BitMove> for BitMoveWrapper {
    fn into(self) -> BitMove {
        *self.clone()
    }
}

impl std::ops::Deref for BitMoveWrapper {
    type Target = BitMove;

    fn deref(&self) -> &Self::Target {
        &BitMove{ data: self.bit_move }
    }
}

impl std::ops::DerefMut for BitMoveWrapper {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut BitMove{ data: self.bit_move }
    }
}

// impl Serialize for BitMoveWrapper {
//     fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
//     where
//         S: Serializer,
//     {
//         serializer.serialize_u16(*self.bit_move.get_raw())
//     }
// }

// impl<'de> Deserialize<'de> for BitMoveWrapper {
//     fn deserialize<D>(deserializer: D) -> Result<BitMoveWrapper, D::Error>
//     where
//         D: Deserializer<'de>,
//     {
//         deserializer.deserialize_u16(U16Visitor)
//     }
// }

