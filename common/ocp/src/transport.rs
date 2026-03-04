// Licensed under the Apache-2.0 license

//! This module defines physical transport requirements for the OCP Recovery protocol.  The recovery
//! protocol currently defines 3 transport mediums:
//! 1. SMBus
//! 2. I3C
//! 3. USB
//!
//! While the packaging of the bytes differs per protocol, the state machine each interacts with is
//! the same.  As such define a common interface which the OCP State machine can use to receive
//! arbitrary messages, to ensure a uniform implementation.

use crate::error::OcpError;

/// Specify the timeout for a transport operation.
pub enum Timeout {
    /// Wait infinitely for a message to arrive.
    Never,

    /// If a message is not received within the given timeframe, return an error.  If the time is 0
    /// the transport run its message processing functionality at least once.
    Milliseconds(usize),
}

/// A common interface for receiving OCP Recovery Messages over arbitrary transports.
pub trait Transport {
    /// A blocking call to receive a message from the underlying transport.  This should be a
    /// complete OCP Recovery Command.  The underlying implementation is responsible for stitching
    /// any partial transportion packets into a unified message.
    ///
    /// This could return an error if the underlying transport media encounters an error, or a
    /// message is not received within the specified timeout timeframe.
    fn recv_msg(&mut self, timeout: Timeout) -> Result<&[u8], OcpError>;

    /// A blocking call to send a message from the underlying transport.  The underlying
    /// implementation is responsible for any chunking into transport packets to meet the
    /// constraints of the underlying medium.
    ///
    /// This could return an error if the underlying transport media encounters an error.
    fn send_msg(&mut self, msg: &[u8]) -> Result<(), OcpError>;
}
