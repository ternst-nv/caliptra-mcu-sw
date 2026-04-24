# Optional USB OCP Recovery Utility for Caliptra 2.1

This RFC describes an optional ROM integration that enables Caliptra MCU platforms to support USB-based firmware recovery using the OCP Secure Firmware Recovery v1.1 protocol. The implementation provides a transport-agnostic OCP device-side state machine, a `UsbDeviceDriver` trait that integrators implement for their specific USB hardware, and an `OcpImageProvider` that bridges OCP recovery into the existing ROM `ImageProvider` / `ImageProviderManager` boot flow. The entire feature is opt-in: platforms that do not need USB recovery simply omit it with no impact on existing boot paths.

## Scope

The following areas of the `caliptra-mcu-sw` repository are affected:

### New crate: `common/ocp/`

A `no_std`, zero-dependency (aside from `bitfield` and `zerocopy`) crate implementing the OCP Secure Firmware Recovery v1.1 device-side protocol. It defines the commands required to support any transportation (I3C, USB, SMBus), a state machine which can receive and process those messages, and finally an interface for detailing how an integrator's hardware would interact with the machinerary.  The mechanisms are extensible to allow an integrator to determine which level of OCP support they require, as well as the ability to define vendor specific messages, etc.

### New crate: `platforms/emulator/rom/usb/`

An exemplar `UsbDeviceDriver` implementation (`ExamplarUsbDriver`) targeting the emulated `usbdev` peripheral. It demonstrates the full driver contract: hardware initialization, USB enumeration (GET_DESCRIPTOR, SET_ADDRESS, SET_CONFIGURATION), multi-packet IN segmentation with ZLP handling, and OUT data aggregation. This serves as a reference for integrators building drivers for real USB hardware.

### ROM integration: `rom/src/recovery/ocp.rs`

`OcpImageProvider` — wraps `RecoveryStateMachine` and implements the existing `ImageProvider` trait so OCP/USB recovery slots into the `ImageProviderManager` provider chain alongside `FlashImageProvider` or any other source. It supports both CMS access models:

* **Indirect path**: `image_ready()` drives `process_command()` until `ActivateRecoveryImage` is returned (full image buffered in the CMS region); `next_bytes()` reads directly from the region.
* **FIFO path**: `image_ready()` blocks until the host declares the image size via `INDIRECT_FIFO_CTRL`; `next_bytes()` interleaves `process_command()` calls with `device_drain()` to stream data incrementally.

### ROM integration: `rom/src/recovery.rs`

Extended with:
* `ImageProviderManager` — manages an ordered sequence of `ImageProviderEntry`s, each pairing a provider with an `ErrorPolicy` (`Continue`, `Retry(N)`, `RetryForever`). The `load_image_with_retry()` function iterates through providers according to their policies, driving the I3C bypass recovery state machine for each.
* `ErrorPolicy` and `ImageProviderEntry` types.

### Platform wiring (emulator example): `platforms/emulator/rom/src/riscv.rs`

Demonstrates how an integrator wires the OCP recovery path using a Cargo feature gate.

### Integration tests: `tests/integration/src/test_usb_ocp_recovery.rs`

There will exist a number of integration tests, validating a variety of positive and negative flows.  This include supporting a full boot sequence (Caliptra Image, Manifest, and MCU Runtime), as well as status message requests.  For negative flows the integration test will include corrupted images, transmission errors with retries, and flow where standard boot methods like flash fail, requiring the ROM to enter a USB recovery state.

### Documentation and specification impacts

* No changes to Caliptra Trademark Compliance requirements — the feature is optional.
* Caliptra documentation will be added to detail how to use this feature at the integrators discretion.  It will emphasize it's optionality, and the fact that non-integrators will pay 0 cost in either instruction or data memory if they choose not use it.
* The existing cold boot specification and ROM boot flow documentation should reference this as an optional recovery path available to integrators.

## Rationale

### Why USB OCP Recovery in Caliptra 2.1

The OCP Secure Firmware Recovery v1.1 specification defines a standard protocol for host-initiated firmware recovery over USB. Caliptra 2.1 platforms already support I3C-based recovery via the existing bypass recovery state machine, but many system designs include USB connectivity and benefit from an additional recovery transport. Providing a reusable OCP protocol implementation as an optional utility:

1. **Reduces integrator effort** — The `ocp` crate handles all protocol parsing, state management, capability negotiation, and CMS region bookkeeping. Integrators only need to implement the `UsbDeviceDriver` trait for their USB hardware.

2. **Maintains OCP transport neutrality** — The trait-based design means the protocol engine is not tied to any specific USB IP block. The exemplar driver demonstrates the contract; integrators substitute their own.

3. **Composes with existing boot infrastructure** — `OcpImageProvider` implements the same `ImageProvider` trait as `FlashImageProvider`, so it plugs into `ImageProviderManager` without changes to the cold boot flow. Platforms can chain providers (e.g., try flash first, fall back to USB OCP) with configurable retry policies.

4. **Is fully optional** — Platforms that do not need USB recovery pay zero cost: the `ocp` crate is only compiled when a platform pulls it in as a dependency, and the ROM integration is behind feature gates.

5. **Aligns with OCP ecosystem** — As Caliptra is an OCP project, providing first-class support for OCP recovery protocols strengthens the ecosystem and simplifies compliance for adopters targeting OCP-aligned platforms.

### Timing

This feature is appropriate for Caliptra 2.1 because:
* The I3C bypass recovery infrastructure and `ImageProvider` trait are already established in the 2.1 ROM.
* The USB `usbdev` peripheral is available in the 2.1 emulated subsystem, enabling full-stack testing.
* Several integrators have expressed interest in USB recovery paths for their 2.1 platform designs.
* The hardware support for USB Integration in Caliptra will not arrive until 2.2.

## Maintenance

The OCP recovery utility is maintained by NVIDIA. The maintenance plan includes:

* **Ongoing ownership** — The `common/ocp/` crate and ROM integration code will be maintained alongside the rest of the `caliptra-mcu-sw` repository by the core MCU firmware contributors.
* **Test coverage** — The integration test suite (`test_usb_ocp_recovery`) and unit tests (`common/ocp/tests/`) will be run as part of the standard CI pipeline. The exemplar driver and test firmware ensure the driver trait contract remains valid as the codebase evolves.
* **Spec tracking** — If future revisions of OCP Secure Firmware Recovery are published, the `ocp` crate will be updated to support new commands or capabilities, with backward compatibility maintained where the spec allows.
* **Integrator support** — The exemplar driver and platform wiring example serve as living documentation. Changes to the `UsbDeviceDriver` trait or `ImageProvider` interface will be accompanied by updates to the exemplar and integration tests.
