# OCP Recovery Interface - Implementation Execution Plan

This document breaks the architecture defined in `ocp_arch.md` into phases and steps.
Each step produces a library that passes `cargo build`, `cargo test`, `cargo fmt --check`,
and `cargo clippy` before proceeding to the next.

---

## Phase 1: CMS Traits and Slice-Backed Implementations

Establish the CMS abstraction layer and provide concrete, reusable implementations
backed by byte slices. These are prerequisites for everything that follows.

### Step 1.1: CMS trait definitions and error type

**Files:** `cms.rs`, `error.rs`, `lib.rs`

- Define the `IndirectCmsRegion` trait with all methods specified in the architecture:
  `region_type`, `size_4b`, `polling_required`, `imo`, `set_imo`, `write`, `read`,
  `overflow`, `poll_ready`, `clear_status`, `reset`.
- Define the `FifoCmsRegion` trait with all methods: `region_type`, `write_index`,
  `read_index`, `fifo_size_4b`, `max_transfer_size_4b`, `push`, `pop`, `is_empty`,
  `is_full`, `reset`.
- Define `CmsError` enum in `error.rs` (or within `cms.rs`) covering: `ReadOnly`,
  `WriteOnly`, `FifoFull`, `FifoEmpty`, `PollingNotReady`.
- Define `IndirectRegionType` and `FifoRegionType` enums for the region type fields.
- Add `pub mod cms;` to `lib.rs`.

**Exit criteria:** Crate compiles with the new module. No tests yet beyond compilation.

### Step 1.2: Slice-backed IndirectCmsRegion

**Files:** `cms/slice_indirect.rs`

- Implement a `SliceIndirectRegion` struct that holds a `&mut [u8]`, an
  `IndirectRegionType`, a `polling_required` flag, and internal state for IMO, overflow,
  and access-error flags.
- Implement `IndirectCmsRegion` for `SliceIndirectRegion`:
  - `set_imo` truncates to 4-byte alignment.
  - `write` copies data at current IMO, auto-increments IMO (rounded up to 4-byte
    boundary), wraps and sets overflow flag if IMO exceeds region size. Returns
    `CmsError::ReadOnly` for log/read-only region types.
  - `read` copies data from current IMO, auto-increments, wraps. Returns
    `CmsError::WriteOnly` for write-only region types.
  - `reset` zeros IMO and clears all status flags.
  - `clear_status` clears overflow and error flags.
- Unit tests in the same file:
  - Write and read back data at offset 0.
  - Auto-increment behavior across multiple writes.
  - IMO wrap and overflow flag.
  - Unaligned IMO truncation.
  - Read-only region rejects writes.
  - Write-only region rejects reads.
  - `reset` clears IMO and status.
  - `clear_status` clears overflow without affecting IMO.

**Exit criteria:** All unit tests pass. `cargo clippy` and `cargo fmt --check` clean.

### Step 1.3: Slice-backed FifoCmsRegion

**Files:** `cms/slice_fifo.rs`

- Implement a `SliceFifoRegion` struct that holds a `&mut [u8]` as a ring buffer, a
  `FifoRegionType`, a `max_transfer_size_4b` value, and internal write/read indices.
- Implement `FifoCmsRegion` for `SliceFifoRegion`:
  - `push` writes data at write index, advances write index. Returns `CmsError::FifoFull`
    if advancing would equal read index. Returns `CmsError::ReadOnly` for read-only types.
  - `pop` reads data at read index, advances read index. Returns `CmsError::FifoEmpty`
    if read index equals write index. Returns `CmsError::WriteOnly` for write-only types.
  - `is_empty` / `is_full` based on index comparison.
  - `reset` sets both indices to 0.
- Unit tests in the same file:
  - Push and pop single element.
  - Push until full, verify `is_full` and `FifoFull` error.
  - Pop until empty, verify `is_empty` and `FifoEmpty` error.
  - Wrap-around behavior (push/pop across buffer boundary).
  - Reset clears indices, FIFO becomes empty.
  - Read-only region rejects push.
  - Write-only region rejects pop.

**Exit criteria:** All unit tests pass. `cargo clippy` and `cargo fmt --check` clean.

---

## Phase 2: VendorHandler Trait and Configuration

Define the integrator callback trait and static configuration struct used by the state
machine.

### Step 2.1: VendorHandler trait and RecoveryDeviceConfig

**Files:** `interface.rs`, `lib.rs`

- Define the `VendorHandler` trait with methods: `handle_vendor_write`,
  `handle_vendor_read`, `vendor_device_status`, `heartbeat`, `hw_status`.
- Provide a `NoopVendorHandler` struct that implements `VendorHandler` with default
  no-op behavior (returns empty/zero for all methods, returns error for vendor
  read/write).
- Define `RecoveryDeviceConfig<'a>` struct with fields: `device_id`, `major_version`,
  `minor_version`, `max_response_time`, `heartbeat_period`.
- Define the `RecoveryAction` enum: `None`, `ActivateRecoveryImage`, `DeviceReset`,
  `ManagementReset`.
- Add `pub mod interface;` to `lib.rs` (if not already present).

**Exit criteria:** Crate compiles. `NoopVendorHandler` has a trivial unit test confirming
default behavior.

---

## Phase 3: State Machine Core

Build the `RecoveryStateMachine` struct, constructor, and internal helpers. No command
handling yet -- just the skeleton that compiles and can be instantiated.

### Step 3.1: RecoveryStateMachine struct and constructor

**Files:** `interface.rs`

- Define `RecoveryStateMachine<'a, T: Transport, V: VendorHandler>` with all fields
  from the architecture: `transport`, protocol structs (`recovery_status`,
  `recovery_ctrl`, `device_reset`, `indirect_ctrl`, `indirect_fifo_ctrl`),
  device status fields (`device_status_value`, `protocol_error`, `recovery_reason`),
  `config`, `indirect_regions`, `fifo_regions`, `vendor`.
- Implement `new()` that initializes all state to spec-defined defaults:
  - `device_status_value = StatusPending`
  - `protocol_error = NoProtocolError`
  - `recovery_reason = NoBootFailure`
  - `recovery_status` with status `NotInRecoveryMode`, index 0
  - `recovery_ctrl` with CMS 0, no selection, no activation
  - `device_reset` with no reset, no forced recovery, mastering disabled
  - `indirect_ctrl` with CMS 0, IMO 0
  - `indirect_fifo_ctrl` with CMS 0, no reset, image size 0
- Implement stub `process_command()` that calls `transport.recv_msg()` and returns
  `Ok(RecoveryAction::None)`.
- Implement stub `complete_activation()`.
- Add internal helper: `lookup_indirect_region(cms: u8)` that scans
  `indirect_regions` for matching index.
- Add internal helper: `lookup_fifo_region(cms: u8)` that scans `fifo_regions`
  for matching index.
- Add internal helper: `set_protocol_error(&mut self, err: ProtocolError)`.
- Unit tests:
  - Construct a state machine with a mock transport and verify default state values.
  - `lookup_indirect_region` finds correct region, returns None for missing index.
  - `lookup_fifo_region` finds correct region, returns None for missing index.

**Exit criteria:** State machine can be instantiated. All unit tests pass.

---

## Phase 4: Read-Only Command Handlers

Implement handlers for commands that only produce responses (no state mutation beyond
clear-on-read fields).

### Step 4.1: PROT_CAP handler

**Files:** `interface.rs`

- Implement `handle_prot_cap_read()` that builds a `ProtCap` from config and CMS
  region slices (derive capability bits from what's present: identification always set,
  device_status always set, push/local c-image from region types, recovery_memory_access
  from indirect regions, fifo_cms_support from fifo regions, etc.).
- Reject writes with `UnsupportedCommand` protocol error.
- Unit tests:
  - Read returns correct magic, version, capabilities, CMS count.
  - Capabilities reflect the regions provided (e.g. push_c_image set when code CMS
    exists in indirect_regions).
  - Write sets protocol error.

**Exit criteria:** Tests pass for PROT_CAP read and write rejection.

### Step 4.2: DEVICE_ID handler

**Files:** `interface.rs`

- Implement `handle_device_id_read()` that serializes `config.device_id.to_message()`.
- Reject writes with `UnsupportedCommand` protocol error.
- Unit tests:
  - Read returns correctly serialized device ID.
  - Write sets protocol error.

**Exit criteria:** Tests pass.

### Step 4.3: DEVICE_STATUS handler

**Files:** `interface.rs`

- Implement `handle_device_status_read()` that:
  - Calls `vendor.heartbeat()` and `vendor.vendor_device_status()` to get dynamic fields.
  - Assembles a `DeviceStatus` from stored fields + vendor data.
  - Serializes and returns the response.
  - Clears `self.protocol_error` after building the response (clear-on-read).
- Reject writes with `UnsupportedCommand` protocol error.
- Unit tests:
  - Read returns correct default status (StatusPending, no error, no reason).
  - Protocol error is cleared after read.
  - Vendor status bytes are included when VendorHandler provides them.
  - Heartbeat value comes from VendorHandler.
  - Write sets protocol error.

**Exit criteria:** Tests pass.

### Step 4.4: RECOVERY_STATUS handler

**Files:** `interface.rs`

- Implement `handle_recovery_status_read()` that serializes stored `recovery_status`.
- Reject writes with `UnsupportedCommand` protocol error.
- Unit tests:
  - Read returns correct default (NotInRecoveryMode, index 0).
  - Write sets protocol error.

**Exit criteria:** Tests pass.

### Step 4.5: HW_STATUS handler

**Files:** `interface.rs`

- Implement `handle_hw_status_read()` that calls `vendor.hw_status()` and serializes
  the returned `HwStatus`.
- Reject writes with `UnsupportedCommand` protocol error.
- Unit tests:
  - Read returns the HwStatus provided by VendorHandler.
  - Write sets protocol error.

**Exit criteria:** Tests pass.

### Step 4.6: INDIRECT_STATUS handler

**Files:** `interface.rs`

- Implement `handle_indirect_status_read()` that:
  - Looks up the CMS region selected by `indirect_ctrl.cms`.
  - If found, builds `IndirectStatus` from region metadata (status flags, region type,
    size). Calls `clear_status()` on the region (clear-on-read).
  - If not found (or is a FIFO region), returns "Unsupported Region" type.
- Reject writes with `UnsupportedCommand` protocol error.
- Unit tests:
  - Returns correct region type and size for a valid CMS.
  - Returns "Unsupported Region" for an invalid CMS index.
  - Returns "Unsupported Region" for a FIFO CMS index.
  - Overflow flag is reported and cleared on read.
  - Write sets protocol error.

**Exit criteria:** Tests pass.

### Step 4.7: INDIRECT_FIFO_STATUS handler

**Files:** `interface.rs`

- Implement `handle_indirect_fifo_status_read()` that:
  - Looks up the FIFO CMS region selected by `indirect_fifo_ctrl.cms`.
  - If found, builds `IndirectFifoStatus` from FIFO metadata (status, type, indices,
    sizes).
  - If not found (or is an indirect region), returns "Unsupported Region" type.
- Reject writes with `UnsupportedCommand` protocol error.
- Unit tests:
  - Returns correct FIFO metadata for a valid FIFO CMS.
  - Returns "Unsupported Region" for invalid or indirect CMS index.
  - Empty/full status bits reflect FIFO state.
  - Write sets protocol error.

**Exit criteria:** Tests pass.

---

## Phase 5: Write Command Handlers

Implement handlers for commands that mutate state machine state.

### Step 5.1: DEVICE_RESET handler

**Files:** `interface.rs`

- Implement `handle_device_reset_write()` that:
  - Parses `DeviceReset::from_message()`.
  - On length error, sets `LengthWriteError`.
  - On invalid parameter, sets `UnsupportedParameter`.
  - Stores the parsed value in `self.device_reset`.
  - Returns `RecoveryAction::DeviceReset` if `reset_control` is DeviceReset.
  - Returns `RecoveryAction::ManagementReset` if `reset_control` is ManagementReset.
  - Records forced recovery mode for next reset.
- Implement `handle_device_reset_read()` that serializes `self.device_reset`.
- Unit tests:
  - Valid write updates stored state and returns appropriate action.
  - Forced recovery mode is recorded.
  - Invalid parameter sets `UnsupportedParameter` error.
  - Wrong length sets `LengthWriteError`.
  - Read returns current state.

**Exit criteria:** Tests pass.

### Step 5.2: RECOVERY_CTRL handler

**Files:** `interface.rs`

- Implement `handle_recovery_ctrl_write()` that:
  - Parses `RecoveryCtrl::from_message()`.
  - Validates CMS is a code region (if image selection is from CMS).
  - Stores the parsed value in `self.recovery_ctrl`.
  - If `activate = Activate`, transitions device status to `RecoveryPending` and
    returns `RecoveryAction::ActivateRecoveryImage`.
  - On errors, sets appropriate protocol error.
- Implement `handle_recovery_ctrl_read()` that serializes `self.recovery_ctrl`.
- Unit tests:
  - Valid write stores CMS and image selection.
  - Activation triggers status transition and returns correct action.
  - Invalid CMS (not a code region) sets error.
  - Read returns current state.

**Exit criteria:** Tests pass.

### Step 5.3: INDIRECT_CTRL handler

**Files:** `interface.rs`

- Implement `handle_indirect_ctrl_write()` that:
  - Parses `IndirectCtrl::from_message()`.
  - If CMS changed, calls `reset()` on the newly selected indirect region.
  - Calls `set_imo()` on the selected region.
  - Stores the parsed value in `self.indirect_ctrl`.
- Implement `handle_indirect_ctrl_read()` that serializes `self.indirect_ctrl`, reading
  back the current IMO from the selected region via `imo()`.
- Unit tests:
  - Write selects CMS and sets IMO on region.
  - CMS change triggers reset on new region.
  - Read returns current CMS and IMO from region.
  - Invalid length sets error.

**Exit criteria:** Tests pass.

### Step 5.4: INDIRECT_FIFO_CTRL handler

**Files:** `interface.rs`

- Implement `handle_indirect_fifo_ctrl_write()` that:
  - Parses `IndirectFifoCtrl::from_message()`.
  - If reset field is set, calls `reset()` on the selected FIFO region.
  - Stores the parsed value in `self.indirect_fifo_ctrl`.
- Implement `handle_indirect_fifo_ctrl_read()` that serializes
  `self.indirect_fifo_ctrl`.
- Unit tests:
  - Write selects FIFO CMS and sets image size.
  - Reset field triggers FIFO reset.
  - Read returns current state.
  - Invalid length sets error.

**Exit criteria:** Tests pass.

---

## Phase 6: Data Transfer Handlers

Implement the data-path commands that move bytes through CMS regions.

### Step 6.1: INDIRECT_DATA handler

**Files:** `interface.rs`

- Implement `handle_indirect_data_write()` that:
  - Looks up the indirect region for the currently selected CMS.
  - Checks `poll_ready()` for polling regions; if not ready, ignores the transaction
    and records a polling error.
  - Calls `region.write(data)`.
  - On `CmsError::ReadOnly`, sets `UnsupportedCommand` protocol error.
- Implement `handle_indirect_data_read()` that:
  - Looks up the indirect region.
  - Checks `poll_ready()`.
  - Calls `region.read(buf)`.
  - On `CmsError::WriteOnly`, sets `UnsupportedCommand` protocol error.
- Unit tests:
  - Write data, read it back via separate read at same offset (reset IMO between).
  - Auto-increment: sequential writes produce contiguous data.
  - Overflow wrap-around.
  - Write to read-only region sets error.
  - Read from write-only region sets error.
  - Polling not ready skips transaction and sets error.

**Exit criteria:** Tests pass.

### Step 6.2: INDIRECT_FIFO_DATA handler

**Files:** `interface.rs`

- Implement `handle_indirect_fifo_data_write()` that:
  - Looks up the FIFO region for the currently selected FIFO CMS.
  - Calls `region.push(data)`.
  - On `CmsError::FifoFull`, the state machine signals NACK (transport-level).
  - On `CmsError::ReadOnly`, sets protocol error.
- Implement `handle_indirect_fifo_data_read()` that:
  - Calls `region.pop(buf)`.
  - On `CmsError::FifoEmpty`, returns empty.
  - On `CmsError::WriteOnly`, sets protocol error.
- Unit tests:
  - Push data, pop it back.
  - Push until full, verify NACK/error.
  - Pop from empty, verify behavior.
  - Write to read-only FIFO sets error.
  - Read from write-only FIFO sets error.

**Exit criteria:** Tests pass.

---

## Phase 7: Vendor Command Delegation

### Step 7.1: VENDOR command handler

**Files:** `interface.rs`

- Implement `handle_vendor_write()` that delegates to
  `vendor.handle_vendor_write(data)`.
- Implement `handle_vendor_read()` that delegates to
  `vendor.handle_vendor_read(buf)`.
- If vendor command is not supported (capability bit not set), set
  `UnsupportedCommand` protocol error.
- Unit tests:
  - With a mock VendorHandler, verify write data is forwarded.
  - With a mock VendorHandler, verify read response is returned.
  - With NoopVendorHandler, verify error is returned.

**Exit criteria:** Tests pass.

---

## Phase 8: Command Dispatch and State Transitions

Wire all handlers into the main `process_command()` loop with full scope checking,
error routing, and state transitions.

### Step 8.1: Command dispatch loop

**Files:** `interface.rs`

- Implement the full `process_command()` method:
  1. Call `transport.recv_msg(timeout)`.
  2. Parse command code into `RecoveryCommand`. Unknown commands set
     `UnsupportedCommand`.
  3. Determine read vs write from transport context.
  4. Check command scope ("A" vs "R"). Recovery-only commands when
     `device_status == StatusPending` set `UnsupportedCommand`.
  5. Enforce read-only / write-only constraints.
  6. Dispatch to the appropriate handler from phases 4-7.
  7. For read commands, call `transport.send_msg()` with the serialized response.
  8. Return the `RecoveryAction` from the handler (or `None`).
- Unit tests (with mock transport):
  - Unknown command code sets protocol error, returns `None`.
  - Write to read-only command sets protocol error.
  - Recovery-only command while StatusPending sets protocol error.
  - Each command code routes to the correct handler.
  - Transport errors propagate as `Err`.

**Exit criteria:** Tests pass.

### Step 8.2: Activation flow and complete_activation

**Files:** `interface.rs`

- Implement `complete_activation()`:
  - On `success = true`: if more stages expected, increment `recovery_image_index`,
    set `recovery_status = AwaitingRecoveryImage`. Otherwise, set
    `recovery_status = RecoverySuccessful` and `device_status = RunningRecoveryImage`.
  - On `success = false`: set `recovery_status = RecoveryFailed`.
- Implement state transition from `RecoveryMode` to `RecoveryPending` when activation
  is requested via `RECOVERY_CTRL`.
- Unit tests:
  - Single-stage activation: push image, activate, complete with success. Verify
    `RecoverySuccessful` status.
  - Single-stage activation failure: verify `RecoveryFailed` status.
  - Multi-stage activation: first stage succeeds, index increments, status returns to
    `AwaitingRecoveryImage`. Second stage succeeds, `RecoverySuccessful`.
  - Activation without image selection sets error.

**Exit criteria:** Tests pass.

### Step 8.3: Device status state transitions

**Files:** `interface.rs`

- Implement methods for the integrator to drive non-command state transitions:
  - `set_device_healthy()` -- transitions to Healthy.
  - `enter_recovery(&mut self, reason: RecoveryReasonCode)` -- transitions to
    RecoveryMode with the given reason code.
  - `set_boot_failure(&mut self, reason: RecoveryReasonCode)` -- transitions to
    BootFailure.
- Ensure forced recovery (from `DEVICE_RESET` handler) sets status to RecoveryMode
  with reason `ForcedRecovery` on next reset.
- Unit tests:
  - StatusPending -> Healthy via `set_device_healthy()`.
  - Healthy -> RecoveryMode via `enter_recovery()` with correct reason code.
  - RecoveryMode -> RecoveryPending -> RecoverySuccessful full flow.
  - BootFailure status with correct reason.
  - Forced recovery sets reason code.

**Exit criteria:** Tests pass.

---

## Phase 9: Integration Tests

End-to-end tests exercising multiple commands in sequence across the full stack:
mock transport, slice-backed CMS regions, and the state machine.

### Step 9.1: Recovery flow integration test

**Files:** `tests/recovery_flow.rs`

- Full recovery sequence:
  1. Read PROT_CAP, verify capabilities.
  2. Read DEVICE_STATUS, verify StatusPending.
  3. Transition to RecoveryMode.
  4. Read DEVICE_STATUS, verify RecoveryMode + reason code.
  5. Write INDIRECT_CTRL to select CMS 0.
  6. Write INDIRECT_DATA with recovery image bytes.
  7. Write RECOVERY_CTRL to select CMS push + activate.
  8. Verify `process_command` returns `ActivateRecoveryImage`.
  9. Call `complete_activation(true)`.
  10. Read RECOVERY_STATUS, verify `RecoverySuccessful`.

**Exit criteria:** Integration test passes.

### Step 9.2: Activation flow integration test

**Files:** `tests/activation.rs`

- Multi-stage activation sequence.
- Activation failure and retry.
- Combined image selection + activation in single RECOVERY_CTRL write.
- Local c-image selection and activation.

**Exit criteria:** Integration test passes.

### Step 9.3: CMS topology integration test

**Files:** `tests/cms_topology.rs`

- Mixed indirect + FIFO regions.
- Accessing FIFO CMS via INDIRECT_CTRL returns "Unsupported Region".
- Accessing indirect CMS via INDIRECT_FIFO_CTRL returns "Unsupported Region".
- Invalid CMS index returns "Unsupported Region".
- Multiple code regions across different CMS indices.

**Exit criteria:** Integration test passes.

---

## Summary

| Phase | Steps | Focus |
|-------|-------|-------|
| 1 | 1.1 - 1.3 | CMS traits + slice-backed implementations |
| 2 | 2.1 | VendorHandler trait + config structs |
| 3 | 3.1 | State machine struct + constructor |
| 4 | 4.1 - 4.7 | Read-only command handlers |
| 5 | 5.1 - 5.4 | Write command handlers |
| 6 | 6.1 - 6.2 | Data transfer handlers |
| 7 | 7.1 | Vendor command delegation |
| 8 | 8.1 - 8.3 | Command dispatch + state transitions |
| 9 | 9.1 - 9.3 | Integration tests |

Total: 9 phases, 22 steps. Each step ends with a clean `cargo build && cargo test &&
cargo fmt --check && cargo clippy`.
