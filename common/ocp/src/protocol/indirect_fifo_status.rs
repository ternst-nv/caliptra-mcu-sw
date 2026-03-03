// Licensed under the Apache-2.0 license

//! INDIRECT_FIFO_STATUS (cmd=0x2E) response structure.
//!
//! Spec reference: Section 9.2 / Section 8.2.5, "Indirect FIFO CMS".
//! A 20-byte RO command reporting status, region type, indices, FIFO size,
//! and max transfer size for the FIFO CMS selected via INDIRECT_FIFO_CTRL.
//! This command is optional (scope R -- recovery interface must be active).

use bitfield::bitfield;
use core::convert::TryFrom;

use crate::error::OcpError;

/// Wire size of an INDIRECT_FIFO_STATUS message in bytes.
pub const MESSAGE_LEN: usize = 20;

/// Byte 1, bits 0-2: FIFO CMS region type.
///
/// Unlike [`super::indirect_status::CmsRegionType`], the FIFO variant has no
/// polling bit, different access directions, and no 0b110 encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FifoCmsRegionType {
    /// Code space for recovery (Write Only).
    CodeSpace = 0b000,
    /// Log, debug format (Read Only).
    Log = 0b001,
    /// Vendor Defined Region (Write Only).
    VendorWo = 0b100,
    /// Vendor Defined Region (Read Only).
    VendorRo = 0b101,
    /// Unsupported Region (address space out of range).
    Unsupported = 0b111,
}

impl TryFrom<u8> for FifoCmsRegionType {
    type Error = OcpError;

    fn try_from(value: u8) -> Result<Self, OcpError> {
        match value {
            0b000 => Ok(Self::CodeSpace),
            0b001 => Ok(Self::Log),
            0b100 => Ok(Self::VendorWo),
            0b101 => Ok(Self::VendorRo),
            0b111 => Ok(Self::Unsupported),
            _ => Err(OcpError::IndirectFifoStatusInvalidRegionType),
        }
    }
}

bitfield! {
    /// Byte 0 of INDIRECT_FIFO_STATUS — FIFO status flags.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct FifoStatusFlags(u8);
    impl Debug;

    /// Bit 0: FIFO is empty.
    pub bool, empty, set_empty: 0;
    /// Bit 1: FIFO is full.
    pub bool, full, set_full: 1;
}

/// INDIRECT_FIFO_STATUS response (20 bytes on the wire).
///
/// | Byte  | Field             |
/// |-------|-------------------|
/// | 0     | Status flags      |
/// | 1     | Region type       |
/// | 2-3   | Reserved          |
/// | 4-7   | Write Index (LE)  |
/// | 8-11  | Read Index (LE)   |
/// | 12-15 | FIFO Size (LE)    |
/// | 16-19 | Max Xfer Size (LE)|
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndirectFifoStatus {
    /// Byte 0: FIFO status flags.
    status: FifoStatusFlags,
    /// Byte 1: region type (bits 0-2).
    region_type: FifoCmsRegionType,
    /// Bytes 4-7: Write Index in 4B units (little-endian).
    pub write_index: u32,
    /// Bytes 8-11: Read Index in 4B units (little-endian).
    pub read_index: u32,
    /// Bytes 12-15: FIFO size in 4B units (little-endian).
    pub fifo_size: u32,
    /// Bytes 16-19: Max transfer size in 4B units (little-endian).
    pub max_transfer_size: u32,
}

impl IndirectFifoStatus {
    /// Create a new INDIRECT_FIFO_STATUS response.
    pub fn new(
        status: FifoStatusFlags,
        region_type: FifoCmsRegionType,
        write_index: u32,
        read_index: u32,
        fifo_size: u32,
        max_transfer_size: u32,
    ) -> Self {
        Self {
            status,
            region_type,
            write_index,
            read_index,
            fifo_size,
            max_transfer_size,
        }
    }

    /// Bit 0: FIFO is empty.
    pub fn empty(&self) -> bool {
        self.status.empty()
    }

    /// Bit 1: FIFO is full.
    pub fn full(&self) -> bool {
        self.status.full()
    }

    /// Byte 1, bits 0-2: FIFO CMS region type.
    pub fn region_type(&self) -> FifoCmsRegionType {
        self.region_type
    }

    /// Serialize into the 20-byte wire representation.
    ///
    /// Reserved bytes 2-3 are written as zero.
    pub fn to_message(self) -> [u8; MESSAGE_LEN] {
        let wi = self.write_index.to_le_bytes();
        let ri = self.read_index.to_le_bytes();
        let fs = self.fifo_size.to_le_bytes();
        let mt = self.max_transfer_size.to_le_bytes();
        [
            self.status.0,
            self.region_type as u8,
            0x00,
            0x00,
            wi[0],
            wi[1],
            wi[2],
            wi[3],
            ri[0],
            ri[1],
            ri[2],
            ri[3],
            fs[0],
            fs[1],
            fs[2],
            fs[3],
            mt[0],
            mt[1],
            mt[2],
            mt[3],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_flags(empty: bool, full: bool) -> FifoStatusFlags {
        let mut f = FifoStatusFlags(0);
        f.set_empty(empty);
        f.set_full(full);
        f
    }

    #[test]
    fn default_all_clear() {
        let status =
            IndirectFifoStatus::new(FifoStatusFlags(0), FifoCmsRegionType::CodeSpace, 0, 0, 0, 0);
        let msg = status.to_message();
        assert_eq!(msg, [0u8; 20]);
    }

    #[test]
    fn empty_flag() {
        let status = IndirectFifoStatus::new(
            status_flags(true, false),
            FifoCmsRegionType::CodeSpace,
            0,
            0,
            0,
            0,
        );
        assert!(status.empty());
        assert!(!status.full());
        assert_eq!(status.to_message()[0], 0x01);
    }

    #[test]
    fn full_flag() {
        let status = IndirectFifoStatus::new(
            status_flags(false, true),
            FifoCmsRegionType::CodeSpace,
            0,
            0,
            0,
            0,
        );
        assert!(!status.empty());
        assert!(status.full());
        assert_eq!(status.to_message()[0], 0x02);
    }

    #[test]
    fn both_flags_set() {
        let status = IndirectFifoStatus::new(
            status_flags(true, true),
            FifoCmsRegionType::CodeSpace,
            0,
            0,
            0,
            0,
        );
        assert_eq!(status.to_message()[0], 0x03);
    }

    #[test]
    fn all_region_types_serialize() {
        let types = [
            (FifoCmsRegionType::CodeSpace, 0b000u8),
            (FifoCmsRegionType::Log, 0b001),
            (FifoCmsRegionType::VendorWo, 0b100),
            (FifoCmsRegionType::VendorRo, 0b101),
            (FifoCmsRegionType::Unsupported, 0b111),
        ];
        for (rt, expected) in types {
            let status = IndirectFifoStatus::new(FifoStatusFlags(0), rt, 0, 0, 0, 0);
            assert_eq!(status.region_type(), rt);
            assert_eq!(status.to_message()[1], expected, "mismatch for {:?}", rt);
        }
    }

    #[test]
    fn reserved_region_types_rejected() {
        for val in [0b010, 0b011, 0b110] {
            assert_eq!(
                FifoCmsRegionType::try_from(val),
                Err(OcpError::IndirectFifoStatusInvalidRegionType),
            );
        }
    }

    #[test]
    fn reserved_bytes_are_zero() {
        let status = IndirectFifoStatus::new(
            FifoStatusFlags(0),
            FifoCmsRegionType::CodeSpace,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
        );
        let msg = status.to_message();
        assert_eq!(msg[2], 0x00);
        assert_eq!(msg[3], 0x00);
    }

    #[test]
    fn write_index_little_endian() {
        let status = IndirectFifoStatus::new(
            FifoStatusFlags(0),
            FifoCmsRegionType::CodeSpace,
            0x04030201,
            0,
            0,
            0,
        );
        let msg = status.to_message();
        assert_eq!(msg[4], 0x01);
        assert_eq!(msg[5], 0x02);
        assert_eq!(msg[6], 0x03);
        assert_eq!(msg[7], 0x04);
    }

    #[test]
    fn read_index_little_endian() {
        let status = IndirectFifoStatus::new(
            FifoStatusFlags(0),
            FifoCmsRegionType::CodeSpace,
            0,
            0x04030201,
            0,
            0,
        );
        let msg = status.to_message();
        assert_eq!(msg[8], 0x01);
        assert_eq!(msg[9], 0x02);
        assert_eq!(msg[10], 0x03);
        assert_eq!(msg[11], 0x04);
    }

    #[test]
    fn fifo_size_little_endian() {
        let status = IndirectFifoStatus::new(
            FifoStatusFlags(0),
            FifoCmsRegionType::CodeSpace,
            0,
            0,
            0x04030201,
            0,
        );
        let msg = status.to_message();
        assert_eq!(msg[12], 0x01);
        assert_eq!(msg[13], 0x02);
        assert_eq!(msg[14], 0x03);
        assert_eq!(msg[15], 0x04);
    }

    #[test]
    fn max_transfer_size_little_endian() {
        let status = IndirectFifoStatus::new(
            FifoStatusFlags(0),
            FifoCmsRegionType::CodeSpace,
            0,
            0,
            0,
            0x04030201,
        );
        let msg = status.to_message();
        assert_eq!(msg[16], 0x01);
        assert_eq!(msg[17], 0x02);
        assert_eq!(msg[18], 0x03);
        assert_eq!(msg[19], 0x04);
    }

    #[test]
    fn full_message_with_all_fields() {
        let status = IndirectFifoStatus::new(
            status_flags(true, false),
            FifoCmsRegionType::VendorWo,
            0x0000_0010,
            0x0000_0004,
            0x0000_0100,
            0x0000_0040,
        );
        let msg = status.to_message();

        assert_eq!(msg[0], 0x01);
        assert_eq!(msg[1], 0b100);
        assert_eq!(msg[2], 0x00);
        assert_eq!(msg[3], 0x00);
        assert_eq!(u32::from_le_bytes([msg[4], msg[5], msg[6], msg[7]]), 0x10);
        assert_eq!(u32::from_le_bytes([msg[8], msg[9], msg[10], msg[11]]), 0x04);
        assert_eq!(
            u32::from_le_bytes([msg[12], msg[13], msg[14], msg[15]]),
            0x100
        );
        assert_eq!(
            u32::from_le_bytes([msg[16], msg[17], msg[18], msg[19]]),
            0x40
        );
    }
}
