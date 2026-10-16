//! BEP 55 holepunch extension (FluxDM patch).
//!
//! payload: msg_type u8 | addr_type u8 (0 = IPv4, 1 = IPv6) | addr 4/16 | port u16 BE | err_code u32 BE
//! The spec always carries err_code (0 outside errors); libtorrent only writes it on errors,
//! so it's optional when reading.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::MessageDeserializeError;

pub const RENDEZVOUS: u8 = 0;
pub const CONNECT: u8 = 1;
pub const ERROR: u8 = 2;

pub const ERR_NO_SUCH_PEER: u32 = 1;
pub const ERR_NOT_CONNECTED: u32 = 2;
pub const ERR_NO_SUPPORT: u32 = 3;
pub const ERR_NO_SELF: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtHolepunch {
    /// "Introduce me to this peer" (initiator → relay).
    Rendezvous(SocketAddr),
    /// "Open a uTP connection to this endpoint now" (relay → both peers).
    Connect(SocketAddr),
    /// The relay couldn't do the rendezvous for this endpoint.
    Error(SocketAddr, u32),
}

impl UtHolepunch {
    pub fn addr(&self) -> SocketAddr {
        match *self {
            UtHolepunch::Rendezvous(a) | UtHolepunch::Connect(a) | UtHolepunch::Error(a, _) => a,
        }
    }

    pub fn serialize(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        let (kind, err) = match *self {
            UtHolepunch::Rendezvous(_) => (RENDEZVOUS, 0),
            UtHolepunch::Connect(_) => (CONNECT, 0),
            UtHolepunch::Error(_, e) => (ERROR, e),
        };
        let addr = self.addr();
        out.write_all(&[kind])?;
        match addr.ip() {
            IpAddr::V4(ip) => {
                out.write_all(&[0])?;
                out.write_all(&ip.octets())?;
            }
            IpAddr::V6(ip) => {
                out.write_all(&[1])?;
                out.write_all(&ip.octets())?;
            }
        }
        out.write_all(&addr.port().to_be_bytes())?;
        out.write_all(&err.to_be_bytes())
    }

    pub fn deserialize(b: &[u8]) -> Result<Self, MessageDeserializeError> {
        let short = |need: usize| MessageDeserializeError::NotEnoughData(need, None);
        if b.len() < 2 {
            return Err(short(2));
        }
        let (kind, rest) = (b[0], &b[2..]);
        let (ip, rest): (IpAddr, &[u8]) = match b[1] {
            0 if rest.len() >= 6 => (Ipv4Addr::new(rest[0], rest[1], rest[2], rest[3]).into(), &rest[4..]),
            1 if rest.len() >= 18 => {
                let mut o = [0u8; 16];
                o.copy_from_slice(&rest[..16]);
                (Ipv6Addr::from(o).into(), &rest[16..])
            }
            0 => return Err(short(8)),
            1 => return Err(short(20)),
            _ => return Err(MessageDeserializeError::UtHolepunchInvalid("bad addr_type")),
        };
        let addr = SocketAddr::new(ip, u16::from_be_bytes([rest[0], rest[1]]));
        let err = rest.get(2..6).map(|e| u32::from_be_bytes([e[0], e[1], e[2], e[3]])).unwrap_or(0);
        match kind {
            RENDEZVOUS => Ok(UtHolepunch::Rendezvous(addr)),
            CONNECT => Ok(UtHolepunch::Connect(addr)),
            ERROR => Ok(UtHolepunch::Error(addr, err)),
            _ => Err(MessageDeserializeError::UtHolepunchInvalid("bad msg_type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_libtorrent_form() {
        for m in [
            UtHolepunch::Rendezvous("1.2.3.4:6881".parse().unwrap()),
            UtHolepunch::Connect("[2001:db8::1]:51413".parse().unwrap()),
            UtHolepunch::Error("5.6.7.8:1".parse().unwrap(), ERR_NOT_CONNECTED),
        ] {
            let mut v = Vec::new();
            m.serialize(&mut v).unwrap();
            assert_eq!(UtHolepunch::deserialize(&v).unwrap(), m);
        }
        // libtorrent leaves err_code out of non-error messages
        let lt = [CONNECT, 0, 10, 0, 0, 1, 0x1a, 0xe1];
        assert_eq!(UtHolepunch::deserialize(&lt).unwrap(), UtHolepunch::Connect("10.0.0.1:6881".parse().unwrap()));
        assert!(UtHolepunch::deserialize(&[CONNECT, 0, 10, 0]).is_err());
    }
}
