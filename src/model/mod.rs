//! The type-system spine everything else hangs on: identifier newtypes whose swap
//! is a compile error, closed enums over what used to be status strings, and
//! compensation as integer minor units. The `Posting` model (B.1) is assembled from
//! these; adapters (Phase C) produce them; the store (Phase D) round-trips them.

pub mod comp;
pub mod enums;
pub mod ids;

pub use comp::{Comp, CompError, CompInterval, CompSource, Currency};
pub use enums::{Ats, ObitKind, WorkplaceType};
pub use ids::{AtsToken, BoardId, ContentHash, ReqId};
