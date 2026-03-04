// Licensed under the Apache-2.0 license

//! Slice-backed [`FifoCmsRegion`](crate::cms::FifoCmsRegion) implementation.
//!
//! Provides a concrete FIFO CMS region backed by a `&mut [u8]` ring buffer suitable
//! for `no_std` environments. Implemented in Step 1.3.
