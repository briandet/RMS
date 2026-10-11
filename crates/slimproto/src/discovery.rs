//! UDP discovery on port 3483.
//!
//! Two request styles exist:
//!
//! * Legacy `'d'` request (Squeezebox 1/2/3, Transporter, Boom):
//!   `d | pad | u8 device id | u8 revision | 8 pad | 6 MAC`, answered with
//!   `D | 17-byte server name (ISO-8859-1, NUL padded)`.
//! * TLV `'e'` request (SqueezePlay, squeezelite, WiiM, Squeezer): `e` followed by
//!   `4-char tag | u8 length | value` entries, answered with `E` followed by the same TLV shape
//!   for every tag the server has a value for (`NAME`, `IPAD`, `JSON`, `VERS`, `UUID`).
//!
//! `JVID` is sent by Jive controllers (their MAC) and gets no response value.

use bytes::{BufMut, BytesMut};

/// A parsed incoming discovery datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryRequest {
    Legacy {
        device_id: u8,
        revision: u8,
        mac: [u8; 6],
    },
    Tlv(Vec<([u8; 4], Vec<u8>)>),
    /// `h`/`i`/`r`/`2`/`a` — SliMP3 protocol, unsupported.
    Slimp3,
    /// An `E` response from another server (server discovery).
    ServerResponse(Vec<([u8; 4], Vec<u8>)>),
    Unknown,
}

impl DiscoveryRequest {
    pub fn parse(msg: &[u8]) -> DiscoveryRequest {
        let Some(&first) = msg.first() else {
            return DiscoveryRequest::Unknown;
        };
        match first {
            b'd' => {
                if msg.len() < 18 {
                    return DiscoveryRequest::Unknown;
                }
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&msg[12..18]);
                DiscoveryRequest::Legacy {
                    device_id: msg[2],
                    revision: msg[3],
                    mac,
                }
            }
            b'e' => DiscoveryRequest::Tlv(parse_tlv(&msg[1..])),
            b'E' => DiscoveryRequest::ServerResponse(parse_tlv(&msg[1..])),
            b'i' | b'r' | b'2' | b'a' => DiscoveryRequest::Slimp3,
            b'h' if !(msg.len() >= 3 && msg[1] == 0 && msg[2] == 0) => DiscoveryRequest::Slimp3,
            _ => DiscoveryRequest::Unknown,
        }
    }
}

fn parse_tlv(mut p: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    let mut out = Vec::new();
    while p.len() >= 5 {
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&p[0..4]);
        let len = p[4] as usize;
        let end = (5 + len).min(p.len());
        out.push((tag, p[5..end].to_vec()));
        p = &p[end..];
    }
    out
}

/// Server-side values advertised in discovery responses.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    /// Library / server name as shown on the player.
    pub name: String,
    /// HTTP address (string form), only sent when configured explicitly.
    pub http_addr: Option<String>,
    /// HTTP (JSON) port.
    pub http_port: u16,
    pub version: String,
    pub uuid: String,
}

impl ServerInfo {
    /// 17-byte legacy name field: at most 16 ISO-8859-1 bytes then NUL padding.
    pub fn legacy_name(&self) -> [u8; 17] {
        let mut out = [0u8; 17];
        for (n, ch) in self.name.chars().take(16).enumerate() {
            let c = ch as u32;
            out[n] = if c <= 0xff { c as u8 } else { b'?' };
        }
        out
    }

    /// Build the response to a request, or `None` when the request needs no answer.
    pub fn respond(&self, req: &DiscoveryRequest) -> Option<Vec<u8>> {
        match req {
            DiscoveryRequest::Legacy { device_id, .. } => {
                let mut out = Vec::with_capacity(18);
                out.push(b'D');
                if *device_id == 1 {
                    out.extend_from_slice(&[0u8; 17]);
                } else {
                    out.extend_from_slice(&self.legacy_name());
                }
                Some(out)
            }
            DiscoveryRequest::Tlv(items) => {
                let mut out = BytesMut::with_capacity(128);
                out.put_u8(b'E');
                for (tag, _value) in items {
                    let v: Option<String> = match tag {
                        b"NAME" => Some(self.name.clone()),
                        b"IPAD" => self.http_addr.clone(),
                        b"JSON" => Some(self.http_port.to_string()),
                        b"VERS" => Some(self.version.clone()),
                        b"UUID" => Some(self.uuid.clone()),
                        _ => None,
                    };
                    if let Some(v) = v.filter(|v| !v.is_empty()) {
                        let bytes = v.as_bytes();
                        let bytes = &bytes[..bytes.len().min(255)];
                        out.put_slice(tag);
                        out.put_u8(bytes.len() as u8);
                        out.put_slice(bytes);
                    }
                }
                Some(out.to_vec())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ServerInfo {
        ServerInfo {
            name: "Rusty Music Server".into(),
            http_addr: None,
            http_port: 9000,
            version: "9.2.0".into(),
            uuid: "abc".into(),
        }
    }

    #[test]
    fn tlv_roundtrip() {
        let mut req = b"e".to_vec();
        for tag in [b"NAME", b"IPAD", b"JSON", b"VERS", b"UUID"] {
            req.extend_from_slice(tag);
            req.push(0);
        }
        req.extend_from_slice(b"JVID\x06\x00\x04\x20\x01\x02\x03");
        let parsed = DiscoveryRequest::parse(&req);
        let DiscoveryRequest::Tlv(items) = &parsed else {
            panic!()
        };
        assert_eq!(items.len(), 6);
        assert_eq!(items[5].1, vec![0, 4, 0x20, 1, 2, 3]);
        let resp = info().respond(&parsed).unwrap();
        assert_eq!(resp[0], b'E');
        let DiscoveryRequest::ServerResponse(r) = DiscoveryRequest::parse(&resp) else {
            panic!()
        };
        let tags: Vec<_> = r
            .iter()
            .map(|(t, _)| std::str::from_utf8(t).unwrap())
            .collect();
        assert_eq!(tags, vec!["NAME", "JSON", "VERS", "UUID"]); // IPAD omitted when unset
        assert_eq!(r[1].1, b"9000");
    }

    #[test]
    fn legacy() {
        let mut req = vec![b'd', 0, 4, 130];
        req.extend_from_slice(&[0u8; 8]);
        req.extend_from_slice(&[0, 4, 0x20, 1, 2, 3]);
        let parsed = DiscoveryRequest::parse(&req);
        assert_eq!(
            parsed,
            DiscoveryRequest::Legacy {
                device_id: 4,
                revision: 130,
                mac: [0, 4, 0x20, 1, 2, 3]
            }
        );
        let resp = info().respond(&parsed).unwrap();
        assert_eq!(resp.len(), 18);
        assert_eq!(&resp[..17], b"DRusty Music Serv");
        assert_eq!(resp[17], 0);
    }
}
