# OCP Recovery Interface - Implementation Plan

## Overview

This document describes the architecture for a transport-agnostic OCP Secure Firmware Recovery
(v1.1) state machine. The implementation targets embedded ROM and runtime firmware in a
`no_std` environment. All state transitions, command handling, and error reporting follow the
spec precisely and must be rigorously tested.

The existing crate already provides:
- Wire-format structs for all 14 recovery commands (`protocol/` submodules)
- A `Transport` trait abstracting SMBus / I3C / USB
- An `OcpError` enum for validation and transport errors

This plan covers the design of the recovery state machine itself, housed in `interface.rs`.

---

## Design Principles

1. **no_std, zero-alloc** -- All storage is provided at construction time. No heap. Const-generic
   or builder-pattern configuration sized at compile time.
2. **Transport-agnostic** -- The state machine operates on parsed command structs and produces
   response bytes. It never touches bus-level details.
3. **Integrator-configurable** -- CMS topology, device identity, hardware identity, and vendor
   extensions are all injected by the integrator.
4. **Spec-faithful error model** -- Every protocol error defined in Section 9.1 is tracked in
   `DEVICE_STATUS.PROTOCOL_ERROR` with clear-on-read semantics.
5. **Testable in isolation** -- The state machine has no side effects beyond writing to
   integrator-provided memory regions and invoking integrator-provided callbacks.

---

## Component Memory Space (CMS) Abstraction

The integrator provides an array of CMS descriptors at construction time. Each descriptor
specifies the type and backing storage for one CMS index.

### CMS Region Types

| Type | Access | Indirect Interface | Notes |
|------|--------|--------------------|-------|
| Code | Read/Write | Memory Window (`INDIRECT_*`) | CMS 0 is the default recovery code region |
| Log | Read Only | Memory Window (`INDIRECT_*`) | Critical logging, circular buffer |
| VendorRW | Read/Write | Memory Window (`INDIRECT_*`) | Vendor-defined |
| VendorRO | Read Only | Memory Window (`INDIRECT_*`) | Vendor-defined logs |
| VendorWO | Write Only | Memory Window (`INDIRECT_*`) | Vendor-defined |
| FifoCode | Write Only | FIFO (`INDIRECT_FIFO_*`) | Code push via FIFO |
| FifoLog | Read Only | FIFO (`INDIRECT_FIFO_*`) | Log read via FIFO |
| FifoVendorWO | Write Only | FIFO (`INDIRECT_FIFO_*`) | Vendor-defined FIFO |
| FifoVendorRO | Read Only | FIFO (`INDIRECT_FIFO_*`) | Vendor-defined FIFO |

### CMS Traits

The INDIRECT (memory-window) and INDIRECT_FIFO interfaces have fundamentally different
access models: memory-window regions use random-access offset-based I/O with polling
semantics, while FIFO regions use streaming producer/consumer I/O with index tracking.
Two independent traits capture these differences cleanly.

#### IndirectCmsRegion -- Memory Window Regions

Used with `INDIRECT_CTRL` / `INDIRECT_STATUS` / `INDIRECT_DATA`.

```rust
/// Integrator-provided backing store for a memory-window CMS region.
///
/// Accessed via INDIRECT_CTRL/STATUS/DATA commands. The region owns its
/// indirect memory offset (IMO) and manages auto-increment, wrap, and
/// overflow tracking internally. Status metadata (region type, size,
/// flags, polling) is returned as a complete `IndirectStatus` struct.
pub trait IndirectCmsRegion {
    /// Returns the current INDIRECT_STATUS for this region.
    ///
    /// The returned `IndirectStatus` contains the status flags (overflow,
    /// read-only error, polling error, write-only error), region type,
    /// polling bit, and region size. The state machine serializes this
    /// directly for INDIRECT_STATUS reads.
    fn status(&self) -> IndirectStatus;

    /// Returns the current indirect memory offset (IMO) in bytes.
    /// Always 4-byte aligned. Used when INDIRECT_CTRL is read back.
    fn imo(&self) -> u32;

    /// Sets the indirect memory offset (IMO) in bytes.
    /// The state machine writes this when INDIRECT_CTRL is written.
    /// Unaligned values are truncated to the previous 4-byte boundary.
    fn set_imo(&mut self, offset: u32);

    /// Write `data` at the current IMO.
    /// The implementation auto-increments the IMO by the transfer size
    /// rounded up to the next 4-byte boundary. If the IMO exceeds the
    /// region size, it wraps to 0 and the implementation signals overflow.
    /// Returns an error if the region is read-only or polling is not ready.
    fn write(&mut self, data: &[u8]) -> Result<(), CmsError>;

    /// Read up to `buf.len()` bytes starting at the current IMO.
    /// The implementation auto-increments the IMO by the transfer size
    /// rounded up to the next 4-byte boundary. If the IMO exceeds the
    /// region size, it wraps to 0 and the implementation signals overflow.
    /// Returns the number of bytes actually read.
    /// Returns an error if the region is write-only or polling is not ready.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, CmsError>;

    /// Clear accumulated status flags (overflow, polling error, access errors).
    /// Called when INDIRECT_STATUS is read (clear-on-read semantics).
    fn clear_status(&mut self);

    /// Called when this CMS is selected via INDIRECT_CTRL (i.e. on CMS change).
    /// The implementation should reset the IMO and any internal state tied
    /// to a transfer session.
    fn reset(&mut self);
}
```

#### FifoCmsRegion -- FIFO Regions

Used with `INDIRECT_FIFO_CTRL` / `INDIRECT_FIFO_STATUS` / `INDIRECT_FIFO_DATA`.

```rust
/// Integrator-provided backing store for a FIFO CMS region.
///
/// Accessed via INDIRECT_FIFO_CTRL/STATUS/DATA commands. The FIFO manages
/// its own write and read indices internally. Status metadata (region type,
/// empty/full flags, indices, sizes) is returned as a complete
/// `IndirectFifoStatus` struct.
pub trait FifoCmsRegion {
    /// Returns the current INDIRECT_FIFO_STATUS for this region.
    ///
    /// The returned `IndirectFifoStatus` contains the status flags
    /// (empty, full), region type, write index, read index, FIFO size,
    /// and max transfer size. The state machine serializes this directly
    /// for INDIRECT_FIFO_STATUS reads.
    fn status(&self) -> IndirectFifoStatus;

    /// Push data into the FIFO (write direction).
    /// Returns an error if the FIFO is full (the state machine will NACK)
    /// or if the region is read-only.
    fn push(&mut self, data: &[u8]) -> Result<(), CmsError>;

    /// Pop data from the FIFO (read direction).
    /// Reads up to `buf.len()` bytes and returns the number actually read.
    /// Returns an error if the FIFO is empty or if the region is write-only.
    fn pop(&mut self, buf: &mut [u8]) -> Result<usize, CmsError>;

    /// Reset the FIFO: write and read indices return to initial values,
    /// FIFO becomes empty. Called when INDIRECT_FIFO_CTRL byte 1 = 0x01.
    fn reset(&mut self);
}
```

### Providing CMS Regions to the State Machine

The state machine constructor takes two separate slices: one for memory-window regions
and one for FIFO regions. Each element is a tuple of `(cms_index, &mut dyn Trait)`,
binding the CMS index directly to its backing region. A given CMS index must appear in
exactly one of the two slices -- never both.

```rust
impl<'a, T: Transport, V: VendorHandler> RecoveryStateMachine<'a, T, V> {
    pub fn new(
        config: RecoveryDeviceConfig<'a>,
        transport: &'a mut T,
        indirect_regions: &'a mut [(u8, &'a mut dyn IndirectCmsRegion)],
        fifo_regions: &'a mut [(u8, &'a mut dyn FifoCmsRegion)],
        vendor: V,
    ) -> Self;
}
```

- The total CMS count reported in `PROT_CAP` byte 12 is derived from the highest CMS
  index present across both slices (or the integrator can provide it explicitly in config).
- When `INDIRECT_CTRL` selects CMS index N, the state machine scans `indirect_regions`
  for a tuple with matching index. If found, the request is routed to that region. If the
  index is found in `fifo_regions` instead, `INDIRECT_STATUS` returns "Unsupported Region".
  If the index is not found in either slice, `INDIRECT_STATUS` returns "Unsupported Region".
- Symmetrically, `INDIRECT_FIFO_CTRL` selecting CMS index N scans `fifo_regions` for a
  match, returning "Unsupported Region" if the index belongs to `indirect_regions` or is
  absent entirely.
- Because the slices hold `dyn` references, each element can be a different concrete type,
  allowing a single state machine instance to manage a mix of RAM, flash, register-backed,
  or any other region implementations.

---

## Integrator-Provided Configuration

### Static Identity

Provided once at construction, immutable for the lifetime of the state machine.

```rust
pub struct RecoveryDeviceConfig<'a> {
    /// DEVICE_ID response payload. Built from protocol::device_id::DeviceId.
    pub device_id: DeviceId<'a>,

    /// PROT_CAP fields derived from the integrator's chosen feature set.
    /// cms_regions count is derived from the region slices.
    /// Capabilities bits are derived from what the integrator enables.
    pub major_version: u8,
    pub minor_version: u8,
    pub max_response_time: u8,
    pub heartbeat_period: u8,

    /// Whether the device supports local c-image recovery (PROT_CAP bit 6).
    pub local_c_image_support: bool,
}
```

### Vendor Capabilities

The integrator reports which optional protocol features the device supports via
`VendorCapabilities`, a bitfield returned by `VendorHandler::capabilities()`.
These bits directly populate PROT_CAP bits 1-3 and 8-10:

```rust
bitfield! {
    pub struct VendorCapabilities(u8);
    pub forced_recovery, set_forced_recovery: 0;    // PROT_CAP bit 1
    pub mgmt_reset, set_mgmt_reset: 1;              // PROT_CAP bit 2
    pub device_reset, set_device_reset: 2;           // PROT_CAP bit 3
    pub interface_isolation, set_interface_isolation: 3; // PROT_CAP bit 8
    pub hardware_status, set_hardware_status: 4;     // PROT_CAP bit 9
    pub vendor_command, set_vendor_command: 5;        // PROT_CAP bit 10
}
```

### Vendor Callbacks

The integrator provides a trait implementation for vendor-specific behavior:

```rust
pub trait VendorHandler {
    /// Report which optional protocol capabilities the device supports.
    fn capabilities(&self) -> VendorCapabilities;

    /// Execute a device reset (called on DEVICE_RESET write with non-zero control).
    fn execute_reset(&mut self, reset: &DeviceReset);

    /// Handle a VENDOR command (cmd=0x2C) write.
    fn handle_vendor_write(&mut self, data: &[u8]) -> Result<(), OcpError>;

    /// Handle a VENDOR command (cmd=0x2C) read.
    fn handle_vendor_read(&self, buf: &mut [u8]) -> Result<usize, OcpError>;

    /// Supply vendor status bytes for DEVICE_STATUS (bytes 7-254).
    fn vendor_device_status(&self, buf: &mut [u8]) -> usize;

    /// Return the current heartbeat counter (DEVICE_STATUS bytes 4-5, 12-bit).
    fn heartbeat(&self) -> u16;

    /// Return the current HW_STATUS snapshot (cmd=0x28).
    fn hw_status(&self) -> HwStatus;
}
```

`NoopVendorHandler` is provided for integrators that do not need vendor extensions.
It returns `VendorCapabilities(0)` (no optional features), returns zero/empty for
`vendor_device_status` and `heartbeat`, and panics with `unimplemented!()` for
methods that should never be called when their corresponding capability is not
advertised (`execute_reset`, `handle_vendor_write`, `handle_vendor_read`, `hw_status`).

---

## Recovery State Machine

### States

The state machine tracks the device's recovery lifecycle as defined in Section 6:

```
                 ┌─────────────────────┐
        ┌───────>│    StatusPending    │
        │        └──────────┬──────────┘
        │                   │ boot completes
        │                   v
        │        ┌─────────────────────┐
   reset│   ┌───>│       Healthy       │
        │   │    └──────────┬──────────┘
        │   │               │ error / forced recovery
        │   │               v
        │   │    ┌─────────────────────┐
        │   │    │    RecoveryMode     │<──── internal error detected
        │   │    └──────────┬──────────┘
        │   │               │ image pushed or c-image selected
        │   │               v
        │   │    ┌─────────────────────┐
        │   │    │   RecoveryPending   │
        │   │    └──────────┬──────────┘
        │   │               │ activation
        │   │               v
        │   │    ┌─────────────────────┐
        │   └────│  RecoverySuccessful │
        │        └─────────────────────┘
        │
        └─── DeviceError / BootFailure / FatalError (terminal or reset-required)
```

### Internal State Structure

```rust
pub struct RecoveryStateMachine<'a, T: Transport, V: VendorHandler> {
    /// The transport medium this state machine operates on.
    transport: &'a mut T,

    // --- Stored as protocol structs (no borrowed data, direct reuse) ---

    /// RECOVERY_STATUS (cmd=0x27) state.
    recovery_status: RecoveryStatus,

    /// RECOVERY_CTRL (cmd=0x26) state.
    /// Updated on writes; read back directly.
    recovery_ctrl: RecoveryCtrl,

    /// DEVICE_RESET (cmd=0x25) state.
    /// Tracks reset control, forced recovery mode, and interface mastering.
    device_reset: DeviceReset,

    /// INDIRECT_CTRL (cmd=0x29) state.
    /// Tracks the currently selected CMS index for memory-window access.
    indirect_ctrl: IndirectCtrl,

    /// INDIRECT_FIFO_CTRL (cmd=0x2D) state.
    /// Tracks the currently selected CMS index and image size for FIFO access.
    indirect_fifo_ctrl: IndirectFifoCtrl,

    // --- DEVICE_STATUS fields stored individually ---
    // DeviceStatus<'a> carries a borrowed vendor_status slice that comes
    // from VendorHandler at read time, so it cannot be stored directly.
    // The state machine stores the mutable fields and builds the full
    // DeviceStatus<'_> on each read by calling vendor.vendor_device_status().

    /// DEVICE_STATUS byte 0.
    device_status_value: DeviceStatusValue,
    /// DEVICE_STATUS byte 1 (clear-on-read).
    protocol_error: ProtocolError,
    /// DEVICE_STATUS bytes 2-3.
    recovery_reason: RecoveryReasonCode,

    // --- Static config (carries lifetime for DeviceId's vendor string) ---

    /// Static device configuration (identity, capabilities).
    config: RecoveryDeviceConfig<'a>,

    // --- CMS regions ---

    /// Memory-window CMS regions. Each tuple is (cms_index, region).
    /// dyn references allow heterogeneous implementations in one slice.
    indirect_regions: &'a mut [(u8, &'a mut dyn IndirectCmsRegion)],

    /// FIFO CMS regions. Each tuple is (cms_index, region).
    /// dyn references allow heterogeneous implementations in one slice.
    fifo_regions: &'a mut [(u8, &'a mut dyn FifoCmsRegion)],

    /// Integrator-provided vendor handler.
    vendor: V,
}
```

Protocol structs are reused directly where they contain no borrowed data:
`RecoveryStatus`, `RecoveryCtrl`, `DeviceReset`, `IndirectCtrl`, `IndirectFifoCtrl`.
Command reads serialize the stored struct; command writes deserialize into it. The
state machine mutates these structs as side effects of state transitions.

`DeviceStatus<'a>` cannot be stored because its `vendor_status: &'a [u8]` comes from
`VendorHandler::vendor_device_status()` at read time. Instead, the state machine stores
the four mutable fields individually and assembles a `DeviceStatus<'_>` on each read.

`HwStatus<'a>` is fully dynamic and returned by `VendorHandler::hw_status()` at read time
-- nothing is stored in the state machine.

`DeviceId<'a>` is static configuration stored inside `RecoveryDeviceConfig<'a>`.

---

## Command Dispatch

The state machine owns the `Transport` and handles receive/send internally. It exposes
a single entry point that blocks for the next command, processes it, sends the response
(if any), and returns an action enum telling the caller what (if anything) it needs to do:

```rust
/// Actions the integrator must handle after process_command returns.
pub enum RecoveryAction {
    /// No integrator action required. The command was fully handled.
    None,

    /// The integrator should activate the recovery image.
    /// After performing activation, the integrator calls
    /// complete_activation() to report the result.
    ActivateRecoveryImage,

    /// The integrator should perform a device reset.
    DeviceReset,

    /// The integrator should perform a management-only reset.
    ManagementReset,
}

impl<'a, T: Transport, V: VendorHandler> RecoveryStateMachine<'a, T, V> {
    /// Block for the next command on the transport, process it, send any
    /// response, and return an action for the integrator to handle.
    ///
    /// The state machine handles all protocol-level concerns internally:
    /// command parsing, scope checking, read/write enforcement, error
    /// reporting, and response serialization/transmission.
    ///
    /// Returns an error only if the transport itself fails. Protocol-level
    /// errors are recorded in DEVICE_STATUS and do not cause this method
    /// to return Err.
    pub fn process_command(
        &mut self,
        timeout: Timeout,
    ) -> Result<RecoveryAction, OcpError>;

    /// Called by the integrator after activation completes (successfully
    /// or not). Updates device status and recovery status accordingly.
    pub fn complete_activation(&mut self, success: bool);
}
```

Dispatch logic:

1. Parse `cmd` byte into `RecoveryCommand` enum. If unknown, set `ProtocolError::UnsupportedCommand`.
2. Check scope: commands marked scope "R" (recovery-only) require `device_status != StatusPending`.
   If not met, set `ProtocolError::UnsupportedCommand`.
3. For write to a read-only command, set `ProtocolError::UnsupportedCommand`.
4. For read to a write-only command, return empty / set error as appropriate.
5. Delegate to the per-command handler method.

### Per-Command Handlers

| Command | Read Handler | Write Handler |
|---------|-------------|---------------|
| `PROT_CAP` (0x22) | Build response from config + CMS count + derived capability bits | Error (read-only) |
| `DEVICE_ID` (0x23) | Serialize `DeviceId` from config | Error (read-only) |
| `DEVICE_STATUS` (0x24) | Serialize status; clear `protocol_error` after read | Error (read-only) |
| `DEVICE_RESET` (0x25) | Serialize current reset state | Parse `DeviceReset`; apply reset/forced-recovery/interface-control |
| `RECOVERY_CTRL` (0x26) | Serialize current recovery ctrl state | Parse `RecoveryCtrl`; validate CMS is code region; update state; trigger activation if requested |
| `RECOVERY_STATUS` (0x27) | Serialize recovery status + vendor status | Error (read-only) |
| `HW_STATUS` (0x28) | Build from `VendorHandler` hw_status methods (flags, temp, vendor status) | Error (read-only) |
| `INDIRECT_CTRL` (0x29) | Serialize current CMS + IMO | Parse `IndirectCtrl`; select CMS; reset IMO |
| `INDIRECT_STATUS` (0x2A) | Call `region.status()` to get `IndirectStatus`; serialize; then `clear_status()` | Error (read-only) |
| `INDIRECT_DATA` (0x2B) | Read from CMS at current IMO; auto-increment | Write to CMS at current IMO; auto-increment; handle wrap/errors |
| `VENDOR` (0x2C) | Delegate to `VendorHandler::handle_vendor_read` | Delegate to `VendorHandler::handle_vendor_write` |
| `INDIRECT_FIFO_CTRL` (0x2D) | Serialize current FIFO CMS + image size | Parse; select FIFO CMS; optionally reset FIFO |
| `INDIRECT_FIFO_STATUS` (0x2E) | Call `region.status()` to get `IndirectFifoStatus`; serialize | Error (read-only) |
| `INDIRECT_FIFO_DATA` (0x2F) | Read from FIFO region | Write to FIFO region; NACK on overflow |

---

## Indirect Memory Offset (IMO) Management

The IMO is owned by each `IndirectCmsRegion` implementation, not the state machine. This
allows region implementations to manage their own addressing, wrapping, and overflow tracking.

- The state machine calls `set_imo()` when `INDIRECT_CTRL` is written (byte 2-5).
  The implementation truncates unaligned values to the previous 4-byte boundary.
- On each `INDIRECT_DATA` read or write, the region implementation auto-increments its IMO
  by the transfer size rounded up to the next 4-byte boundary.
- If the IMO exceeds the region size, the implementation wraps to 0 and records overflow
  internally. The overflow flag is reported via `status()` in the `IndirectStatus` struct.
- Changing the CMS in `INDIRECT_CTRL` calls `reset()` on the newly selected region, which
  resets its IMO and clears accumulated status.
- The state machine reads back the current IMO via `imo()` when `INDIRECT_CTRL` is read.
- Polling regions: the region's `write()` / `read()` methods internally check readiness
  and return `CmsError::PollingNotReady` if not ready, recording the polling-error flag
  in its status. The state machine reports this via `status()` in `INDIRECT_STATUS`.

---

## Activation Flow

When `RECOVERY_CTRL` is written with `activate = 0xF`:

1. Validate that a recovery image source has been selected (CMS push or local c-image).
2. If the device is in `RecoveryMode`, transition to `RecoveryPending`.
3. `process_command()` returns `RecoveryAction::ActivateRecoveryImage`.
4. The integrator performs the actual reset/boot into the recovery image and then calls
   `complete_activation(success)` to report the result.
5. On success, if more stages are expected, the device increments `recovery_image_index` and
   sets `recovery_status = AwaitingRecoveryImage`. Otherwise, `recovery_status = RecoverySuccessful`.
6. On failure, `recovery_status = RecoveryFailed`.

Similarly, `DEVICE_RESET` writes that request a device reset or management reset cause
`process_command()` to return `RecoveryAction::DeviceReset` or
`RecoveryAction::ManagementReset` respectively.

---

## Error Handling (Section 9.1)

All protocol errors are recorded in `DEVICE_STATUS` byte 1 and cleared on read:

| Condition | Error Code |
|-----------|-----------|
| Unknown or unsupported command | `UnsupportedCommand` (0x01) |
| Write to read-only command | `UnsupportedCommand` (0x01) |
| Unsupported parameter value | `UnsupportedParameter` (0x02) |
| Write with wrong byte count | `LengthWriteError` (0x03) |
| CRC/PEC failure (reported by transport) | `CrcError` (0x04) |
| Any other error | `GeneralProtocolError` (0xFF) |

The state machine sets these via `self.protocol_error = ...` and the next `DEVICE_STATUS` read
returns and clears the value.

---

## Integration Pattern

```rust
// Integrator constructs concrete CMS backing stores.
// These can be different concrete types -- the state machine sees them as dyn references.
let mut code_region = RamCodeRegion::new(&mut code_buf);   // impl IndirectCmsRegion
let mut log_region = FlashLogRegion::new(&flash);          // impl IndirectCmsRegion (different type)
let mut fifo_region = RamFifoRegion::new(&mut fifo_buf);   // impl FifoCmsRegion

// Build slices of (cms_index, dyn region) tuples.
let indirect_regions: &mut [(u8, &mut dyn IndirectCmsRegion)] = &mut [
    (0, &mut code_region),   // CMS 0: RAM-backed code region
    (1, &mut log_region),    // CMS 1: flash-backed log region
];
let fifo_regions: &mut [(u8, &mut dyn FifoCmsRegion)] = &mut [
    (2, &mut fifo_region),   // CMS 2: RAM-backed FIFO code push
];

// Build config.
let config = RecoveryDeviceConfig {
    device_id: DeviceId::pci_vendor(0x1234, 0x5678, 0, 0, 0, None),
    major_version: 1,
    minor_version: 1,
    max_response_time: 17,  // 2^17 us ≈ 131 ms
    heartbeat_period: 0,
    local_c_image_support: false,
};

// Instantiate the state machine with transport.
let mut sm = RecoveryStateMachine::new(
    config,
    smbus_transport,
    indirect_regions,
    fifo_regions,
    NoopVendorHandler,
);

// Main loop: the state machine handles receive/send internally.
loop {
    match sm.process_command(Timeout::Never) {
        Ok(RecoveryAction::None) => {},
        Ok(RecoveryAction::ActivateRecoveryImage) => {
            let ok = do_activation();
            sm.complete_activation(ok);
        },
        Ok(RecoveryAction::DeviceReset) => {
            perform_device_reset();
        },
        Ok(RecoveryAction::ManagementReset) => {
            perform_mgmt_reset();
        },
        Err(e) => {
            handle_transport_error(e);
        },
    }
}
```

---

## Testing Strategy

All tests run under `std` (the crate is `#[cfg_attr(not(test), no_std)]`).

**Unit tests** live in `#[cfg(test)] mod tests` blocks within each source file. They test
the module in isolation.

**Integration tests** live in `common/ocp/tests/` and exercise the full state machine with
mock transports and slice-backed CMS regions. These test cross-module interactions and
end-to-end recovery flows.

| Layer | What is Tested | Location | Approach |
|-------|---------------|----------|----------|
| Protocol structs | Serialization round-trips, field validation, boundary values | Unit tests in each `protocol/*.rs` (partially present) | Per-field and boundary-value tests |
| Slice-backed CMS | Read/write/wrap/overflow/polling, push/pop/full/empty/reset | Unit tests in `cms/slice_indirect.rs`, `cms/slice_fifo.rs` | In-memory buffer tests |
| State machine internals | Per-command handler logic, error recording, state field mutations | Unit tests in `interface.rs` | Construct state machine, call handlers directly |
| Error paths | Every `ProtocolError` variant triggered by malformed input | Unit tests in `interface.rs` | One test per error condition in Section 9.1 |
| Command dispatch | Scope checking, read-only enforcement, unknown commands | Unit tests in `interface.rs` | Table-driven tests across all 14 command codes |
| Activation flow | Single-stage and multi-stage activation, success and failure | `tests/activation.rs` | Sequence tests: push image -> activate -> verify status |
| Full recovery flow | End-to-end from `RecoveryMode` to `RecoverySuccessful` | `tests/recovery_flow.rs` | Mock transport + slice-backed CMS, multi-command sequences |
| Multi-CMS topology | Mixed indirect + FIFO regions, unsupported region errors | `tests/cms_topology.rs` | Various CMS configurations with heterogeneous regions |

---

## File Layout

```
common/ocp/
├── src/
│   ├── lib.rs              (crate root, existing)
│   ├── error.rs            (OcpError, existing -- extend with CmsError)
│   ├── transport.rs        (Transport trait, existing)
│   ├── protocol.rs         (RecoveryCommand enum, existing)
│   ├── protocol/           (wire-format structs, existing)
│   │   ├── prot_cap.rs
│   │   ├── device_id.rs
│   │   ├── device_status.rs
│   │   ├── device_reset.rs
│   │   ├── recovery_ctrl.rs
│   │   ├── recovery_status.rs
│   │   ├── hw_status.rs
│   │   ├── indirect_ctrl.rs
│   │   ├── indirect_status.rs
│   │   ├── indirect_fifo_ctrl.rs
│   │   └── indirect_fifo_status.rs
│   ├── cms.rs              (NEW: IndirectCmsRegion, FifoCmsRegion traits, CmsError)
│   ├── cms/
│   │   ├── slice_indirect.rs  (NEW: slice-backed IndirectCmsRegion implementation)
│   │   └── slice_fifo.rs      (NEW: slice-backed FifoCmsRegion implementation)
│   ├── vendor.rs           (NEW: VendorHandler trait, NoopVendorHandler)
│   └── interface.rs        (NEW: RecoveryStateMachine, command dispatch, state transitions)
└── tests/                  (NEW: integration tests)
    ├── activation.rs       (activation flow: single/multi-stage, success/failure)
    ├── recovery_flow.rs    (end-to-end recovery with mock transport + slice CMS)
    └── cms_topology.rs     (mixed indirect + FIFO regions, unsupported region errors)
```

---

## Implementation Order

1. **`cms.rs`** -- Define `IndirectCmsRegion`, `FifoCmsRegion` traits, and `CmsError` type.
2. **`cms/slice_indirect.rs`** -- Implement `IndirectCmsRegion` backed by a `&mut [u8]` slice. Manages IMO, 4-byte-aligned auto-increment, wrap/overflow tracking, polling, and clear-on-read status.
3. **`cms/slice_fifo.rs`** -- Implement `FifoCmsRegion` backed by a `&mut [u8]` slice used as a ring buffer. Manages write/read indices, push/pop, empty/full detection, and reset.
4. **`interface.rs` -- Core state struct** -- `RecoveryStateMachine` with config, state fields, constructor.
5. **`interface.rs` -- Read-only command handlers** -- `PROT_CAP`, `DEVICE_ID`, `DEVICE_STATUS`, `RECOVERY_STATUS`, `HW_STATUS`, `INDIRECT_STATUS`, `INDIRECT_FIFO_STATUS`.
6. **`interface.rs` -- Write command handlers** -- `DEVICE_RESET`, `RECOVERY_CTRL`, `INDIRECT_CTRL`, `INDIRECT_FIFO_CTRL`.
7. **`interface.rs` -- Data transfer handlers** -- `INDIRECT_DATA` (read + write + IMO management), `INDIRECT_FIFO_DATA`.
8. **`interface.rs` -- Vendor command delegation** -- `VENDOR` read/write via `VendorHandler`.
9. **`interface.rs` -- Activation flow** -- `complete_activation()`, multi-stage support.
10. **`interface.rs` -- Command dispatch** -- `process_command()` with scope checks, error routing.
11. **Tests** -- Unit tests for each step above (including slice-backed CMS implementations), then integration tests for full recovery flows.

---

## Spec Deviations

### `push_c_image_support` with FIFO-only CMS (deviation from Spec 1.1, Section 9.2)

The OCP Secure Firmware Recovery spec v1.1 states that when `push_c_image_support`
(PROT_CAP bit 7) is set, `recovery_memory_access` (bit 5) MUST also be set. This
requirement assumes that push-based recovery is performed exclusively through the
memory-window (INDIRECT_DATA) path.

However, a device that only provides FIFO CMS regions (`INDIRECT_FIFO_DATA`) is
equally capable of accepting a pushed recovery image. Requiring
`recovery_memory_access` in this case would force the device to advertise
memory-window support it does not have, or prevent it from advertising push
capability at all.

**Our behavior:** `push_c_image_support` is set whenever any CMS region capable of
receiving a recovery image exists -- either indirect (memory-window) or FIFO. The
`validate_capabilities` check in `ProtCap` accepts `push_c_image_support` when
*either* `recovery_memory_access` or `fifo_cms_support` is set. This is believed
to be a spec bug that will be corrected in a future revision.
