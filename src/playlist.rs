//! Saved playlist prefixes and pages use Spotify's raw item offsets. Hidden
//! items still advance the cursor; repeated tracks remain separate rows.

use crate::model::ListedTrack;

pub const PAGE_SIZE: u32 = 50;

#[derive(Clone, Debug, Eq, PartialEq)]
/// One coherent prefix of a playlist version, in default order.
pub struct PlaylistSnapshot {
    pub revision: String,
    pub tracks: Vec<ListedTrack>,
    /// The next raw item offset, including entries hidden from the track list.
    pub next_offset: u32,
    /// Spotify's raw item count, rather than the displayed track count.
    pub total: u32,
}

impl PlaylistSnapshot {
    pub fn complete(&self) -> bool {
        self.next_offset >= self.total
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One page to append only to its revision and expected raw offset.
pub struct PlaylistPage {
    pub revision: String,
    pub offset: u32,
    pub next_offset: u32,
    pub total: u32,
    pub tracks: Vec<ListedTrack>,
}
