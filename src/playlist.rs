//! Saved playlist prefixes and pages use Spotify's raw item offsets. Hidden
//! items still advance the cursor; repeated tracks remain separate rows.

use crate::model::ListedTrack;

pub const PAGE_SIZE: u32 = 50;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistSnapshot {
    pub revision: String,
    pub tracks: Vec<ListedTrack>,
    pub next_offset: u32,
    pub total: u32,
}

impl PlaylistSnapshot {
    pub fn complete(&self) -> bool {
        self.next_offset >= self.total
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistPage {
    pub revision: String,
    pub offset: u32,
    pub next_offset: u32,
    pub total: u32,
    pub tracks: Vec<ListedTrack>,
}
