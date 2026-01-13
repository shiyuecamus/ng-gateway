use bytes::{BufMut, Bytes};

/// Unified wire encoding trait across layers
pub trait WireEncode {
    type Error: std::fmt::Debug + Send + Sync + 'static;
    type Context;

    fn encoded_len(&self, ctx: &Self::Context) -> usize;
    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error>;
}

/// Unified zero-copy wire decoding trait across layers
pub trait WireDecode: Sized {
    type Error: std::fmt::Debug + Send + Sync + 'static;
    type Context;

    /// Parse from `input`, returning the remaining slice and the parsed value.
    /// `parent` is used to permit zero-copy `Bytes::slice_ref` construction.
    fn parse<'a>(
        input: &'a [u8],
        parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error>;
}
