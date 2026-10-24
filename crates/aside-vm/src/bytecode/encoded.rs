use super::{Error, Limits, VerifiedProgram};

/// Immutable bytecode bytes paired with the verified program they encode.
/// Container writers can inspect the program without decoding another copy.
pub struct EncodedProgram {
    program: VerifiedProgram,
    bytes: Vec<u8>,
}

impl VerifiedProgram {
    /// Serialize this program while retaining its shared verification result.
    pub fn encode(&self, limits: Limits) -> Result<EncodedProgram, Error> {
        Ok(EncodedProgram {
            bytes: super::codec::encode_verified(self, limits)?,
            program: self.clone(),
        })
    }
}

impl EncodedProgram {
    /// The verified program corresponding to these immutable bytes.
    pub fn program(&self) -> &VerifiedProgram {
        &self.program
    }

    /// Borrow the canonical wire representation.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Transfer the bytes and release the retained program reference.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
