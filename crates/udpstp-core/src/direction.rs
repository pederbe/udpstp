//! The direction of a Test.

use udpstp_wire::activation::cmd_request;

/// Which endpoint sends the load. A Test has exactly one direction, shared
/// by all its Test Connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// The Client sends Load PDUs; the Server sends Status PDUs.
    Upstream,
    /// The Server sends Load PDUs; the Client sends Status PDUs.
    Downstream,
}

impl Direction {
    /// The Activation Request cmdRequest value for this direction.
    pub(crate) fn activation_cmd_request(self) -> u8 {
        match self {
            Direction::Upstream => cmd_request::UPSTREAM,
            Direction::Downstream => cmd_request::DOWNSTREAM,
        }
    }

    /// The direction an Activation Request's cmdRequest asks for, if valid.
    pub(crate) fn from_activation_cmd_request(value: u8) -> Option<Self> {
        match value {
            cmd_request::UPSTREAM => Some(Direction::Upstream),
            cmd_request::DOWNSTREAM => Some(Direction::Downstream),
            _ => None,
        }
    }
}
