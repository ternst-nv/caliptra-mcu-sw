# Optional Software OCP Secure Firmware Recovery Utility for Caliptra 2.1

This RFC describes an optional ROM integration that implements the OCP Secure Firmware Recovery v1.1 device-side protocol for Caliptra MCU platforms. The implementation provides a transport-agnostic OCP protocol engine and state machine, a `TransportDriver` trait that integrators implement for their specific transport hardware, and an `OcpImageProvider` that bridges OCP recovery into the existing ROM `ImageProvider` / `ImageProviderManager` boot flow. USB is included as the exemplar transport to demonstrate the full driver contract and enable end-to-end testing on the emulated platform. The entire feature is opt-in: platforms that do not need OCP recovery simply omit it with no impact on existing boot paths.

This functionality is included in addition to the hardware backed I3C OCP Recovery to enable Integrators who choose to include additional recovery hardware on the AXI bus.

## Scope

The following areas of the `caliptra-mcu-sw` repository are affected:

### New crate: `common/ocp/`

A `no_std`, zero-dependency (aside from `bitfield` and `zerocopy`) crate implementing the OCP Secure Firmware Recovery v1.1 device-side protocol. It defines the command set specified by OCP, a state machine which can receive and process those messages, and a `TransportDriver` trait that abstracts the underlying transport (USB, I3C, SMBus, or any other). The mechanisms are extensible to allow an integrator to determine which level of OCP support they require, as well as the ability to define vendor specific messages, etc.

### New crate: `platforms/emulator/rom/usb/`

An exemplar `TransportDriver` implementation using USB as the chosen transport, targeting the emulated `usbdev` peripheral. It demonstrates the full driver contract — including hardware initialization, USB enumeration (GET_DESCRIPTOR, SET_ADDRESS, SET_CONFIGURATION), multi-packet IN segmentation with ZLP handling, and OUT data aggregation — and serves as a reference for integrators building drivers for their own transport hardware.

### ROM integration: `rom/src/recovery/ocp.rs`

`OcpImageProvider` — wraps `RecoveryStateMachine` and implements the existing `ImageProvider` trait so OCP recovery slots into the `ImageProviderManager` provider chain alongside `FlashImageProvider` or any other source. It supports both CMS access models:

* **Indirect path**: `image_ready()` drives `process_command()` until `ActivateRecoveryImage` is returned (full image buffered in the CMS region); `next_bytes()` reads directly from the region.
* **FIFO path**: `image_ready()` blocks until the host declares the image size via `INDIRECT_FIFO_CTRL`; `next_bytes()` interleaves `process_command()` calls with `device_drain()` to stream data incrementally.

### ROM integration: `rom/src/recovery.rs`

Extended with:
* `ImageProviderManager` — manages an ordered sequence of `ImageProviderEntry`s, each pairing a provider with an `ErrorPolicy` (`Continue`, `Retry(N)`, `RetryForever`). The `load_image_with_retry()` function iterates through providers according to their policies, driving the I3C bypass recovery state machine for each.
* `ErrorPolicy` and `ImageProviderEntry` types.

### Platform wiring (emulator example): `platforms/emulator/rom/src/riscv.rs`

Demonstrates how an integrator wires the OCP recovery path using a Cargo feature gate.

### Integration tests: `tests/integration/src/test_usb_ocp_recovery.rs`

There will exist a number of integration tests, validating a variety of positive and negative flows.  This include supporting a full boot sequence (Caliptra Image, Manifest, and MCU Runtime), as well as status message requests.  For negative flows the integration test will include corrupted images, transmission errors with retries, and flows where standard boot methods like flash fail, requiring the ROM to enter OCP recovery.

### Documentation and specification impacts

* No changes to Caliptra Trademark Compliance requirements — the feature is optional.
* Caliptra documentation will be added to detail how to use this feature at the integrators discretion.  It will emphasize it's optionality, and the fact that non-integrators will pay 0 cost in either instruction or data memory if they choose not use it.
* The existing cold boot specification and ROM boot flow documentation should reference this as an optional recovery path available to integrators.

## Rationale

### Why OCP Secure Firmware Recovery in Caliptra 2.1

The OCP Secure Firmware Recovery v1.1 specification defines a standard, transport-agnostic protocol for host-initiated firmware recovery. Caliptra 2.1 platforms already support I3C-based recovery via the existing bypass recovery state machine, but the OCP spec is designed to work across multiple transports (USB, I3C, SMBus, etc.), and many system designs benefit from additional recovery paths. Providing a reusable OCP protocol implementation as an optional utility:

1. **Reduces integrator effort** — The `ocp` crate handles all protocol parsing, state management, capability negotiation, and CMS region bookkeeping. Integrators only need to implement the `TransportDriver` trait for their chosen transport hardware.

2. **Is transport-agnostic by design** — The trait-based architecture means the OCP protocol engine is not tied to any specific transport or IP block. USB is provided as the exemplar transport; integrators substitute their own driver for whatever transport their platform supports.

3. **Composes with existing boot infrastructure** — `OcpImageProvider` implements the same `ImageProvider` trait as `FlashImageProvider`, so it plugs into `ImageProviderManager` without changes to the cold boot flow. Platforms can chain providers (e.g., try flash first, fall back to OCP recovery) with configurable retry policies.

4. **Is fully optional** — Platforms that do not need OCP recovery pay zero cost: the `ocp` crate is only compiled when a platform pulls it in as a dependency, and the ROM integration is behind feature gates.

5. **Aligns with OCP ecosystem** — As Caliptra is an OCP project, providing first-class support for OCP recovery protocols strengthens the ecosystem and simplifies compliance for adopters targeting OCP-aligned platforms.

### Timing

This feature is appropriate for Caliptra 2.1 because:
* Several integrators have expressed interest in OCP recovery paths via tranports other than I3C for their 2.1 platform designs.
* Hardware support for native USB integration in Caliptra will not arrive until 2.2.

## Maintenance

The OCP recovery utility is maintained by NVIDIA. The maintenance plan includes:

* **Ongoing ownership** — The `common/ocp/` crate and ROM integration code will be maintained alongside the rest of the `caliptra-mcu-sw` repository by the core MCU firmware contributors.
* **Test coverage** — The integration test suite (`test_usb_ocp_recovery`) and unit tests (`common/ocp/tests/`) will be run as part of the standard CI pipeline. The exemplar USB transport driver and test firmware ensure the `TransportDriver` trait contract remains valid as the codebase evolves.
* **Spec tracking** — If future revisions of OCP Secure Firmware Recovery are published, the `ocp` crate will be updated to support new commands or capabilities, with backward compatibility maintained where the spec allows.
* **Integrator support** — The exemplar driver and platform wiring example serve as living documentation. Changes to the `TransportDriver` trait or `ImageProvider` interface will be accompanied by updates to the exemplar and integration tests.
