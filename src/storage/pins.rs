//! Where the account's pins are kept between launches.
//!
//! The set is small, read whole and written whole, so it lives in
//! `preferences` as one JSON list rather than in a table of its own — the
//! same shape the open folders use. Cadence keeps no pins of its own: this
//! is a copy of what Spotify holds, so that the section is drawn before the
//! network answers and still drawn when there is no session at all.

use anyhow::Result;

use crate::pins::Pins;

use super::Store;

/// The pin list as last read, and the token the next increment carries.
const PINS_KEY: &str = "pins";
const SYNC_TOKEN_KEY: &str = "pin_sync_token";

/// Both belong to the account, so both go when the account does.
pub(super) const ACCOUNT_KEYS: [&str; 2] = [PINS_KEY, SYNC_TOKEN_KEY];

impl Store {
    pub fn pins(&self) -> Result<Pins> {
        let stored: Vec<String> = self
            .preference(PINS_KEY)?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        Ok(Pins::from_uris(stored))
    }

    /// Replaces the pin list, and stores the sync token that goes with it.
    /// Both are written together: a token without the list it belongs to
    /// would make the next increment apply to the wrong set.
    pub fn set_pins(&mut self, pins: &Pins, sync_token: Option<&str>) -> Result<()> {
        let written = [
            Some((PINS_KEY, serde_json::to_string(pins.uris())?)),
            sync_token.map(|token| (SYNC_TOKEN_KEY, token.to_owned())),
        ];
        let transaction = self.connection.transaction()?;
        for (key, value) in written.into_iter().flatten() {
            transaction.execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// The token the last read handed back. Without one there is nothing to
    /// ask for an increment against, so the set is read in full.
    pub fn pin_sync_token(&self) -> Result<Option<String>> {
        Ok(self
            .preference(SYNC_TOKEN_KEY)?
            .filter(|token| !token.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_and_their_sync_token_survive_a_round_trip() {
        let mut store = Store::in_memory().unwrap();
        assert!(store.pins().unwrap().is_empty());
        assert_eq!(store.pin_sync_token().unwrap(), None);

        let pins = Pins::from_uris([
            "spotify:playlist:aaa".to_owned(),
            "spotify:folder:f1".to_owned(),
        ]);
        store.set_pins(&pins, Some("sync-1")).unwrap();

        assert_eq!(store.pins().unwrap(), pins);
        assert_eq!(store.pin_sync_token().unwrap().as_deref(), Some("sync-1"));

        // A write with no token keeps the one already stored.
        store.set_pins(&Pins::default(), None).unwrap();
        assert!(store.pins().unwrap().is_empty());
        assert_eq!(store.pin_sync_token().unwrap().as_deref(), Some("sync-1"));
    }

    #[test]
    fn signing_out_forgets_the_pins_with_the_rest_of_the_library() {
        let mut store = Store::in_memory().unwrap();
        store
            .set_pins(
                &Pins::from_uris(["spotify:playlist:aaa".to_owned()]),
                Some("sync-1"),
            )
            .unwrap();

        store.clear_library_cache().unwrap();

        assert!(store.pins().unwrap().is_empty());
        assert_eq!(store.pin_sync_token().unwrap(), None);
    }
}
