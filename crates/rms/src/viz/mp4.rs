//! Just enough of fragmented MP4 to share one live stream between several players: split
//! what the encoder writes into the init segment (`ftyp` + `moov`) and fragments (`moof` +
//! `mdat`), learn each track's timescale, and rebase a fragment's decode times (`tfdt`) so
//! every reader's stream starts at zero wherever it joined.

use bytes::{Bytes, BytesMut};
use std::collections::HashMap;

/// A fragment: one `moof` and its `mdat`.
#[derive(Debug, Clone)]
pub struct Fragment {
    pub data: Bytes,
    /// Each track's base decode time in this fragment, and where its value sits in `data`.
    pub times: Vec<TrackTime>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackTime {
    pub track: u32,
    pub time: u64,
    /// Offset of the value in the fragment's bytes.
    pub at: usize,
    /// 8 bytes (`tfdt` version 1) or 4.
    pub wide: bool,
}

/// What came out of the encoder, in order.
#[derive(Debug)]
pub enum Piece {
    /// `ftyp` + `moov`, and each track's timescale.
    Init(Bytes, HashMap<u32, u32>),
    Fragment(Fragment),
}

/// Splits a byte stream into [`Piece`]s as it arrives.
#[derive(Default)]
pub struct Splitter {
    buf: BytesMut,
    init: BytesMut,
    moof: Option<Bytes>,
    done_init: bool,
}

/// A box's type and total size at the start of `b`, when its header is complete.
fn header(b: &[u8]) -> Option<([u8; 4], usize, usize)> {
    if b.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes(b[0..4].try_into().ok()?) as u64;
    let kind: [u8; 4] = b[4..8].try_into().ok()?;
    let (size, head) = match size {
        1 => {
            if b.len() < 16 {
                return None;
            }
            (u64::from_be_bytes(b[8..16].try_into().ok()?), 16)
        }
        // to the end of the file: only `mdat` at the very end, never in a live stream
        0 => return None,
        s => (s, 8),
    };
    Some((kind, size as usize, head))
}

impl Splitter {
    /// Add bytes; returns the pieces they complete.
    pub fn push(&mut self, data: &[u8]) -> Vec<Piece> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some((kind, size, _)) = header(&self.buf) {
            if size < 8 || self.buf.len() < size {
                break;
            }
            let b = self.buf.split_to(size).freeze();
            match &kind {
                b"ftyp" | b"moov" if !self.done_init => {
                    self.init.extend_from_slice(&b);
                    if &kind == b"moov" {
                        self.done_init = true;
                        let init = std::mem::take(&mut self.init).freeze();
                        let scales = timescales(&init);
                        out.push(Piece::Init(init, scales));
                    }
                }
                b"moof" => self.moof = Some(b),
                b"mdat" => {
                    if let Some(moof) = self.moof.take() {
                        let times = track_times(&moof);
                        let mut data = BytesMut::with_capacity(moof.len() + b.len());
                        data.extend_from_slice(&moof);
                        data.extend_from_slice(&b);
                        out.push(Piece::Fragment(Fragment {
                            data: data.freeze(),
                            times,
                        }));
                    }
                }
                // `mfra` at the end, `styp`, `sidx`, `free`: not needed by a live reader
                _ => {}
            }
        }
        out
    }
}

/// The boxes directly inside `b` (a box's payload): (type, payload offset, payload length).
fn children(b: &[u8], from: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut i = from;
    while let Some((kind, size, head)) = header(&b[i..]) {
        if size < head || i + size > b.len() {
            break;
        }
        out.push((kind, i + head, size - head));
        i += size;
    }
    out
}

/// Each track's timescale (`moov/trak/tkhd` track id, `moov/trak/mdia/mdhd` timescale).
pub fn timescales(init: &[u8]) -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    for (kind, at, len) in children(init, 0) {
        if &kind != b"moov" {
            continue;
        }
        let moov = &init[..at + len];
        for (kind, at, len) in children(moov, at) {
            if &kind != b"trak" {
                continue;
            }
            let (mut id, mut scale) = (None, None);
            let trak = &moov[..at + len];
            for (kind, at, len) in children(trak, at) {
                match &kind {
                    b"tkhd" if len >= 24 => {
                        // version 1 has 64-bit times before the id
                        let off = if trak[at] == 1 { 20 } else { 12 };
                        id = trak
                            .get(at + off..at + off + 4)
                            .map(|v| u32::from_be_bytes(v.try_into().unwrap()));
                    }
                    b"mdia" => {
                        let mdia = &trak[..at + len];
                        for (kind, at, _) in children(mdia, at) {
                            if &kind == b"mdhd" {
                                let off = if mdia[at] == 1 { 20 } else { 12 };
                                scale = mdia
                                    .get(at + off..at + off + 4)
                                    .map(|v| u32::from_be_bytes(v.try_into().unwrap()));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(id), Some(scale)) = (id, scale) {
                out.insert(id, scale);
            }
        }
    }
    out
}

/// Each track's base decode time in a `moof` (`traf/tfhd` track id, `traf/tfdt`).
fn track_times(moof: &[u8]) -> Vec<TrackTime> {
    let mut out = Vec::new();
    let Some((_, size, head)) = header(moof) else {
        return out;
    };
    let moof = &moof[..size.min(moof.len())];
    for (kind, at, len) in children(moof, head) {
        if &kind != b"traf" {
            continue;
        }
        let traf = &moof[..at + len];
        let mut track = None;
        let mut time = None;
        for (kind, at, len) in children(traf, at) {
            match &kind {
                b"tfhd" if len >= 8 => {
                    track = Some(u32::from_be_bytes(traf[at + 4..at + 8].try_into().unwrap()))
                }
                b"tfdt" if len >= 8 => {
                    let wide = traf[at] == 1;
                    let v = if wide && len >= 12 {
                        u64::from_be_bytes(traf[at + 4..at + 12].try_into().unwrap())
                    } else {
                        u32::from_be_bytes(traf[at + 4..at + 8].try_into().unwrap()) as u64
                    };
                    time = Some((v, at + 4, wide && len >= 12));
                }
                _ => {}
            }
        }
        if let (Some(track), Some((time, at, wide))) = (track, time) {
            out.push(TrackTime {
                track,
                time,
                at,
                wide,
            });
        }
    }
    out
}

impl Fragment {
    /// When the fragment starts, in seconds (the first track's time).
    pub fn start(&self, scales: &HashMap<u32, u32>) -> f64 {
        self.times
            .iter()
            .find_map(|t| {
                scales
                    .get(&t.track)
                    .map(|s| t.time as f64 / *s.max(&1) as f64)
            })
            .unwrap_or(0.0)
    }

    /// The fragment with each track's decode time less that track's time in `base`.
    pub fn rebased(&self, base: &Fragment) -> Bytes {
        if base.times.iter().all(|t| t.time == 0) {
            return self.data.clone();
        }
        let mut data = BytesMut::from(&self.data[..]);
        for t in &self.times {
            let b = base
                .times
                .iter()
                .find(|b| b.track == t.track)
                .map(|b| b.time)
                .unwrap_or(0);
            let v = t.time.saturating_sub(b);
            if t.wide {
                data[t.at..t.at + 8].copy_from_slice(&v.to_be_bytes());
            } else {
                data[t.at..t.at + 4].copy_from_slice(&(v as u32).to_be_bytes());
            }
        }
        data.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    fn full(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![version, 0, 0, 0];
        p.extend_from_slice(body);
        bx(kind, &p)
    }

    fn trak(id: u32, scale: u32) -> Vec<u8> {
        let mut tkhd = vec![0u8; 8]; // creation, modification
        tkhd.extend_from_slice(&id.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 72]);
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&scale.to_be_bytes());
        mdhd.extend_from_slice(&[0u8; 8]);
        let mdia = bx(b"mdia", &full(b"mdhd", 0, &mdhd));
        let mut t = full(b"tkhd", 0, &tkhd);
        t.extend_from_slice(&mdia);
        bx(b"trak", &t)
    }

    fn traf(id: u32, time: u64) -> Vec<u8> {
        let mut t = full(b"tfhd", 0, &id.to_be_bytes());
        t.extend_from_slice(&full(b"tfdt", 1, &time.to_be_bytes()));
        bx(b"traf", &t)
    }

    fn fragment(v: u64, a: u64) -> Vec<u8> {
        let mut m = traf(1, v);
        m.extend_from_slice(&traf(2, a));
        let mut f = bx(b"moof", &m);
        f.extend_from_slice(&bx(b"mdat", b"frames"));
        f
    }

    fn stream() -> Vec<u8> {
        let mut s = bx(b"ftyp", b"isom");
        let mut moov = trak(1, 15360);
        moov.extend_from_slice(&trak(2, 44100));
        s.extend_from_slice(&bx(b"moov", &moov));
        s.extend_from_slice(&fragment(0, 0));
        s.extend_from_slice(&fragment(15360, 44100));
        s.extend_from_slice(&fragment(30720, 88200));
        s.extend_from_slice(&bx(b"mfra", b"end"));
        s
    }

    #[test]
    fn a_stream_splits_into_init_and_fragments_whatever_the_chunks() {
        let s = stream();
        for chunk in [1, 7, 100, s.len()] {
            let mut sp = Splitter::default();
            let pieces: Vec<Piece> = s.chunks(chunk).flat_map(|c| sp.push(c)).collect();
            assert_eq!(pieces.len(), 4, "chunks of {chunk}");
            let Piece::Init(init, scales) = &pieces[0] else {
                panic!("init first")
            };
            assert!(init.starts_with(&s[..12]));
            assert_eq!(scales.get(&1), Some(&15360));
            assert_eq!(scales.get(&2), Some(&44100));
            let Piece::Fragment(f) = &pieces[2] else {
                panic!("then fragments")
            };
            assert_eq!(f.times.len(), 2);
            assert_eq!(f.start(scales), 1.0);
        }
    }

    #[test]
    fn fragments_rebase_to_where_a_reader_joined() {
        let mut sp = Splitter::default();
        let pieces = sp.push(&stream());
        let frags: Vec<&Fragment> = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Fragment(f) => Some(f),
                _ => None,
            })
            .collect();
        // a reader that joined at the second fragment
        let moved = frags[2].rebased(frags[1]);
        let again = sp_one(&moved);
        assert_eq!(
            again.times.iter().map(|t| t.time).collect::<Vec<_>>(),
            vec![15360, 44100]
        );
        // from the first fragment nothing changes
        assert_eq!(frags[2].rebased(frags[0]), frags[2].data);
    }

    fn sp_one(data: &[u8]) -> Fragment {
        let mut s = bx(b"ftyp", b"isom");
        s.extend_from_slice(&bx(b"moov", &trak(1, 1)));
        s.extend_from_slice(data);
        Splitter::default()
            .push(&s)
            .into_iter()
            .find_map(|p| match p {
                Piece::Fragment(f) => Some(f),
                _ => None,
            })
            .unwrap()
    }
}
