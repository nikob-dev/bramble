//! Pure domain logic for Bramble: pet state, lessons, the lesson session state
//! machine, and wire-protocol types. No IO lives here so everything stays
//! deterministic and unit-testable.

pub mod curriculum;
pub mod lesson;
pub mod mastery;
pub mod pet;
pub mod protocol;
pub mod session;
pub mod teacher;

pub const PROTOCOL_VERSION: u16 = 1;
