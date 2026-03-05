// Licensed under the Apache-2.0 license

//! OCP Recovery Interface state machine.
//!
//! This module defines the `RecoveryStateMachine` that implements the OCP
//! Secure Firmware Recovery v1.1 command processing loop, along with the
//! `RecoveryDeviceConfig` and `RecoveryAction` types used by integrators.

use crate::cms::{FifoCmsRegion, IndirectCmsRegion};
use crate::error::OcpError;
use crate::protocol::device_id::DeviceId;
use crate::protocol::device_reset::{
    DeviceReset, ForcedRecoveryMode, InterfaceControl, ResetControl,
};
use crate::protocol::device_status::{DeviceStatusValue, ProtocolError, RecoveryReasonCode};
use crate::protocol::indirect_ctrl::IndirectCtrl;
use crate::protocol::indirect_fifo_ctrl::IndirectFifoCtrl;
use crate::protocol::recovery_ctrl::{ActivateRecoveryImage, ImageSelection, RecoveryCtrl};
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

    pub(crate) vendor: V,
}

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
    ) -> Self {
        // SAFETY: RecoveryStatus::new with status=NotInRecovery and
        // image_index=0 always succeeds (0 is within the 4-bit range).
        let recovery_status = match RecoveryStatus::new(DeviceRecoveryStatus::NotInRecovery, 0, 0) {
            Ok(s) => s,
            Err(_) => unreachable!(),
        };

        Self {
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
            device_status_value: DeviceStatusValue::StatusPending,
            protocol_error: ProtocolError::NoError,
            recovery_reason: RecoveryReasonCode::NoBootFailure,
            config,
            indirect_regions,
            fifo_regions,
            vendor,
        }
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
    use crate::protocol::indirect_fifo_status::FifoCmsRegionType;
    use crate::protocol::indirect_status::CmsRegionType;
    use crate::vendor::NoopVendorHandler;

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
            NoopVendorHandler,
        );

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
            NoopVendorHandler,
        );

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
            NoopVendorHandler,
        );

        let err = sm.process_command(Timeout::Never).unwrap_err();
        assert_eq!(err, OcpError::TransportTimeout);
    }

    #[test]
    fn lookup_indirect_region_finds_match() {
        let mut buf = [0u8; 64];
        let mut region = SliceIndirectRegion::new(&mut buf, CmsRegionType::CodeSpace).unwrap();
        let mut regions: [(u8, &mut dyn IndirectCmsRegion); 1] = [(3, &mut region)];

        let mut transport = MockTransport::new();
        let mut sm = RecoveryStateMachine::new(
            test_config(),
            &mut transport,
            &mut regions,
            &mut [],
            NoopVendorHandler,
        );

        assert!(sm.lookup_indirect_region(3).is_some());
        assert!(sm.lookup_indirect_region(0).is_none());
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
            NoopVendorHandler,
        );

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
            NoopVendorHandler,
        );

        assert_eq!(sm.protocol_error, ProtocolError::NoError);
        sm.set_protocol_error(ProtocolError::UnsupportedCommand);
        assert_eq!(sm.protocol_error, ProtocolError::UnsupportedCommand);
    }
}
