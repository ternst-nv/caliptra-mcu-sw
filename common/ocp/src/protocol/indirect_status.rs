// Licensed under the Apache-2.0 license

//! INDIRECT_STATUS (cmd=0x2A) response structure.
//!
//! Spec reference: Section 9.2, "INDIRECT" / Indirect Memory Rules.
//! A 6-byte RO command reporting status and type of the selected CMS region.
//! This command is optional (scope R -- recovery interface must be active).

use bitfield::bitfield;
use core::convert::TryFrom;

use crate::error::OcpError;

/// Wire size of an INDIRECT_STATUS message in bytes.
pub const MESSAGE_LEN: usize = 6;

/// Byte 1, bits 0-2: CMS region type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CmsRegionType {
    /// Code space (R/W).
    CodeSpace = 0b000,
    /// Log, debug format (RO).
    Log = 0b001,
    /// Vendor Defined (WO).
    VendorWo = 0b100,
    /// Vendor Defined (R/W).
    VendorRw = 0b101,
    /// Vendor Defined (RO).
    VendorRo = 0b110,
    /// Unsupported Region.
    Unsupported = 0b111,
}

impl TryFrom<u8> for CmsRegionType {
    type Error = OcpError;

    fn try_from(value: u8) -> Result<Self, OcpError> {
        match value {
            0b000 => Ok(Self::CodeSpace),
            0b001 => Ok(Self::Log),
            0b100 => Ok(Self::VendorWo),
            0b101 => Ok(Self::VendorRw),
            0b110 => Ok(Self::VendorRo),
            0b111 => Ok(Self::Unsupported),
            _ => Err(OcpError::IndirectStatusInvalidCmsRegionType),
        }
    }
}

bitfield! {
    /// Byte 0 of INDIRECT_STATUS — status flags.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct StatusFlags(u8);
    impl Debug;

    /// Bit 0: CMS address overflow (address wrapped to beginning).
    pub bool, overflow_cms, set_overflow_cms: 0;
    /// Bit 1: Read-only error (write to RO region).
    pub bool, read_only_error, set_read_only_error: 1;
    /// Bit 2: ACK.
    pub bool, ack, set_ack: 2;
    /// Bit 3: CMS polling error (polling region not ready).
    pub bool, cms_polling_error, set_cms_polling_error: 3;
    /// Bit 4: Write-only error.
    pub bool, write_only_error, set_write_only_error: 4;
}

bitfield! {
    /// Byte 1 of INDIRECT_STATUS — CMS region type and polling bit.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct RegionType(u8);
    impl Debug;

    /// Bits 0-2: Region type encoding.
    pub u8, region_type, set_region_type: 2, 0;
    /// Bit 3: Polling required (P bit).
    pub bool, polling, set_polling: 3;
}

/// INDIRECT_STATUS response (6 bytes on the wire).
///
/// | Byte | Field       |
/// |------|-------------|
/// | 0    | Status flags|
/// | 1    | CMS type    |
/// | 2-5  | Region size |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndirectStatus {
    /// Byte 0: status flags.
    status: StatusFlags,
    /// Byte 1: CMS type (region type + polling bit).
    region_type: RegionType,
    /// Bytes 2-5: Region size in bytes (little-endian).
    pub region_size: u32,
}

impl IndirectStatus {
    /// Create a new INDIRECT_STATUS response.
    pub fn new(
        status: StatusFlags,
        cms_region_type: CmsRegionType,
        polling: bool,
        region_size: u32,
    ) -> Self {
        let mut region_type = RegionType(0);
        region_type.set_region_type(cms_region_type as u8);
        region_type.set_polling(polling);

        Self {
            status,
            region_type,
            region_size,
        }
    }

    /// Bit 0: CMS address overflow.
    pub fn overflow_cms(&self) -> bool {
        self.status.overflow_cms()
    }

    /// Bit 1: Read-only error.
    pub fn read_only_error(&self) -> bool {
        self.status.read_only_error()
    }

    /// Bit 2: ACK.
    pub fn ack(&self) -> bool {
        self.status.ack()
    }

    /// Bit 3: CMS polling error.
    pub fn cms_polling_error(&self) -> bool {
        self.status.cms_polling_error()
    }

    /// Bit 4: Write-only error.
    pub fn write_only_error(&self) -> bool {
        self.status.write_only_error()
    }

    /// Byte 1, bits 0-2: CMS region type.
    pub fn cms_region_type(&self) -> CmsRegionType {
        CmsRegionType::try_from(self.region_type.region_type())
            .expect("constructed with a valid region type")
    }

    /// Byte 1, bit 3: Polling required.
    pub fn polling(&self) -> bool {
        self.region_type.polling()
    }

    /// Serialize into the 6-byte wire representation.
    pub fn to_message(self) -> [u8; MESSAGE_LEN] {
        let size = self.region_size.to_le_bytes();
        [
            self.status.0,
            self.region_type.0,
            size[0],
            size[1],
            size[2],
            size[3],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(
        overflow_cms: bool,
        read_only_error: bool,
        ack: bool,
        cms_polling_error: bool,
        write_only_error: bool,
    ) -> StatusFlags {
        let mut f = StatusFlags(0);
        f.set_overflow_cms(overflow_cms);
        f.set_read_only_error(read_only_error);
        f.set_ack(ack);
        f.set_cms_polling_error(cms_polling_error);
        f.set_write_only_error(write_only_error);
        f
    }

    #[test]
    fn default_all_clear() {
        let status = IndirectStatus::new(StatusFlags(0), CmsRegionType::CodeSpace, false, 0);
        let msg = status.to_message();
        assert_eq!(msg, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn overflow_cms_flag() {
        let status = IndirectStatus::new(
            flags(true, false, false, false, false),
            CmsRegionType::CodeSpace,
            false,
            0,
        );
        assert!(status.overflow_cms());
        assert_eq!(status.to_message()[0], 0x01);
    }

    #[test]
    fn read_only_error_flag() {
        let status = IndirectStatus::new(
            flags(false, true, false, false, false),
            CmsRegionType::CodeSpace,
            false,
            0,
        );
        assert!(status.read_only_error());
        assert_eq!(status.to_message()[0], 0x02);
    }

    #[test]
    fn ack_flag() {
        let status = IndirectStatus::new(
            flags(false, false, true, false, false),
            CmsRegionType::CodeSpace,
            false,
            0,
        );
        assert!(status.ack());
        assert_eq!(status.to_message()[0], 0x04);
    }

    #[test]
    fn cms_polling_error_flag() {
        let status = IndirectStatus::new(
            flags(false, false, false, true, false),
            CmsRegionType::CodeSpace,
            false,
            0,
        );
        assert!(status.cms_polling_error());
        assert_eq!(status.to_message()[0], 0x08);
    }

    #[test]
    fn write_only_error_flag() {
        let status = IndirectStatus::new(
            flags(false, false, false, false, true),
            CmsRegionType::CodeSpace,
            false,
            0,
        );
        assert!(status.write_only_error());
        assert_eq!(status.to_message()[0], 0x10);
    }

    #[test]
    fn all_flags_set() {
        let status = IndirectStatus::new(
            flags(true, true, true, true, true),
            CmsRegionType::CodeSpace,
            false,
            0,
        );
        assert_eq!(status.to_message()[0], 0x1F);
    }

    #[test]
    fn all_region_types_serialize() {
        let types = [
            (CmsRegionType::CodeSpace, 0b000u8),
            (CmsRegionType::Log, 0b001),
            (CmsRegionType::VendorWo, 0b100),
            (CmsRegionType::VendorRw, 0b101),
            (CmsRegionType::VendorRo, 0b110),
            (CmsRegionType::Unsupported, 0b111),
        ];
        for (rt, expected) in types {
            let status = IndirectStatus::new(StatusFlags(0), rt, false, 0);
            assert_eq!(status.cms_region_type(), rt);
            assert_eq!(status.to_message()[1], expected, "mismatch for {:?}", rt);
        }
    }

    #[test]
    fn polling_bit_set() {
        let status = IndirectStatus::new(StatusFlags(0), CmsRegionType::CodeSpace, true, 0);
        assert!(status.polling());
        assert_eq!(status.to_message()[1], 0x08);
    }

    #[test]
    fn polling_with_region_type() {
        let status = IndirectStatus::new(StatusFlags(0), CmsRegionType::Log, true, 0);
        assert_eq!(status.to_message()[1], 0b1001);
    }

    #[test]
    fn region_size_little_endian() {
        let status =
            IndirectStatus::new(StatusFlags(0), CmsRegionType::CodeSpace, false, 0x04030201);
        let msg = status.to_message();
        assert_eq!(msg[2], 0x01);
        assert_eq!(msg[3], 0x02);
        assert_eq!(msg[4], 0x03);
        assert_eq!(msg[5], 0x04);
    }

    #[test]
    fn reserved_region_types_rejected() {
        assert_eq!(
            CmsRegionType::try_from(0b010),
            Err(OcpError::IndirectStatusInvalidCmsRegionType),
        );
        assert_eq!(
            CmsRegionType::try_from(0b011),
            Err(OcpError::IndirectStatusInvalidCmsRegionType),
        );
    }

    #[test]
    fn full_message_with_all_fields() {
        let status = IndirectStatus::new(
            flags(true, false, true, false, false),
            CmsRegionType::VendorRw,
            true,
            0x0000_1000,
        );
        let msg = status.to_message();

        assert_eq!(msg[0], 0x05);
        assert_eq!(msg[1], 0b00001101);
        assert_eq!(
            u32::from_le_bytes([msg[2], msg[3], msg[4], msg[5]]),
            0x0000_1000,
        );
    }
}
