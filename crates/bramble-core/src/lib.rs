//! Pure domain logic for Bramble: pet state, lessons, the lesson session state
//! machine, and wire-protocol types. No IO lives here so everything stays
//! deterministic and unit-testable.

pub mod curriculum;
pub mod lesson;
pub mod mastery;
pub mod money;
pub mod net;
pub mod pet;
pub mod protocol;
pub mod session;
pub mod teacher;
pub mod text;

pub const PROTOCOL_VERSION: u16 = 1;
