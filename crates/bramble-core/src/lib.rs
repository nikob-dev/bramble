//! Pure domain logic for Bramble: pet state, curriculum graph, mastery, protocol types.
//! No IO lives here so everything stays deterministic and unit-testable.

pub const PROTOCOL_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_starts_at_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
