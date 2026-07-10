//! Identifier newtypes. These are the KEYS — the strings whose accidental swap
//! should be a compile error, not a zero-row query three layers down. Free-text
//! fields (title, locations, department) stay bare `String`; a newtype there is
//! noise, since nothing swaps into them by mistake.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A string identifier that serializes as its bare value (JSON + TOML) and, via
/// `#[sqlx(transparent)]`, binds and decodes as TEXT. One definition site for the
/// boundary-crossing boilerplate, so the whole point of a newtype — that it's free —
/// actually holds.
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_id! {
    /// Our name for a configured board, e.g. `"stripe"`. Distinct from [`AtsToken`]:
    /// this is the id in the user's config, that is the tenant slug in the ATS URL.
    BoardId
}

string_id! {
    /// The requisition id exactly as the ATS reports it. Half of a posting's
    /// identity — the pair `(BoardId, ReqId)` — so swapping it with a `BoardId` is
    /// precisely the bug this newtype makes uncompilable.
    ReqId
}

string_id! {
    /// The ATS tenant slug that appears in the board's API URL, e.g. `"stripe"` in
    /// `boards-api.greenhouse.io/v1/boards/stripe/jobs`.
    AtsToken
}

/// A digest over a posting's material fields — the change-detection key. Stored and
/// serialized as lowercase hex TEXT (a `sqlite3` CLI can read it), NOT as raw bytes.
/// The hashing itself lives with the `Posting` model; this type only carries the
/// result and owns its wire/DB encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash([u8; 32]);

/// Returned when hex text isn't a 32-byte content hash.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid content hash: expected 64 hex chars (32 bytes), got {0:?}")]
pub struct ParseContentHashError(String);

impl ContentHash {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self, ParseContentHashError> {
        let bytes = hex::decode(s).map_err(|_| ParseContentHashError(s.to_owned()))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| ParseContentHashError(s.to_owned()))?;
        Ok(Self(arr))
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

// Stored as hex TEXT, delegating to `String`'s codec. Not `#[sqlx(transparent)]`,
// because the wire/DB form is the hex string, not the raw `[u8; 32]`.
impl sqlx::Type<sqlx::Sqlite> for ContentHash {
    fn type_info() -> <sqlx::Sqlite as sqlx::Database>::TypeInfo {
        <String as sqlx::Type<sqlx::Sqlite>>::type_info()
    }

    fn compatible(ty: &<sqlx::Sqlite as sqlx::Database>::TypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for ContentHash {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<sqlx::Sqlite>>::encode(self.to_hex(), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for ContentHash {
    fn decode(
        value: <sqlx::Sqlite as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        Ok(Self::from_hex(&s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Row, SqlitePool};

    #[test]
    fn string_id_serializes_transparently() {
        let id = BoardId::new("stripe");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"stripe\"");
        let back: BoardId = serde_json::from_str("\"stripe\"").unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn content_hash_round_trips_through_hex() {
        let h = ContentHash::from_bytes([0xab; 32]);
        assert_eq!(h.to_hex(), "ab".repeat(32));
        assert_eq!(ContentHash::from_hex(&h.to_hex()).unwrap(), h);
        assert!(serde_json::from_str::<ContentHash>("\"aabb\"").is_err());
        assert!(ContentHash::from_hex("zz").is_err());
    }

    // Proves the sqlx boundary for each id now, against an in-memory DB, rather than
    // discovering an Encode/Decode gap when D.1 first writes a real query.
    #[tokio::test]
    async fn ids_round_trip_through_sqlite() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE t (board TEXT, req TEXT, token TEXT, hash TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        let board = BoardId::new("stripe");
        let req = ReqId::new("4152884006");
        let token = AtsToken::new("stripe");
        let hash = ContentHash::from_bytes([7; 32]);
        sqlx::query("INSERT INTO t VALUES (?, ?, ?, ?)")
            .bind(&board)
            .bind(&req)
            .bind(&token)
            .bind(hash)
            .execute(&pool)
            .await
            .unwrap();

        let row = sqlx::query("SELECT board, req, token, hash FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.get::<BoardId, _>("board"), board);
        assert_eq!(row.get::<ReqId, _>("req"), req);
        assert_eq!(row.get::<AtsToken, _>("token"), token);
        assert_eq!(row.get::<ContentHash, _>("hash"), hash);
    }
}
