#![allow(dead_code, non_snake_case, clippy::too_many_arguments)]

use std::ffi::{c_char, c_void};

use windows_sys::core::{GUID, HRESULT, PWSTR};

pub type AsioBool = i32;
pub type AsioErrorCode = i32;

pub const ASIO_TRUE: AsioBool = 1;
pub const ASIO_FALSE: AsioBool = 0;

pub const ASE_OK: AsioErrorCode = 0;
pub const ASE_SUCCESS: AsioErrorCode = 0x3f48_47a0;
pub const ASE_NOT_PRESENT: AsioErrorCode = -1000;
pub const ASE_HWMALFUNCTION: AsioErrorCode = -999;
pub const ASE_INVALID_PARAMETER: AsioErrorCode = -998;
pub const ASE_INVALID_MODE: AsioErrorCode = -997;
pub const ASE_SP_NOT_ADVANCING: AsioErrorCode = -996;
pub const ASE_NO_CLOCK: AsioErrorCode = -995;
pub const ASE_NO_MEMORY: AsioErrorCode = -994;

// asioMessage selectors
pub const K_ASIO_SELECTOR_SUPPORTED: i32 = 1;
pub const K_ASIO_ENGINE_VERSION: i32 = 2;
pub const K_ASIO_RESET_REQUEST: i32 = 3;
pub const K_ASIO_BUFFER_SIZE_CHANGE: i32 = 4;
pub const K_ASIO_RESYNC_REQUEST: i32 = 5;
pub const K_ASIO_LATENCIES_CHANGED: i32 = 6;
pub const K_ASIO_SUPPORTS_TIME_INFO: i32 = 7;
pub const K_ASIO_SUPPORTS_TIME_CODE: i32 = 8;

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
pub struct AsioSamples {
    pub hi: u32,
    pub lo: u32,
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
pub struct AsioTimeStamp {
    pub hi: u32,
    pub lo: u32,
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioClockSource {
    pub index: i32,
    pub associated_channel: i32,
    pub associated_group: i32,
    pub is_current_source: AsioBool,
    pub name: [c_char; 32],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioChannelInfo {
    pub channel: i32,
    pub is_input: AsioBool,
    pub is_active: AsioBool,
    pub channel_group: i32,
    pub sample_type: i32,
    pub name: [c_char; 32],
}

impl Default for AsioChannelInfo {
    fn default() -> Self {
        Self {
            channel: 0,
            is_input: ASIO_FALSE,
            is_active: ASIO_FALSE,
            channel_group: 0,
            sample_type: 0,
            name: [0; 32],
        }
    }
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioBufferInfo {
    pub is_input: AsioBool,
    pub channel_num: i32,
    pub buffers: [*mut c_void; 2],
}

impl Default for AsioBufferInfo {
    fn default() -> Self {
        Self {
            is_input: ASIO_FALSE,
            channel_num: 0,
            buffers: [std::ptr::null_mut(), std::ptr::null_mut()],
        }
    }
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
pub struct AsioTimeInfo {
    pub speed: f64,
    pub system_time: AsioTimeStamp,
    pub sample_position: AsioSamples,
    pub sample_rate: f64,
    pub flags: u32,
    pub reserved: [u8; 12],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioTimeCode {
    pub speed: f64,
    pub time_code_samples: AsioSamples,
    pub flags: u32,
    pub future: [u8; 64],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct AsioTime {
    pub reserved: [i32; 4],
    pub time_info: AsioTimeInfo,
    pub time_code: AsioTimeCode,
}

#[repr(C, packed(4))]
pub struct AsioCallbacks {
    pub buffer_switch:
        Option<unsafe extern "C" fn(double_buffer_index: i32, direct_process: AsioBool)>,
    pub sample_rate_did_change: Option<unsafe extern "C" fn(s_rate: f64)>,
    pub asio_message: Option<
        unsafe extern "C" fn(selector: i32, value: i32, message: *mut c_void, opt: *mut f64) -> i32,
    >,
    pub buffer_switch_time_info: Option<
        unsafe extern "C" fn(
            params: *mut AsioTime,
            double_buffer_index: i32,
            direct_process: AsioBool,
        ) -> *mut AsioTime,
    >,
}

#[repr(C)]
pub struct IAsio {
    pub vtbl: *const IAsioVtbl,
}

#[repr(C)]
pub struct IAsioVtbl {
    pub query_interface: unsafe extern "system" fn(
        this: *mut IAsio,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT,
    pub add_ref: unsafe extern "system" fn(this: *mut IAsio) -> u32,
    pub release: unsafe extern "system" fn(this: *mut IAsio) -> u32,
    pub init: unsafe extern "system" fn(this: *mut IAsio, sys_handle: *mut c_void) -> AsioBool,
    pub get_driver_name: unsafe extern "system" fn(this: *mut IAsio, name: *mut c_char),
    pub get_driver_version: unsafe extern "system" fn(this: *mut IAsio) -> i32,
    pub get_error_message: unsafe extern "system" fn(this: *mut IAsio, string: *mut c_char),
    pub start: unsafe extern "system" fn(this: *mut IAsio) -> AsioErrorCode,
    pub stop: unsafe extern "system" fn(this: *mut IAsio) -> AsioErrorCode,
    pub get_channels: unsafe extern "system" fn(
        this: *mut IAsio,
        num_input_channels: *mut i32,
        num_output_channels: *mut i32,
    ) -> AsioErrorCode,
    pub get_latencies: unsafe extern "system" fn(
        this: *mut IAsio,
        input_latency: *mut i32,
        output_latency: *mut i32,
    ) -> AsioErrorCode,
    pub get_buffer_size: unsafe extern "system" fn(
        this: *mut IAsio,
        min_size: *mut i32,
        max_size: *mut i32,
        preferred_size: *mut i32,
        granularity: *mut i32,
    ) -> AsioErrorCode,
    pub can_sample_rate:
        unsafe extern "system" fn(this: *mut IAsio, sample_rate: f64) -> AsioErrorCode,
    pub get_sample_rate:
        unsafe extern "system" fn(this: *mut IAsio, sample_rate: *mut f64) -> AsioErrorCode,
    pub set_sample_rate:
        unsafe extern "system" fn(this: *mut IAsio, sample_rate: f64) -> AsioErrorCode,
    pub get_clock_sources: unsafe extern "system" fn(
        this: *mut IAsio,
        clocks: *mut AsioClockSource,
        num_sources: *mut i32,
    ) -> AsioErrorCode,
    pub set_clock_source:
        unsafe extern "system" fn(this: *mut IAsio, reference: i32) -> AsioErrorCode,
    pub get_sample_position: unsafe extern "system" fn(
        this: *mut IAsio,
        s_pos: *mut AsioSamples,
        t_stamp: *mut AsioTimeStamp,
    ) -> AsioErrorCode,
    pub get_channel_info:
        unsafe extern "system" fn(this: *mut IAsio, info: *mut AsioChannelInfo) -> AsioErrorCode,
    pub create_buffers: unsafe extern "system" fn(
        this: *mut IAsio,
        buffer_infos: *mut AsioBufferInfo,
        num_channels: i32,
        buffer_size: i32,
        callbacks: *mut AsioCallbacks,
    ) -> AsioErrorCode,
    pub dispose_buffers: unsafe extern "system" fn(this: *mut IAsio) -> AsioErrorCode,
    pub control_panel: unsafe extern "system" fn(this: *mut IAsio) -> AsioErrorCode,
    pub future: unsafe extern "system" fn(
        this: *mut IAsio,
        selector: i32,
        opt: *mut c_void,
    ) -> AsioErrorCode,
    pub output_ready: unsafe extern "system" fn(this: *mut IAsio) -> AsioErrorCode,
}

pub const CLSID_MM_DEVICE_ENUMERATOR: GUID =
    GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);
pub const IID_IMM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);

#[repr(C)]
pub struct IMMDeviceEnumerator {
    pub vtbl: *const IMMDeviceEnumeratorVtbl,
}

#[repr(C)]
pub struct IMMDeviceEnumeratorVtbl {
    pub query_interface: unsafe extern "system" fn(
        this: *mut IMMDeviceEnumerator,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT,
    pub add_ref: unsafe extern "system" fn(this: *mut IMMDeviceEnumerator) -> u32,
    pub release: unsafe extern "system" fn(this: *mut IMMDeviceEnumerator) -> u32,
    pub enum_audio_endpoints: unsafe extern "system" fn(
        this: *mut IMMDeviceEnumerator,
        data_flow: i32,
        state_mask: u32,
        devices: *mut *mut c_void,
    ) -> HRESULT,
    pub get_default_audio_endpoint: unsafe extern "system" fn(
        this: *mut IMMDeviceEnumerator,
        data_flow: i32,
        role: i32,
        endpoint: *mut *mut IMMDevice,
    ) -> HRESULT,
}

#[repr(C)]
pub struct IMMDevice {
    pub vtbl: *const IMMDeviceVtbl,
}

#[repr(C)]
pub struct IMMDeviceVtbl {
    pub query_interface: unsafe extern "system" fn(
        this: *mut IMMDevice,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT,
    pub add_ref: unsafe extern "system" fn(this: *mut IMMDevice) -> u32,
    pub release: unsafe extern "system" fn(this: *mut IMMDevice) -> u32,
    pub activate: unsafe extern "system" fn(
        this: *mut IMMDevice,
        iid: *const GUID,
        cls_ctx: u32,
        activation_params: *mut c_void,
        interface: *mut *mut c_void,
    ) -> HRESULT,
    pub open_property_store: unsafe extern "system" fn(
        this: *mut IMMDevice,
        stgm_access: u32,
        properties: *mut *mut c_void,
    ) -> HRESULT,
    pub get_id: unsafe extern "system" fn(this: *mut IMMDevice, id: *mut PWSTR) -> HRESULT,
}
