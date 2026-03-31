// Licensed under the Apache-2.0 license

//! Length-checked slice copying for `no_std` environments.
//!
//! Wraps [`copy_from_slice`](slice::copy_from_slice) with a length check that
//! returns an error instead of panicking when the slices differ in length.

#![no_std]

/// Copies all elements from `src` into `dest`, returning an error if the slices
/// are not the same length.
///
/// Unlike [`copy_from_slice`](slice::copy_from_slice), this function will never
/// panic. When the lengths match, the contents of `src` are copied into `dest`
/// and `Ok(())` is returned. When they differ, `dest` is left unchanged and
/// `Err(())` is returned.
///
/// Since the error length check is so transparent to the compiler in this
/// formulation the now redundant panic length check will be removed from the
/// compiler, allowing resulting binaries to be no-panic.
///
/// # Errors
///
/// Returns `Err(())` if `dest.len() != src.len()`.
#[inline(always)]
#[allow(clippy::result_unit_err)]
pub fn checked_copy_from_slice<T>(dest: &mut [T], src: &[T]) -> Result<(), ()>
where
    T: Copy,
{
    if dest.len() != src.len() {
        return Err(());
    }

    dest.copy_from_slice(src);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_length_copies_successfully() {
        let src = [1u8, 2, 3, 4];
        let mut dest = [0u8; 4];
        assert_eq!(checked_copy_from_slice(&mut dest, &src), Ok(()));
        assert_eq!(dest, src);
    }

    #[test]
    fn mismatched_length_returns_err() {
        let src = [1u8, 2, 3];
        let mut dest = [0u8; 5];
        assert_eq!(checked_copy_from_slice(&mut dest, &src), Err(()));
        assert_eq!(dest, [0u8; 5]);
    }
}
