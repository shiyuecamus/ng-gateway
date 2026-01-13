pub mod apci;
pub mod asdu;
pub mod cproc;
pub mod csys;
pub mod mproc;
pub mod msys;
pub mod time;

use self::{apci::Apci, asdu::Asdu};
use std::fmt::Display;

#[derive(Debug)]
pub struct Apdu {
    pub apci: Apci,
    pub asdu: Option<Asdu>,
}

impl Display for Apdu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.apci.to_string().as_str())?;
        if let Some(asdu) = &self.asdu {
            f.write_str(asdu.to_string().as_str())?;
        }
        Ok(())
    }
}
