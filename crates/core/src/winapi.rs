//! Thin RAII wrapper over a Windows device handle and `DeviceIoControl`. Every unsafe call is here.

use std::ffi::c_void;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_BEGIN, FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, ReadFile, SetFilePointerEx,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::error::{Error, Result};

pub const GENERIC_READ: u32 = 0x8000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct Device(HANDLE);

// SAFETY: a file HANDLE can be used from any thread; all access goes through `&self` Win32 calls.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateFileW and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

impl Device {
    /// Opens `path` with the given access mask (0 = query only, no data access).
    pub fn open(path: &str, access: u32, flags: u32) -> Result<Device> {
        let p = wide(path);
        // SAFETY: `p` is NUL-terminated and outlives the call; no security attributes / template file.
        let h = unsafe {
            CreateFileW(
                p.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            // SAFETY: trivial thread-local error read.
            Err(Error::Os {
                op: "CreateFile",
                code: unsafe { GetLastError() },
            })
        } else {
            Ok(Device(h))
        }
    }

    pub fn open_query(path: &str) -> Result<Device> {
        Device::open(path, 0, 0)
    }

    /// Read-only handle for sequential unbuffered reads. This is the only way the surface scan opens a device.
    pub fn open_read_only_unbuffered(path: &str) -> Result<Device> {
        Device::open(path, GENERIC_READ, FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
    }

    /// `DeviceIoControl` with separate input and output buffers; returns the number of bytes written to `output`.
    pub fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<u32> {
        let mut returned = 0u32;
        // SAFETY: both buffers are valid for their stated lengths for the duration of the call; no overlapped I/O.
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                if input.is_empty() {
                    std::ptr::null()
                } else {
                    input.as_ptr().cast::<c_void>()
                },
                input.len() as u32,
                if output.is_empty() {
                    null_mut()
                } else {
                    output.as_mut_ptr().cast::<c_void>()
                },
                output.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: trivial thread-local error read.
            Err(Error::Os {
                op: "DeviceIoControl",
                code: unsafe { GetLastError() },
            })
        } else {
            Ok(returned)
        }
    }

    /// In-place `DeviceIoControl` on one buffer (typed requests that return their answer in the same struct).
    ///
    /// # Safety
    /// `buf` must point to a live, correctly laid out request for `code` that is at least `len` bytes long.
    pub unsafe fn ioctl_inout(&self, code: u32, buf: *mut c_void, len: usize) -> Result<u32> {
        let mut returned = 0u32;
        // SAFETY: guaranteed by the caller.
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                code,
                buf,
                len as u32,
                buf,
                len as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: trivial thread-local error read.
            Err(Error::Os {
                op: "DeviceIoControl",
                code: unsafe { GetLastError() },
            })
        } else {
            Ok(returned)
        }
    }

    /// Positioned read. `buf` and `offset` must be sector aligned for unbuffered handles.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        // SAFETY: valid handle; `buf` valid for `len` bytes; synchronous handle.
        unsafe {
            if SetFilePointerEx(self.0, offset as i64, null_mut(), FILE_BEGIN) == 0 {
                return Err(Error::Os {
                    op: "SetFilePointerEx",
                    code: GetLastError(),
                });
            }
            let mut read = 0u32;
            if ReadFile(self.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read, null_mut()) == 0 {
                return Err(Error::Os {
                    op: "ReadFile",
                    code: GetLastError(),
                });
            }
            Ok(read as usize)
        }
    }
}
