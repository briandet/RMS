//! A player's playlist, mirroring `Slim::Player::Playlist`.
//!
//! The list keeps every entry in insertion order (`entries`) plus an `order` vector that maps
//! playlist positions to entries.  When shuffle is off `order` is the identity; when shuffle is
//! on it is a permutation.  All "index" values exposed to clients (`playlist index`,
//! `playlist_cur_index`, `status` loops) refer to positions in `order`, exactly like the
//! reference server's shuffle list.

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

pub const SHUFFLE_OFF: u8 = 0;
pub const SHUFFLE_SONGS: u8 = 1;
pub const SHUFFLE_ALBUMS: u8 = 2;

#[cfg(test)]
const REPEAT_ONE: u8 = 1;
#[cfg(test)]
const REPEAT_ALL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub url: String,
    /// Library track id when known (local tracks); `None` for remote URLs.
    #[serde(default)]
    pub track_id: Option<i64>,
    /// Album grouping key used for shuffle-by-album (album id or title); set when added.
    #[serde(default)]
    pub album_key: Option<String>,
}

impl Entry {
    pub fn new(url: impl Into<String>) -> Entry {
        Entry {
            url: url.into(),
            track_id: None,
            album_key: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Playlist {
    pub entries: Vec<Entry>,
    pub order: Vec<usize>,
    pub shuffle: u8,
    pub repeat: u8,
    /// Position in `order` of the current song.
    pub current: usize,
    /// Name of the saved playlist this was loaded from, if any.
    pub name: Option<String>,
    /// URL of the saved playlist this was loaded from, if any.
    pub url: Option<String>,
    /// Set when the playlist differs from the saved playlist it came from.
    pub modified: bool,
    /// `playlist_timestamp` for status queries.
    pub timestamp: f64,
}

impl Playlist {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entry at playlist position `pos` (in shuffled order).
    pub fn song(&self, pos: usize) -> Option<&Entry> {
        self.order.get(pos).and_then(|&i| self.entries.get(i))
    }

    #[cfg(test)]
    pub fn current_song(&self) -> Option<&Entry> {
        self.song(self.current)
    }

    /// Iterate (position, entry) in play order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.order
            .iter()
            .enumerate()
            .filter_map(|(pos, &i)| self.entries.get(i).map(|e| (pos, e)))
    }

    fn touch(&mut self) {
        self.timestamp = crate::util::now();
        self.modified = true;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.current = 0;
        self.name = None;
        self.url = None;
        self.touch();
        self.modified = false;
    }

    /// Append entries.  With shuffle on, new tracks are shuffled in after the current position,
    /// like `Slim::Player::Playlist::reshuffle($client, 1)` keeping the current song.
    pub fn append(&mut self, new: Vec<Entry>) {
        if new.is_empty() {
            return;
        }
        let start = self.entries.len();
        let count = new.len();
        self.entries.extend(new);
        if self.shuffle == SHUFFLE_OFF {
            self.order.extend(start..start + count);
        } else {
            let mut tail: Vec<usize> = (start..start + count).collect();
            if self.shuffle == SHUFFLE_SONGS {
                tail.shuffle(&mut rand::rng());
            } else {
                tail = self.shuffle_by_album(tail);
            }
            // keep everything up to and including the current song, shuffle the rest together
            let keep = if self.order.is_empty() {
                0
            } else {
                (self.current + 1).min(self.order.len())
            };
            let mut rest: Vec<usize> = self.order.split_off(keep);
            rest.extend(tail);
            if self.shuffle == SHUFFLE_SONGS {
                rest.shuffle(&mut rand::rng());
            } else {
                rest = self.shuffle_by_album(rest);
            }
            self.order.extend(rest);
        }
        self.touch();
    }

    /// Insert entries right after the current song (`playlist insert`).
    pub fn insert_after_current(&mut self, new: Vec<Entry>) {
        if new.is_empty() {
            return;
        }
        let start = self.entries.len();
        let count = new.len();
        self.entries.extend(new);
        let at = if self.order.is_empty() {
            0
        } else {
            (self.current + 1).min(self.order.len())
        };
        let ids: Vec<usize> = (start..start + count).collect();
        self.order.splice(at..at, ids);
        self.touch();
    }

    #[cfg(test)]
    /// Replace the whole list (`playlist play`/`loadtracks`).
    pub fn load(&mut self, new: Vec<Entry>, start_index: Option<usize>) {
        self.entries = new;
        self.order = (0..self.entries.len()).collect();
        self.current = 0;
        self.name = None;
        self.url = None;
        if self.shuffle != SHUFFLE_OFF {
            self.reshuffle(false);
        }
        if let Some(i) = start_index {
            self.jump_to_entry(i);
        }
        self.touch();
        self.modified = false;
    }

    /// Make `entry_index` (an index into `entries`) the current song, moving it to the front
    /// of the shuffled order when shuffling (so that a chosen track plays first).
    pub fn jump_to_entry(&mut self, entry_index: usize) {
        if let Some(pos) = self.order.iter().position(|&i| i == entry_index) {
            if self.shuffle != SHUFFLE_OFF && pos != 0 {
                self.order.remove(pos);
                self.order.insert(0, entry_index);
                self.current = 0;
            } else {
                self.current = pos;
            }
        }
    }

    /// Delete the song at playlist position `pos`.  Returns true when the current song was removed.
    pub fn delete(&mut self, pos: usize) -> bool {
        if pos >= self.order.len() {
            return false;
        }
        let entry_index = self.order.remove(pos);
        self.entries.remove(entry_index);
        for i in self.order.iter_mut() {
            if *i > entry_index {
                *i -= 1;
            }
        }
        let was_current = pos == self.current;
        if pos < self.current {
            self.current -= 1;
        } else if self.current >= self.order.len() {
            self.current = 0;
        }
        self.touch();
        was_current
    }

    /// Move the song at `from` to position `to` (`playlist move`).
    pub fn move_song(&mut self, from: usize, to: usize) {
        if from >= self.order.len() || to >= self.order.len() || from == to {
            return;
        }
        let e = self.order.remove(from);
        self.order.insert(to, e);
        if self.current == from {
            self.current = to;
        } else if from < self.current && to >= self.current {
            self.current -= 1;
        } else if from > self.current && to <= self.current {
            self.current += 1;
        }
        self.touch();
    }

    /// Set the shuffle mode and reshuffle (keeping the current song first when a song is playing).
    pub fn set_shuffle(&mut self, mode: u8, keep_current: bool) {
        self.shuffle = mode.min(2);
        self.reshuffle(keep_current);
        self.timestamp = crate::util::now();
    }

    /// Rebuild `order`.  When shuffle is off this restores insertion order and keeps the same
    /// current song.
    pub fn reshuffle(&mut self, keep_current: bool) {
        let current_entry = self.order.get(self.current).copied();
        let mut ids: Vec<usize> = (0..self.entries.len()).collect();
        match self.shuffle {
            SHUFFLE_SONGS => {
                ids.shuffle(&mut rand::rng());
                if let (true, Some(cur)) = (keep_current, current_entry) {
                    if let Some(p) = ids.iter().position(|&i| i == cur) {
                        ids.remove(p);
                        ids.insert(0, cur);
                    }
                }
                self.order = ids;
                self.current = 0;
            }
            SHUFFLE_ALBUMS => {
                let mut shuffled = self.shuffle_by_album(ids);
                if let (true, Some(cur)) = (keep_current, current_entry) {
                    // rotate so the album containing the current song comes first, current first in it
                    if let Some(p) = shuffled.iter().position(|&i| i == cur) {
                        let key = self.entries[cur].album_key.clone();
                        let mut album_start = p;
                        while album_start > 0
                            && self.entries[shuffled[album_start - 1]].album_key == key
                        {
                            album_start -= 1;
                        }
                        shuffled.rotate_left(album_start);
                        let p2 = shuffled.iter().position(|&i| i == cur).unwrap_or(0);
                        let cur_id = shuffled.remove(p2);
                        shuffled.insert(0, cur_id);
                    }
                }
                self.order = shuffled;
                self.current = 0;
            }
            _ => {
                self.order = ids;
                self.current = current_entry
                    .unwrap_or(0)
                    .min(self.order.len().saturating_sub(1));
            }
        }
    }

    fn shuffle_by_album(&self, ids: Vec<usize>) -> Vec<usize> {
        // group consecutive runs with the same album key, shuffle the groups, keep track order inside
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for id in ids {
            let key = self.entries[id].album_key.as_deref();
            match groups.last_mut() {
                Some(g)
                    if key.is_some()
                        && self.entries[*g.last().unwrap()].album_key.as_deref() == key =>
                {
                    g.push(id)
                }
                _ => groups.push(vec![id]),
            }
        }
        groups.shuffle(&mut rand::rng());
        groups.into_iter().flatten().collect()
    }

    #[cfg(test)]
    /// Index of the track that follows `pos` according to repeat mode (`nextsong`).
    /// Returns `None` at the end of the playlist when repeat is off.
    pub fn next_index(&self, pos: usize) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        if self.repeat == REPEAT_ONE {
            return Some(pos);
        }
        let next = pos + 1;
        if next >= self.order.len() {
            if self.repeat == REPEAT_ALL {
                Some(0)
            } else {
                None
            }
        } else {
            Some(next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(n: usize) -> Playlist {
        let mut p = Playlist::default();
        p.load(
            (0..n)
                .map(|i| Entry::new(format!("file:///{}.flac", i)))
                .collect(),
            None,
        );
        p
    }

    #[test]
    fn basic_ops() {
        let mut p = pl(5);
        assert_eq!(p.len(), 5);
        assert_eq!(p.next_index(4), None);
        p.repeat = REPEAT_ALL;
        assert_eq!(p.next_index(4), Some(0));
        p.repeat = REPEAT_ONE;
        assert_eq!(p.next_index(2), Some(2));
        p.current = 2;
        p.delete(1);
        assert_eq!(p.current, 1);
        assert_eq!(p.current_song().unwrap().url, "file:///2.flac");
        p.move_song(0, 3);
        assert_eq!(p.current, 0);
        assert_eq!(p.song(3).unwrap().url, "file:///0.flac");
        p.insert_after_current(vec![Entry::new("file:///x.flac")]);
        assert_eq!(p.song(1).unwrap().url, "file:///x.flac");
    }

    #[test]
    fn shuffle_keeps_current_first_and_restores_order() {
        let mut p = pl(20);
        p.current = 7;
        p.set_shuffle(SHUFFLE_SONGS, true);
        assert_eq!(p.current, 0);
        assert_eq!(p.current_song().unwrap().url, "file:///7.flac");
        let mut sorted = p.order.clone();
        sorted.sort();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        p.set_shuffle(SHUFFLE_OFF, true);
        assert_eq!(p.current, 7);
        assert_eq!(p.order, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn album_shuffle_groups() {
        let mut p = Playlist::default();
        let mut e = Vec::new();
        for a in 0..4 {
            for t in 0..3 {
                let mut en = Entry::new(format!("file:///{}-{}.flac", a, t));
                en.album_key = Some(format!("album{}", a));
                e.push(en);
            }
        }
        p.load(e, None);
        p.set_shuffle(SHUFFLE_ALBUMS, false);
        // every album stays contiguous and in track order
        for chunk in p.order.chunks(3) {
            let keys: Vec<_> = chunk
                .iter()
                .map(|&i| p.entries[i].album_key.clone())
                .collect();
            assert!(keys.iter().all(|k| *k == keys[0]));
            assert!(chunk.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
