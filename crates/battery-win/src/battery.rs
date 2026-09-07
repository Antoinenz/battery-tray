//! Direct battery-driver access via device IOCTLs.
//!
//! Deliberately not WMI: every WMI query spins up `WmiPrvSE.exe` and costs tens
//! of megabytes and real CPU. These IOCTLs are what WMI wraps anyway, and they
//! additionally expose a *blocking wait*, which is what lets the app sample
//! event-driven at effectively zero idle cost.

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::DeviceIoControl;

const GUID_DEVICE_BATTERY: GUID = GUID {
    data1: 0x72631e54,
    data2: 0x78a4,
    data3: 0x11d0,
    data4: [0xbc, 0xf7, 0x00, 0xaa, 0x00, 0xb7, 0xb3, 0x2a],
};

const fn ctl_code(dev: u32, func: u32, meth: u32, acc: u32) -> u32 {
    (dev << 16) | (acc << 14) | (func << 2) | meth
}
const FILE_DEVICE_BATTERY: u32 = 0x29;
const IOCTL_BATTERY_QUERY_TAG: u32 = ctl_code(FILE_DEVICE_BATTERY, 0x10, 0, 1);
const IOCTL_BATTERY_QUERY_INFORMATION: u32 = ctl_code(FILE_DEVICE_BATTERY, 0x11, 0, 1);
const IOCTL_BATTERY_QUERY_STATUS: u32 = ctl_code(FILE_DEVICE_BATTERY, 0x13, 0, 1);

const BATTERY_CAPACITY_RELATIVE: u32 = 0x4000_0000;

#[repr(C)]
#[derive(Clone, Copy)]
struct QueryInformation {
    tag: u32,
    level: u32,
    at_rate: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInformation {
    capabilities: u32,
    technology: u8,
    reserved: [u8; 3],
    chemistry: [u8; 4],
    designed_capacity: u32,
    full_charged_capacity: u32,
    default_alert1: u32,
    default_alert2: u32,
    critical_bias: u32,
    cycle_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WaitStatus {
    tag: u32,
    timeout: u32,
    power_state: u32,
    low_capacity: u32,
    high_capacity: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct RawStatus {
    pub power_state: u32,
    pub capacity_mwh: u32,
    pub voltage_mv: u32,
    pub rate_mw: i32,
}

#[derive(Clone, Debug)]
pub struct BatteryInfo {
    pub designed_mwh: u32,
    pub full_mwh: u32,
    pub cycle_count: u32,
    /// True when the driver reports capacity in unknown relative units, in
    /// which case watts are not available and only percentages are meaningful.
    pub relative: bool,
    pub chemistry: String,
}

unsafe fn ioctl<I, O>(h: HANDLE, code: u32, inp: Option<&I>, out: &mut O) -> bool {
    let mut returned = 0u32;
    let (ip, il) = match inp {
        Some(v) => (v as *const I as *const c_void, std::mem::size_of::<I>() as u32),
        None => (null(), 0),
    };
    DeviceIoControl(
        h,
        code,
        ip,
        il,
        out as *mut O as *mut c_void,
        std::mem::size_of::<O>() as u32,
        &mut returned,
        null_mut(),
    ) != 0
}

/// An open handle to the system battery.
pub struct Battery {
    handle: HANDLE,
    tag: u32,
}

// The handle is only ever touched from the thread that owns this value.
unsafe impl Send for Battery {}

impl Drop for Battery {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
                CloseHandle(self.handle);
            }
        }
    }
}

fn device_paths() -> Vec<Vec<u16>> {
    let mut out = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(
            &GUID_DEVICE_BATTERY,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set == 0 || set == -1 {
            return out;
        }
        let mut idx = 0u32;
        loop {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), &GUID_DEVICE_BATTERY, idx, &mut ifd) == 0 {
                break;
            }
            let mut needed = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut needed, null_mut());
            if needed >= 8 {
                let mut buf = vec![0u8; needed as usize];
                let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
                (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                if SetupDiGetDeviceInterfaceDetailW(set, &ifd, detail, needed, &mut needed, null_mut())
                    != 0
                {
                    let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
                    let mut len = 0usize;
                    while *p.add(len) != 0 {
                        len += 1;
                    }
                    let mut w = std::slice::from_raw_parts(p, len).to_vec();
                    w.push(0);
                    out.push(w);
                }
            }
            idx += 1;
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

impl Battery {
    /// Open the first system battery, if the machine has one.
    pub fn open_first() -> Option<Battery> {
        for path in device_paths() {
            unsafe {
                let h = CreateFileW(
                    path.as_ptr(),
                    (GENERIC_READ | GENERIC_WRITE) as u32,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    null_mut(),
                );
                if h == INVALID_HANDLE_VALUE {
                    continue;
                }
                let mut b = Battery { handle: h, tag: 0 };
                if b.refresh_tag() {
                    return Some(b);
                }
            }
        }
        None
    }

    /// Re-read the battery tag. The tag changes when a pack is swapped, and
    /// every other IOCTL fails until it is re-fetched.
    pub fn refresh_tag(&mut self) -> bool {
        unsafe {
            let wait_ms = 0u32;
            let mut tag = 0u32;
            if ioctl(self.handle, IOCTL_BATTERY_QUERY_TAG, Some(&wait_ms), &mut tag) && tag != 0 {
                self.tag = tag;
                true
            } else {
                false
            }
        }
    }

    pub fn info(&self) -> Option<BatteryInfo> {
        unsafe {
            let q = QueryInformation { tag: self.tag, level: 0, at_rate: 0 };
            let mut raw = RawInformation::default();
            if !ioctl(self.handle, IOCTL_BATTERY_QUERY_INFORMATION, Some(&q), &mut raw) {
                return None;
            }
            Some(BatteryInfo {
                designed_mwh: raw.designed_capacity,
                full_mwh: raw.full_charged_capacity,
                cycle_count: raw.cycle_count,
                relative: raw.capabilities & BATTERY_CAPACITY_RELATIVE != 0,
                chemistry: String::from_utf8_lossy(&raw.chemistry)
                    .trim_end_matches('\0')
                    .trim()
                    .to_string(),
            })
        }
    }

    pub fn status(&self) -> Option<RawStatus> {
        unsafe {
            let w = WaitStatus { tag: self.tag, ..Default::default() };
            let mut st = RawStatus::default();
            ioctl(self.handle, IOCTL_BATTERY_QUERY_STATUS, Some(&w), &mut st).then_some(st)
        }
    }

    /// Block until the battery meaningfully changes, or `timeout_ms` elapses.
    ///
    /// The kernel returns early if the power state differs from `power_state`
    /// or capacity leaves the band around `capacity_mwh`, so plugging the
    /// charger in registers immediately rather than at the next poll tick.
    pub fn wait(
        &self,
        timeout_ms: u32,
        power_state: u32,
        capacity_mwh: u32,
        band_mwh: u32,
    ) -> Option<RawStatus> {
        unsafe {
            let w = WaitStatus {
                tag: self.tag,
                timeout: timeout_ms,
                power_state,
                low_capacity: capacity_mwh.saturating_sub(band_mwh),
                high_capacity: capacity_mwh.saturating_add(band_mwh),
            };
            let mut st = RawStatus::default();
            ioctl(self.handle, IOCTL_BATTERY_QUERY_STATUS, Some(&w), &mut st).then_some(st)
        }
    }
}
