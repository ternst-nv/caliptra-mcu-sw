// Licensed under the Apache-2.0 license

//! OCP Recovery Interface state machine.
//!
//! This module defines the `RecoveryStateMachine` that implements the OCP
//! Secure Firmware Recovery v1.1 command processing loop, along with the
//! `RecoveryDeviceConfig` and `RecoveryAction` types used by integrators.

use crate::cms::{FifoCmsRegion, IndirectCmsRegion};
use crate::error::OcpError;
use crate::protocol::device_id;
use crate::protocol::device_id::DeviceId;
use crate::protocol::device_reset::{
    DeviceReset, ForcedRecoveryMode, InterfaceControl, ResetControl,
};
use crate::protocol::device_status;
use crate::protocol::device_status::{
    DeviceStatus, DeviceStatusValue, ProtocolError, RecoveryReasonCode,
};
use crate::protocol::hw_status;
use crate::protocol::indirect_ctrl::IndirectCtrl;
use crate::protocol::indirect_fifo_ctrl::IndirectFifoCtrl;
use crate::protocol::indirect_status;
use crate::protocol::indirect_status::{CmsRegionType, IndirectStatus, StatusFlags};
use crate::protocol::prot_cap::{ProtCap, RecoveryProtocolCapabilities, RESPONSE_LEN};
use crate::protocol::recovery_ctrl::{ActivateRecoveryImage, ImageSelection, RecoveryCtrl};
use crate::protocol::recovery_status;
use crate::protocol::recovery_status::{DeviceRecoveryStatus, RecoveryStatus};
use crate::transport::{Timeout, Transport};
use crate::vendor::VendorHandler;

/// Static device configuration provided at state machine construction time.
///
/// These fields are immutable for the lifetime of the state machine and are
/// used to populate PROT_CAP and DEVICE_ID responses.
pub struct RecoveryDeviceConfig<'a> {
    /// DEVICE_ID response payload.
    pub device_id: DeviceId<'a>,

    /// PROT_CAP major version (byte 8).
    pub major_version: u8,

    /// PROT_CAP minor version (byte 9).
    pub minor_version: u8,

    /// PROT_CAP max response time exponent (byte 13).
    /// Actual time = 2^max_response_time microseconds.
    pub max_response_time: u8,

    /// PROT_CAP heartbeat period exponent (byte 14).
    /// 0 means heartbeat is not supported.
    pub heartbeat_period: u8,

    /// Whether the device supports local c-image recovery (PROT_CAP bit 6).
    /// Set to `true` if the device can recover from a locally stored image.
    pub local_c_image_support: bool,
}

/// Actions the integrator must handle after `process_command` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// No integrator action required. The command was fully handled.
    None,

    /// The integrator should activate the recovery image.
    /// After performing activation, the integrator calls
    /// `complete_activation()` to report the result.
    ActivateRecoveryImage,

    /// The integrator should perform a device reset.
    DeviceReset,

    /// The integrator should perform a management-only reset.
    ManagementReset,
}

/// OCP Recovery Interface state machine.
///
/// Implements the recovery lifecycle defined in the OCP Secure Firmware
/// Recovery spec v1.1. The state machine owns the transport and handles
/// receive/send internally, exposing `process_command()` as the single
/// entry point for the integrator's main loop.
// TODO: Remove when process_command dispatches to handlers (Phase 5+),
// which will read all fields from non-test code.
#[allow(dead_code)]
pub struct RecoveryStateMachine<'a, T: Transport, V: VendorHandler> {
    pub(crate) transport: &'a mut T,

    // Protocol structs stored directly (no borrowed data).
    pub(crate) recovery_status: RecoveryStatus,
    pub(crate) recovery_ctrl: RecoveryCtrl,
    pub(crate) device_reset: DeviceReset,
    pub(crate) indirect_ctrl: IndirectCtrl,
    pub(crate) indirect_fifo_ctrl: IndirectFifoCtrl,

    // DEVICE_STATUS fields stored individually. DeviceStatus<'a> cannot be
    // stored because its vendor_status slice comes from VendorHandler at
    // read time.
    pub(crate) device_status_value: DeviceStatusValue,
    pub(crate) protocol_error: ProtocolError,
    pub(crate) recovery_reason: RecoveryReasonCode,

    pub(crate) config: RecoveryDeviceConfig<'a>,

    pub(crate) indirect_regions: &'a mut [(u8, &'a mut dyn IndirectCmsRegion)],
    pub(crate) fifo_regions: &'a mut [(u8, &'a mut dyn FifoCmsRegion)],
    pub(crate) cms_count: u8,

    pub(crate) vendor: V,
}

// TODO: Remove when process_command dispatches to handlers (Phase 5+),
// which will call all private methods from non-test code.
#[allow(dead_code)]
impl<'a, T: Transport, V: VendorHandler> RecoveryStateMachine<'a, T, V> {
    /// Construct a new state machine with spec-defined defaults.
    ///
    /// All protocol state is initialized to power-on defaults:
    /// device status is `StatusPending`, no protocol errors, no boot failure,
    /// recovery status is `NotInRecovery`, and all control registers are zeroed.
    pub fn new(
        config: RecoveryDeviceConfig<'a>,
        transport: &'a mut T,
        indirect_regions: &'a mut [(u8, &'a mut dyn IndirectCmsRegion)],
        fifo_regions: &'a mut [(u8, &'a mut dyn FifoCmsRegion)],
        vendor: V,
    ) -> Result<Self, OcpError> {
        let recovery_status = RecoveryStatus::new(DeviceRecoveryStatus::NotInRecovery, 0, 0)?;

        if !indirect_regions.is_empty() {
            let has_cms0_code = indirect_regions.iter().any(|(idx, r)| {
                *idx == 0 && r.status().cms_region_type() == CmsRegionType::CodeSpace
            });
            if !has_cms0_code {
                return Err(OcpError::IndirectCms0NotCodeSpace);
            }
        }

        // Verify no CMS index appears more than once across both slices.
        // Note: CMS regions are likely to be few, so this operation is not prohibetively expensive.
        for (i, (idx_a, _)) in indirect_regions.iter().enumerate() {
            for (idx_b, _) in indirect_regions[i + 1..].iter() {
                if idx_a == idx_b {
                    return Err(OcpError::DuplicateCmsIndex);
                }
            }
            for (idx_b, _) in fifo_regions.iter() {
                if idx_a == idx_b {
                    return Err(OcpError::DuplicateCmsIndex);
                }
            }
        }
        for (i, (idx_a, _)) in fifo_regions.iter().enumerate() {
            for (idx_b, _) in fifo_regions[i + 1..].iter() {
                if idx_a == idx_b {
                    return Err(OcpError::DuplicateCmsIndex);
                }
            }
        }

        let indirect_count = indirect_regions.len();
        let fifo_count = fifo_regions.len();
        let cms_count: u8 = (indirect_count + fifo_count)
            .try_into()
            .map_err(|_| OcpError::InvalidCmdBufferCount)?;

        Ok(Self {
            transport,
            recovery_status,
            recovery_ctrl: RecoveryCtrl::new(
                0,
                ImageSelection::NoOperation,
                ActivateRecoveryImage::DoNotActivate,
            ),
            device_reset: DeviceReset::new(
                ResetControl::NoReset,
                ForcedRecoveryMode::None,
                InterfaceControl::DisableMastering,
            ),
            indirect_ctrl: IndirectCtrl { cms: 0, imo: 0 },
            indirect_fifo_ctrl: IndirectFifoCtrl::new(0, false, 0),
            cms_count,
            device_status_value: DeviceStatusValue::StatusPending,
            protocol_error: ProtocolError::NoError,
            recovery_reason: RecoveryReasonCode::NoBootFailure,
            config,
            indirect_regions,
            fifo_regions,
            vendor,
        })
    }

    /// Block for the next command on the transport, process it, send any
    /// response, and return an action for the integrator to handle.
    ///
    /// Returns an error only if the transport itself fails. Protocol-level
    /// errors are recorded in DEVICE_STATUS and do not cause this method
    /// to return Err.
    pub fn process_command(&mut self, timeout: Timeout) -> Result<RecoveryAction, OcpError> {
        let _msg = self.transport.recv_msg(timeout)?;
        Ok(RecoveryAction::None)
    }

    /// Called by the integrator after activation completes (successfully
    /// or not). Updates device status and recovery status accordingly.
    pub fn complete_activation(&mut self, _success: bool) {
        // Stub: will be implemented in Phase 8.
    }

    /// Look up a memory-window CMS region by index.
    fn lookup_indirect_region(&mut self, cms: u8) -> Option<&mut dyn IndirectCmsRegion> {
        for (idx, region) in self.indirect_regions.iter_mut() {
            if *idx == cms {
                return Some(*region);
            }
        }
        None
    }

    /// Look up a FIFO CMS region by index.
    fn lookup_fifo_region(&mut self, cms: u8) -> Option<&mut dyn FifoCmsRegion> {
        for (idx, region) in self.fifo_regions.iter_mut() {
            if *idx == cms {
                return Some(*region);
            }
        }
        None
    }

    /// Record a protocol error for the next DEVICE_STATUS read.
    fn set_protocol_error(&mut self, err: ProtocolError) {
        self.protocol_error = err;
    }

    /// Build the PROT_CAP capabilities bitfield from the current region
    /// configuration and vendor-reported capabilities.
    ///
    /// Identification and device_status are always set. CMS 0 is guaranteed
    /// to be CodeSpace when indirect regions are present (enforced by `new()`),
    /// so push_c_image_support and recovery_memory_access are set together
    /// whenever indirect regions exist. Bits 1-3 and 8-10 are populated from
    /// `VendorHandler::capabilities()`.
    fn build_capabilities(&self) -> RecoveryProtocolCapabilities {
        let mut caps = RecoveryProtocolCapabilities(0);
        let vendor_caps = self.vendor.capabilities();

        caps.set_identification(true);
        caps.set_device_status(true);
        caps.set_local_c_image_support(self.config.local_c_image_support);

        caps.set_forced_recovery(vendor_caps.forced_recovery());
        caps.set_mgmt_reset(vendor_caps.mgmt_reset());
        caps.set_device_reset(vendor_caps.device_reset());
        caps.set_interface_isolation(vendor_caps.interface_isolation());
        caps.set_hardware_status(vendor_caps.hardware_status());
        caps.set_vendor_command(vendor_caps.vendor_command());

        if !self.indirect_regions.is_empty() {
            caps.set_push_c_image_support(true);
            caps.set_recovery_memory_access(true);
        }

        if !self.fifo_regions.is_empty() {
            // NOTE: This is a deviation from the 1.1 SPEC.  There is currently a bug in the spec
            // where it requires indirect region if push is set, instead of being indirect region
            // and/or indirect fifo.
            caps.set_push_c_image_support(true);
            caps.set_fifo_cms_support(true);
        }

        caps
    }

    /// Handle a PROT_CAP (cmd=0x22) read: build and serialize the response.
    fn handle_prot_cap_read(&self) -> Result<[u8; RESPONSE_LEN], OcpError> {
        let caps = self.build_capabilities();
        let prot_cap = ProtCap::new(
            self.config.major_version,
            self.config.minor_version,
            caps,
            self.cms_count,
            self.config.max_response_time,
            self.config.heartbeat_period,
        );
        prot_cap.to_message()
    }

    /// Handle a PROT_CAP (cmd=0x22) write: read-only command, set error.
    fn handle_prot_cap_write(&mut self) {
        self.set_protocol_error(ProtocolError::UnsupportedCommand);
    }

    /// Handle a DEVICE_ID (cmd=0x23) read: serialize the device identity.
    fn handle_device_id_read(&self) -> ([u8; device_id::MAX_MESSAGE_LEN], usize) {
        self.config.device_id.to_message()
    }

    /// Handle a DEVICE_ID (cmd=0x23) write: read-only command, set error.
    fn handle_device_id_write(&mut self) {
        self.set_protocol_error(ProtocolError::UnsupportedCommand);
    }

    /// Handle a DEVICE_STATUS (cmd=0x24) read: assemble dynamic fields and serialize.
    ///
    /// Protocol error is clear-on-read: the current value is included in the
    /// response, then reset to `NoError`.
    fn handle_device_status_read(
        &mut self,
    ) -> Result<([u8; device_status::MAX_MESSAGE_LEN], usize), OcpError> {
        let heartbeat = self.vendor.heartbeat();
        let mut vendor_buf = [0u8; device_status::MAX_VENDOR_STATUS_LEN];
        let vendor_len = self.vendor.vendor_device_status(&mut vendor_buf);

        let status = DeviceStatus::new(
            self.device_status_value,
            self.protocol_error,
            self.recovery_reason,
            heartbeat,
            &vendor_buf[..vendor_len],
        )?;

        let result = status.to_message();
        self.protocol_error = ProtocolError::NoError;
        Ok(result)
    }

    /// Handle a DEVICE_STATUS (cmd=0x24) write: read-only command, set error.
    fn handle_device_status_write(&mut self) {
        self.set_protocol_error(ProtocolError::UnsupportedCommand);
    }

    /// Handle a RECOVERY_STATUS (cmd=0x27) read: serialize stored recovery status.
    fn handle_recovery_status_read(&self) -> [u8; recovery_status::MESSAGE_LEN] {
        self.recovery_status.to_message()
    }

    /// Handle a RECOVERY_STATUS (cmd=0x27) write: read-only command, set error.
    fn handle_recovery_status_write(&mut self) {
        self.set_protocol_error(ProtocolError::UnsupportedCommand);
    }

    /// Handle a HW_STATUS (cmd=0x28) read: delegate to vendor and serialize.
    fn handle_hw_status_read(&self) -> ([u8; hw_status::MAX_MESSAGE_LEN], usize) {
        self.vendor.hw_status().to_message()
    }

    /// Handle a HW_STATUS (cmd=0x28) write: read-only command, set error.
    fn handle_hw_status_write(&mut self) {
        self.set_protocol_error(ProtocolError::UnsupportedCommand);
    }

    /// Handle an INDIRECT_STATUS (cmd=0x2A) read.
    ///
    /// Looks up the memory-window CMS region selected by `indirect_ctrl.cms`.
    /// If found, returns its status and clears accumulated flags (clear-on-read).
    /// If the index doesn't match any indirect region (including FIFO-only
    /// indices), returns `CmsRegionType::Unsupported`.
    fn handle_indirect_status_read(&mut self) -> [u8; indirect_status::MESSAGE_LEN] {
        let cms = self.indirect_ctrl.cms;
        match self.lookup_indirect_region(cms) {
            Some(region) => {
                let status = region.status();
                region.clear_status();
                status.to_message()
            }
            None => IndirectStatus::new(StatusFlags(0), CmsRegionType::Unsupported, false, 0)
                .to_message(),
        }
    }

    /// Handle an INDIRECT_STATUS (cmd=0x2A) write: read-only command, set error.
    fn handle_indirect_status_write(&mut self) {
        self.set_protocol_error(ProtocolError::UnsupportedCommand);
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::cms::slice_fifo::SliceFifoRegion;
    use crate::cms::slice_indirect::SliceIndirectRegion;
    use crate::protocol::device_id::{DeviceDescriptor, PciVendorDescriptor};
    use crate::protocol::device_reset::DeviceReset;
    use crate::protocol::hw_status::{CompositeTemperature, HwStatus, HwStatusFlags};
    use crate::protocol::indirect_fifo_status::FifoCmsRegionType;
    use crate::protocol::indirect_status::CmsRegionType;
    use crate::protocol::prot_cap;
    use crate::vendor::VendorCapabilities;

    struct MockVendorHandler {
        caps: VendorCapabilities,
        heartbeat_val: u16,
        vendor_status_data: Vec<u8>,
        hw_flags: HwStatusFlags,
        hw_vendor_status_byte: u8,
        hw_composite_temp: CompositeTemperature,
        hw_vendor_specific: Vec<u8>,
    }

    impl MockVendorHandler {
        fn new() -> Self {
            Self {
                caps: VendorCapabilities(0),
                heartbeat_val: 0,
                vendor_status_data: Vec::new(),
                hw_flags: HwStatusFlags(0),
                hw_vendor_status_byte: 0,
                hw_composite_temp: CompositeTemperature::NoData,
                hw_vendor_specific: Vec::new(),
            }
        }

        fn with_all_caps() -> Self {
            Self {
                caps: VendorCapabilities(0b0011_1111),
                ..Self::new()
            }
        }
    }

    impl crate::vendor::VendorHandler for MockVendorHandler {
        fn capabilities(&self) -> VendorCapabilities {
            self.caps
        }

        fn execute_reset(&mut self, _reset: &DeviceReset) {}

        fn handle_vendor_write(&mut self, _data: &[u8]) -> Result<(), OcpError> {
            Ok(())
        }

        fn handle_vendor_read(&self, _buf: &mut [u8]) -> Result<usize, OcpError> {
            Ok(0)
        }

        fn vendor_device_status(&self, buf: &mut [u8]) -> usize {
            let len = self.vendor_status_data.len();
            buf[..len].copy_from_slice(&self.vendor_status_data);
            len
        }

        fn heartbeat(&self) -> u16 {
            self.heartbeat_val
        }

        fn hw_status(&self) -> HwStatus<'_> {
            HwStatus::new(
                self.hw_flags,
                self.hw_vendor_status_byte,
                self.hw_composite_temp,
                &self.hw_vendor_specific,
            )
            .expect("MockVendorHandler: hw_status fields are valid")
        }
    }

    struct MockTransport {
        recv_data: Vec<Vec<u8>>,
        recv_idx: usize,
        sent: Vec<Vec<u8>>,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                recv_data: Vec::new(),
                recv_idx: 0,
                sent: Vec::new(),
            }
        }

        fn enqueue(&mut self, msg: Vec<u8>) {
            self.recv_data.push(msg);
        }
    }

    impl Transport for MockTransport {
        fn recv_msg(&mut self, _timeout: Timeout) -> Result<&[u8], OcpError> {
            if self.recv_idx < self.recv_data.len() {
                let idx = self.recv_idx;
                self.recv_idx += 1;
                Ok(&self.recv_data[idx])
            } else {
                Err(OcpError::TransportTimeout)
            }
        }

        fn send_msg(&mut self, msg: &[u8]) -> Result<(), OcpError> {
            self.sent.push(msg.to_vec());
            Ok(())
        }
    }

    fn test_config() -> RecoveryDeviceConfig<'static> {
        let desc = DeviceDescriptor::PciVendor(PciVendorDescriptor {
            vendor_id: 0x1234,
            device_id: 0x5678,
            subsystem_vendor_id: 0,
            subsystem_id: 0,
            revision_id: 0,
        });
        RecoveryDeviceConfig {
            device_id: DeviceId::new(desc, &[]).unwrap(),
            major_version: 1,
            minor_version: 1,
            max_response_time: 17,
            heartbeat_period: 0,
            local_c_image_support: true,
        }
    }

    #[test]
    fn default_state_after_construction() {
        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        assert_eq!(sm.device_status_value, DeviceStatusValue::StatusPending);
        assert_eq!(sm.protocol_error, ProtocolError::NoError);
        assert_eq!(sm.recovery_reason, RecoveryReasonCode::NoBootFailure);

        assert_eq!(
            sm.recovery_status.status(),
            DeviceRecoveryStatus::NotInRecovery
        );
        assert_eq!(sm.recovery_status.image_index(), 0);

        assert_eq!(sm.recovery_ctrl.cms, 0);
        assert_eq!(
            sm.recovery_ctrl.image_selection,
            ImageSelection::NoOperation
        );
        assert_eq!(
            sm.recovery_ctrl.activate,
            ActivateRecoveryImage::DoNotActivate
        );

        assert_eq!(sm.device_reset.reset_control, ResetControl::NoReset);
        assert_eq!(sm.device_reset.forced_recovery, ForcedRecoveryMode::None);
        assert_eq!(
            sm.device_reset.interface_control,
            InterfaceControl::DisableMastering
        );

        assert_eq!(sm.indirect_ctrl.cms, 0);
        assert_eq!(sm.indirect_ctrl.imo, 0);

        assert_eq!(sm.indirect_fifo_ctrl.cms, 0);
        assert!(!sm.indirect_fifo_ctrl.reset);
        assert_eq!(sm.indirect_fifo_ctrl.image_size, 0);

        assert_eq!(sm.cms_count, 0);
    }

    #[test]
    fn process_command_stub_returns_none() {
        let mut transport = MockTransport::new();
        transport.enqueue(vec![0x22]); // PROT_CAP command code
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let action = sm.process_command(Timeout::Never).unwrap();
        assert_eq!(action, RecoveryAction::None);
    }

    #[test]
    fn process_command_propagates_transport_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let err = sm.process_command(Timeout::Never).unwrap_err();
        assert_eq!(err, OcpError::TransportTimeout);
    }

    #[test]
    fn lookup_indirect_region_finds_match() {
        let mut buf0 = [0u8; 64];
        let mut buf3 = [0u8; 64];
        let mut r0 = SliceIndirectRegion::new(&mut buf0, CmsRegionType::CodeSpace).unwrap();
        let mut r3 = SliceIndirectRegion::new(&mut buf3, CmsRegionType::Log).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 2] = [(0, &mut r0), (3, &mut r3)];

        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        assert!(sm.lookup_indirect_region(0).is_some());
        assert!(sm.lookup_indirect_region(3).is_some());
        assert!(sm.lookup_indirect_region(1).is_none());
        assert!(sm.lookup_indirect_region(255).is_none());
    }

    #[test]
    fn lookup_fifo_region_finds_match() {
        let mut buf = [0u8; 64];
        let mut region = SliceFifoRegion::new(&mut buf, FifoCmsRegionType::CodeSpace, 16).unwrap();
        let mut regions: [(u8, &mut dyn FifoCmsRegion); 1] = [(5, &mut region)];

        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut regions,
            MockVendorHandler::new(),
        )
        .unwrap();

        assert!(sm.lookup_fifo_region(5).is_some());
        assert!(sm.lookup_fifo_region(0).is_none());
        assert!(sm.lookup_fifo_region(3).is_none());
    }

    #[test]
    fn set_protocol_error_updates_state() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        assert_eq!(sm.protocol_error, ProtocolError::NoError);
        sm.set_protocol_error(ProtocolError::UnsupportedCommand);
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }

    // -- PROT_CAP handler tests --

    #[test]
    fn prot_cap_read_returns_magic_version_and_config() {
        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        assert_eq!(&msg[0..8], &prot_cap::MAGIC);
        assert_eq!(msg[8], 1); // major
        assert_eq!(msg[9], 1); // minor
        assert_eq!(msg[13], 17); // max_response_time
        assert_eq!(msg[14], 0); // heartbeat_period
    }

    #[test]
    fn prot_cap_read_no_regions_reports_local_c_image() {
        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        let caps_raw = u16::from_le_bytes([msg[10], msg[11]]);
        let caps = RecoveryProtocolCapabilities(caps_raw);

        assert!(caps.identification());
        assert!(caps.device_status());
        assert!(caps.local_c_image_support());
        assert!(!caps.push_c_image_support());
        assert!(!caps.recovery_memory_access());
        assert!(!caps.fifo_cms_support());
        assert_eq!(msg[12], 0);
    }

    #[test]
    fn prot_cap_read_with_indirect_code_sets_push_and_memory_access() {
        let mut buf = [0u8; 64];
        let mut region = SliceIndirectRegion::new(&mut buf, CmsRegionType::CodeSpace).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut region)];

        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        let caps_raw = u16::from_le_bytes([msg[10], msg[11]]);
        let caps = RecoveryProtocolCapabilities(caps_raw);

        assert!(caps.push_c_image_support());
        assert!(caps.recovery_memory_access());
        assert!(!caps.fifo_cms_support());
        assert_eq!(msg[12], 1);
    }

    #[test]
    fn prot_cap_read_with_fifo_only_sets_push_and_fifo() {
        let mut buf = [0u8; 64];
        let mut region = SliceFifoRegion::new(&mut buf, FifoCmsRegionType::CodeSpace, 16).unwrap();
        let mut regions: [(u8, &mut dyn FifoCmsRegion); 1] = [(2, &mut region)];

        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut regions,
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        let caps_raw = u16::from_le_bytes([msg[10], msg[11]]);
        let caps = RecoveryProtocolCapabilities(caps_raw);

        // Spec 1.1 deviation: push_c_image_support is set for FIFO-only
        // configurations even without recovery_memory_access.
        assert!(caps.push_c_image_support());
        assert!(caps.fifo_cms_support());
        assert!(!caps.recovery_memory_access());
        assert_eq!(msg[12], 1);
    }

    #[test]
    fn prot_cap_read_cms_count_is_total_regions() {
        let mut buf0 = [0u8; 64];
        let mut buf1 = [0u8; 64];
        let mut r0 = SliceIndirectRegion::new(&mut buf0, CmsRegionType::CodeSpace).unwrap();
        let mut r1 = SliceIndirectRegion::new(&mut buf1, CmsRegionType::Log).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 2] = [(0, &mut r0), (7, &mut r1)];

        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        assert_eq!(msg[12], 2); // 2 regions total (indices are not contiguous)
    }

    #[test]
    fn prot_cap_read_cms_count_spans_indirect_and_fifo() {
        let mut ibuf = [0u8; 64];
        let mut fbuf = [0u8; 64];
        let mut ir = SliceIndirectRegion::new(&mut ibuf, CmsRegionType::CodeSpace).unwrap();
        let mut fr = SliceFifoRegion::new(&mut fbuf, FifoCmsRegionType::CodeSpace, 16).unwrap();
        let mut indirect: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut ir)];
        let mut fifo: [(u8, &mut dyn FifoCmsRegion); 1] = [(1, &mut fr)];

        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut indirect,
            &mut fifo,
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        assert_eq!(msg[12], 2); // 1 indirect + 1 fifo = 2
    }

    #[test]
    fn new_rejects_indirect_without_cms0_code() {
        let mut buf = [0u8; 64];
        let mut region = SliceIndirectRegion::new(&mut buf, CmsRegionType::Log).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut region)];

        let mut transport = MockTransport::new();
        let result = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        );

        assert!(matches!(result, Err(OcpError::IndirectCms0NotCodeSpace)));
    }

    #[test]
    fn new_rejects_indirect_missing_cms0() {
        let mut buf = [0u8; 64];
        let mut region = SliceIndirectRegion::new(&mut buf, CmsRegionType::CodeSpace).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 1] = [(1, &mut region)];

        let mut transport = MockTransport::new();
        let result = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        );

        assert!(matches!(result, Err(OcpError::IndirectCms0NotCodeSpace)));
    }

    #[test]
    fn prot_cap_write_sets_unsupported_command_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.handle_prot_cap_write();
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }

    #[test]
    fn prot_cap_read_vendor_caps_all_set() {
        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::with_all_caps(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        let caps_raw = u16::from_le_bytes([msg[10], msg[11]]);
        let caps = RecoveryProtocolCapabilities(caps_raw);

        assert!(caps.forced_recovery());
        assert!(caps.mgmt_reset());
        assert!(caps.device_reset());
        assert!(caps.interface_isolation());
        assert!(caps.hardware_status());
        assert!(caps.vendor_command());
    }

    #[test]
    fn prot_cap_read_vendor_caps_none_set() {
        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_prot_cap_read().unwrap();
        let caps_raw = u16::from_le_bytes([msg[10], msg[11]]);
        let caps = RecoveryProtocolCapabilities(caps_raw);

        assert!(!caps.forced_recovery());
        assert!(!caps.mgmt_reset());
        assert!(!caps.device_reset());
        assert!(!caps.interface_isolation());
        assert!(!caps.hardware_status());
        assert!(!caps.vendor_command());
    }

    // -- Duplicate CMS index tests --

    #[test]
    fn new_rejects_duplicate_indirect_indices() {
        let mut buf0 = [0u8; 64];
        let mut buf1 = [0u8; 64];
        let mut r0 = SliceIndirectRegion::new(&mut buf0, CmsRegionType::CodeSpace).unwrap();
        let mut r1 = SliceIndirectRegion::new(&mut buf1, CmsRegionType::Log).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 2] = [(0, &mut r0), (0, &mut r1)];

        let mut transport = MockTransport::new();
        let result = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        );

        assert!(matches!(result, Err(OcpError::DuplicateCmsIndex)));
    }

    #[test]
    fn new_rejects_duplicate_fifo_indices() {
        let mut buf0 = [0u8; 64];
        let mut buf1 = [0u8; 64];
        let mut r0 = SliceFifoRegion::new(&mut buf0, FifoCmsRegionType::CodeSpace, 16).unwrap();
        let mut r1 = SliceFifoRegion::new(&mut buf1, FifoCmsRegionType::Log, 16).unwrap();
        let mut regions: [(u8, &mut dyn FifoCmsRegion); 2] = [(2, &mut r0), (2, &mut r1)];

        let mut transport = MockTransport::new();
        let result = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut regions,
            MockVendorHandler::new(),
        );

        assert!(matches!(result, Err(OcpError::DuplicateCmsIndex)));
    }

    #[test]
    fn new_rejects_overlapping_indirect_and_fifo_index() {
        let mut ibuf = [0u8; 64];
        let mut fbuf = [0u8; 64];
        let mut ir = SliceIndirectRegion::new(&mut ibuf, CmsRegionType::CodeSpace).unwrap();
        let mut fr = SliceFifoRegion::new(&mut fbuf, FifoCmsRegionType::CodeSpace, 16).unwrap();
        let mut indirect: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut ir)];
        let mut fifo: [(u8, &mut dyn FifoCmsRegion); 1] = [(0, &mut fr)];

        let mut transport = MockTransport::new();
        let result = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut indirect,
            &mut fifo,
            MockVendorHandler::new(),
        );

        assert!(matches!(result, Err(OcpError::DuplicateCmsIndex)));
    }

    // -- DEVICE_ID handler tests --

    #[test]
    fn device_id_read_returns_serialized_id() {
        let mut transport = MockTransport::new();
        let config = test_config();
        let expected = config.device_id.to_message();
        let sm = RecoveryStateMachine::new(
            config,
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let (buf, len) = sm.handle_device_id_read();
        assert_eq!(len, expected.1);
        assert_eq!(&buf[..len], &expected.0[..expected.1]);
    }

    #[test]
    fn device_id_write_sets_unsupported_command_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.handle_device_id_write();
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }

    // -- DEVICE_STATUS handler tests --

    #[test]
    fn device_status_read_returns_default_status() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let (msg, len) = sm.handle_device_status_read().unwrap();
        assert_eq!(len, 7);
        assert_eq!(msg[0], DeviceStatusValue::StatusPending as u8);
        assert_eq!(msg[1], ProtocolError::NoError as u8);
        assert_eq!(u16::from_le_bytes([msg[2], msg[3]]), 0x00); // NoBootFailure
        assert_eq!(u16::from_le_bytes([msg[4], msg[5]]), 0); // heartbeat
        assert_eq!(msg[6], 0); // vendor status length
    }

    #[test]
    fn device_status_read_clears_protocol_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.set_protocol_error(ProtocolError::UnsupportedCommand);

        let (msg, _) = sm.handle_device_status_read().unwrap();
        assert_eq!(msg[1], ProtocolError::UnsupportedCommand as u8);
        assert_eq!(sm.protocol_error, ProtocolError::NoError);
    }

    #[test]
    fn device_status_read_includes_vendor_status() {
        let mut transport = MockTransport::new();
        let mut vendor = MockVendorHandler::new();
        vendor.vendor_status_data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut sm =
            RecoveryStateMachine::new(test_config(), &mut transport, &mut [], &mut [], vendor)
                .unwrap();

        let (msg, len) = sm.handle_device_status_read().unwrap();
        assert_eq!(len, 11); // 7 + 4 vendor bytes
        assert_eq!(msg[6], 4);
        assert_eq!(&msg[7..11], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn device_status_read_heartbeat_from_vendor() {
        let mut transport = MockTransport::new();
        let mut vendor = MockVendorHandler::new();
        vendor.heartbeat_val = 42;
        let mut sm =
            RecoveryStateMachine::new(test_config(), &mut transport, &mut [], &mut [], vendor)
                .unwrap();

        let (msg, _) = sm.handle_device_status_read().unwrap();
        assert_eq!(u16::from_le_bytes([msg[4], msg[5]]), 42);
    }

    #[test]
    fn device_status_write_sets_unsupported_command_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.handle_device_status_write();
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }

    // -- RECOVERY_STATUS handler tests --

    #[test]
    fn recovery_status_read_returns_default() {
        let mut transport = MockTransport::new();
        let sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_recovery_status_read();
        assert_eq!(msg.len(), 2);
        let byte0 = recovery_status::RecoveryStatusByte0(msg[0]);
        assert_eq!(byte0.status(), DeviceRecoveryStatus::NotInRecovery as u8);
        assert_eq!(byte0.image_index(), 0);
        assert_eq!(msg[1], 0); // vendor status
    }

    #[test]
    fn recovery_status_write_sets_unsupported_command_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.handle_recovery_status_write();
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }

    // -- HW_STATUS handler tests --

    #[test]
    fn hw_status_read_returns_vendor_hw_status() {
        let mut transport = MockTransport::new();
        let mut vendor = MockVendorHandler::new();
        let mut flags = HwStatusFlags(0);
        flags.set_temp_critical(true);
        vendor.hw_flags = flags;
        vendor.hw_vendor_status_byte = 0xAB;
        vendor.hw_composite_temp = CompositeTemperature::Celsius(42);
        vendor.hw_vendor_specific = vec![0x01, 0x02, 0x03];

        let sm = RecoveryStateMachine::new(test_config(), &mut transport, &mut [], &mut [], vendor)
            .unwrap();

        let (msg, len) = sm.handle_hw_status_read();
        assert_eq!(len, 7); // 4 header + 3 vendor specific
        assert_eq!(msg[0], 0x01); // temp_critical flag
        assert_eq!(msg[1], 0xAB); // vendor hw status byte
        assert_eq!(msg[2], 42); // composite temp 42°C
        assert_eq!(msg[3], 3); // vendor specific length
        assert_eq!(&msg[4..7], &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn hw_status_write_sets_unsupported_command_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.handle_hw_status_write();
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }

    // -- INDIRECT_STATUS handler tests --

    #[test]
    fn indirect_status_read_returns_region_type_and_size() {
        let mut buf = [0u8; 64];
        let mut region = SliceIndirectRegion::new(&mut buf, CmsRegionType::CodeSpace).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut region)];

        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_indirect_status_read();
        let status = IndirectStatus::new(StatusFlags(0), CmsRegionType::CodeSpace, false, 64);
        assert_eq!(msg, status.to_message());
    }

    #[test]
    fn indirect_status_read_unsupported_for_invalid_index() {
        let mut buf = [0u8; 64];
        let mut region = SliceIndirectRegion::new(&mut buf, CmsRegionType::CodeSpace).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut region)];

        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.indirect_ctrl.cms = 99;
        let msg = sm.handle_indirect_status_read();
        let expected =
            IndirectStatus::new(StatusFlags(0), CmsRegionType::Unsupported, false, 0).to_message();
        assert_eq!(msg, expected);
    }

    #[test]
    fn indirect_status_read_unsupported_for_fifo_index() {
        let mut ibuf = [0u8; 64];
        let mut fbuf = [0u8; 64];
        let mut ir = SliceIndirectRegion::new(&mut ibuf, CmsRegionType::CodeSpace).unwrap();
        let mut fr = SliceFifoRegion::new(&mut fbuf, FifoCmsRegionType::CodeSpace, 16).unwrap();
        let mut indirect: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut ir)];
        let mut fifo: [(u8, &mut dyn FifoCmsRegion); 1] = [(1, &mut fr)];

        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut indirect,
            &mut fifo,
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.indirect_ctrl.cms = 1;
        let msg = sm.handle_indirect_status_read();
        let expected =
            IndirectStatus::new(StatusFlags(0), CmsRegionType::Unsupported, false, 0).to_message();
        assert_eq!(msg, expected);
    }

    #[test]
    fn indirect_status_read_overflow_reported_and_cleared() {
        let mut buf = [0u8; 4];
        let mut region = SliceIndirectRegion::new(&mut buf, CmsRegionType::CodeSpace).unwrap();
        region.write(&[0xAA; 4]).unwrap();

        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 1] = [(0, &mut region)];

        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        let msg = sm.handle_indirect_status_read();
        assert_eq!(msg[0] & 0x01, 0x01, "overflow flag should be set");

        let msg2 = sm.handle_indirect_status_read();
        assert_eq!(msg2[0] & 0x01, 0x00, "overflow flag should be cleared");
    }

    #[test]
    fn indirect_status_write_sets_unsupported_command_error() {
        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut [],
            &mut [],
            MockVendorHandler::new(),
        )
        .unwrap();

        sm.handle_indirect_status_write();
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }
}
