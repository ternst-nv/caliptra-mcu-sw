// Licensed under the Apache-2.0 license

//! Simple flash storage implementation using memory. Useful for testing and emulation.

use crate::{
    hil::{FlashDrvError, FlashStorage},
    rom_copy_from_slice,
};
use core::{cell::Cell, result::Result};

pub struct SimpleFlash {
    memory: Cell<&'static mut [u8]>,
}

impl SimpleFlash {
    /// Create a new SimpleFlash instance with the provided memory slice.
    pub fn new(memory: &'static mut [u8]) -> Self {
        SimpleFlash {
            memory: Cell::new(memory),
        }
    }
}

impl FlashStorage for SimpleFlash {
    /// Read from the flash storage, filling the provided buffer with data
    fn read(&self, buffer: &mut [u8], address: usize) -> Result<(), FlashDrvError> {
        let mem = self.memory.take();
        let result = match mem.get(address..address + buffer.len()) {
            Some(slice) => {
                rom_copy_from_slice(buffer, slice);
                Ok(())
            }
            _ => Err(FlashDrvError::INVAL),
        };
        self.memory.set(mem);
        result
    }

    /// Write to the flash storage with the full contents of the buffer, starting at the specified address
    fn write(&self, buffer: &[u8], address: usize) -> Result<(), FlashDrvError> {
        let mem = self.memory.take();
        let result = match mem.get_mut(address..address + buffer.len()) {
            Some(slice) => {
                rom_copy_from_slice(slice, buffer);
                Ok(())
            }
            _ => Err(FlashDrvError::INVAL),
        };
        self.memory.set(mem);
        result
    }

    /// Erase `length` bytes starting at address `address`. The address must be
    /// in the address space of the physical storage.
    fn erase(&self, address: usize, length: usize) -> Result<(), FlashDrvError> {
        let mem = self.memory.take();
        let result = match mem.get_mut(address..address + length) {
            Some(slice) => {
                slice.fill(0);
                Ok(())
            }
            _ => Err(FlashDrvError::INVAL),
        };
        self.memory.set(mem);
        result
    }

    /// Returns the size of the flash storage in bytes.
    fn capacity(&self) -> usize {
        let mem = self.memory.take();
        let len = mem.len();
        self.memory.set(mem);
        len
    }
}
