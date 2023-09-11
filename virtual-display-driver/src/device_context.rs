use std::{mem, sync::atomic::Ordering};

use wdf_umdf::{
    IddCxAdapterInitAsync, IddCxMonitorArrival, IddCxMonitorCreate, IntoHelper,
    WDF_DECLARE_CONTEXT_TYPE,
};
use wdf_umdf_sys::{
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, IDARG_IN_ADAPTER_INIT, IDARG_IN_MONITORCREATE,
    IDARG_OUT_ADAPTER_INIT, IDARG_OUT_MONITORARRIVAL, IDARG_OUT_MONITORCREATE, IDDCX_ADAPTER,
    IDDCX_ADAPTER_CAPS, IDDCX_ENDPOINT_DIAGNOSTIC_INFO, IDDCX_ENDPOINT_VERSION,
    IDDCX_FEATURE_IMPLEMENTATION, IDDCX_MONITOR, IDDCX_MONITOR_DESCRIPTION,
    IDDCX_MONITOR_DESCRIPTION_TYPE, IDDCX_MONITOR_INFO, IDDCX_TRANSMISSION_TYPE, NTSTATUS,
    WDFDEVICE, WDFOBJECT, WDF_OBJECT_ATTRIBUTES,
};
use widestring::u16cstr;
use windows::core::GUID;

use crate::callbacks::MONITOR_COUNT;

// Taken from
// https://github.com/ge9/IddSampleDriver/blob/fe98ccff703b5c1e578a0d627aeac2fa77ac58e2/IddSampleDriver/Driver.cpp#L403
static MONITOR_EDID: &[u8] = &[
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x31, 0xD8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x05, 0x16, 0x01, 0x03, 0x6D, 0x32, 0x1C, 0x78, 0xEA, 0x5E, 0xC0, 0xA4, 0x59, 0x4A, 0x98, 0x25,
    0x20, 0x50, 0x54, 0x00, 0x00, 0x00, 0xD1, 0xC0, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x3A, 0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C,
    0x45, 0x00, 0xF4, 0x19, 0x11, 0x00, 0x00, 0x1E, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x4C, 0x69, 0x6E,
    0x75, 0x78, 0x20, 0x23, 0x30, 0x0A, 0x20, 0x20, 0x20, 0x20, 0x00, 0x00, 0x00, 0xFD, 0x00, 0x3B,
    0x3D, 0x42, 0x44, 0x0F, 0x00, 0x0A, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x00, 0x00, 0x00, 0xFC,
    0x00, 0x4C, 0x69, 0x6E, 0x75, 0x78, 0x20, 0x46, 0x48, 0x44, 0x0A, 0x20, 0x20, 0x20, 0x00, 0x05,
];

// Maximum amount of monitors that can be connected
pub const MAX_MONITORS: u8 = 10;

pub struct DeviceContext {
    pub device: WDFDEVICE,
    adapter: Option<IDDCX_ADAPTER>,
    monitors: Vec<IDDCX_MONITOR>,
}

WDF_DECLARE_CONTEXT_TYPE!(pub DeviceContext);

// SAFETY: Raw ptr is managed by external library
unsafe impl Sync for DeviceContext {}

impl DeviceContext {
    pub fn new(device: WDFDEVICE) -> Self {
        Self {
            device,
            adapter: None,
            monitors: Vec::new(),
        }
    }

    pub fn init_adapter(&mut self) -> NTSTATUS {
        let version = IDDCX_ENDPOINT_VERSION {
            Size: mem::size_of::<IDDCX_ENDPOINT_VERSION>() as u32,
            MajorVer: env!("CARGO_PKG_VERSION_MAJOR").parse::<u32>().unwrap(),
            MinorVer: concat!(
                env!("CARGO_PKG_VERSION_MINOR"),
                env!("CARGO_PKG_VERSION_PATCH")
            )
            .parse::<u32>()
            .unwrap(),
            ..Default::default()
        };

        let adapter_caps = IDDCX_ADAPTER_CAPS {
            Size: mem::size_of::<IDDCX_ADAPTER_CAPS>() as u32,
            MaxMonitorsSupported: MAX_MONITORS as u32,

            EndPointDiagnostics: IDDCX_ENDPOINT_DIAGNOSTIC_INFO {
                Size: mem::size_of::<IDDCX_ENDPOINT_DIAGNOSTIC_INFO>() as u32,
                GammaSupport: IDDCX_FEATURE_IMPLEMENTATION::IDDCX_FEATURE_IMPLEMENTATION_NONE,
                TransmissionType: IDDCX_TRANSMISSION_TYPE::IDDCX_TRANSMISSION_TYPE_WIRED_OTHER,

                pEndPointFriendlyName: u16cstr!("Virtual Display").as_ptr(),
                pEndPointManufacturerName: u16cstr!("Cherry Tech").as_ptr(),
                pEndPointModelName: u16cstr!("VirtuDisplay Pro").as_ptr(),

                pFirmwareVersion: &version as *const _ as *mut _,
                pHardwareVersion: &version as *const _ as *mut _,
            },

            ..Default::default()
        };

        let attr = WDF_OBJECT_ATTRIBUTES::init_context_type(unsafe { Self::get_type_info() });

        let adapter_init = IDARG_IN_ADAPTER_INIT {
            // this is WdfDevice because that's what we set last
            WdfDevice: self.device,
            pCaps: &adapter_caps as *const _ as *mut _,
            ObjectAttributes: &attr as *const _ as *mut _,
        };

        let mut adapter_init_out = IDARG_OUT_ADAPTER_INIT::default();
        let mut status =
            unsafe { IddCxAdapterInitAsync(&adapter_init, &mut adapter_init_out) }.into_status();

        if status.is_success() {
            self.adapter = Some(adapter_init_out.AdapterObject);

            status = unsafe { self.clone_into(adapter_init_out.AdapterObject as WDFOBJECT) }
                .into_status();
        }

        status
    }

    pub fn finish_init(&mut self) -> NTSTATUS {
        let mut status = NTSTATUS::STATUS_SUCCESS;

        for i in 0..MONITOR_COUNT.load(Ordering::Relaxed) {
            status = self.create_monitor(i);
            if !status.is_success() {
                break;
            }
        }

        status
    }

    fn create_monitor(&mut self, index: u32) -> NTSTATUS {
        let mut attr =
            WDF_OBJECT_ATTRIBUTES::init_context_type(unsafe { DeviceContext::get_type_info() });

        let mut monitor_info = IDDCX_MONITOR_INFO {
            Size: mem::size_of::<IDDCX_MONITOR_INFO>() as u32,
            // SAFETY: windows-rs + generated _GUID types are same size, with same fields, and repr C
            // see: https://microsoft.github.io/windows-docs-rs/doc/windows/core/struct.GUID.html
            // and: wmdf_umdf_sys::_GUID
            MonitorContainerId: unsafe { mem::transmute(GUID::new().unwrap()) },
            MonitorType:
                DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI,

            ConnectorIndex: index,
            MonitorDescription: IDDCX_MONITOR_DESCRIPTION {
                Size: mem::size_of::<IDDCX_MONITOR_DESCRIPTION>() as u32,
                Type: IDDCX_MONITOR_DESCRIPTION_TYPE::IDDCX_MONITOR_DESCRIPTION_TYPE_EDID,
                DataSize: MONITOR_EDID.len() as u32,
                pData: MONITOR_EDID.as_ptr() as *const _ as *mut _,
            },
        };

        let monitor_create = IDARG_IN_MONITORCREATE {
            ObjectAttributes: &mut attr,
            pMonitorInfo: &mut monitor_info,
        };

        let mut monitor_create_out = IDARG_OUT_MONITORCREATE::default();
        let mut status = unsafe {
            IddCxMonitorCreate(
                self.adapter.unwrap(),
                &monitor_create,
                &mut monitor_create_out,
            )
        }
        .into_status();

        if status.is_success() {
            self.monitors.push(monitor_create_out.MonitorObject);

            unsafe {
                status = self
                    .clone_into(monitor_create_out.MonitorObject as *mut _)
                    .into_status();
            }

            // tell os monitor is plugged in
            if status.is_success() {
                let mut arrival_out = IDARG_OUT_MONITORARRIVAL::default();

                status = unsafe {
                    IddCxMonitorArrival(monitor_create_out.MonitorObject, &mut arrival_out)
                        .into_status()
                };
            }
        }

        status
    }

    pub fn assign_swap_chain() {}

    pub fn unassign_swap_chain() {}
}
