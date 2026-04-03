//! Raw FFI declarations for macOS IOKit USB.
//!
//! These map to the IOKit/usb/IOUSBLib.h interfaces used by aft-mtp-cli.
//! IOKit USB uses COM-like vtable interfaces: each "interface" is a pointer
//! to a pointer to a vtable of function pointers.

#![allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    dead_code,
    improper_ctypes
)]

use std::os::raw::{c_char, c_int, c_void};

// --- CoreFoundation types ---
pub type CFIndex = isize;
pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFUUIDRef = *const c_void;
pub type CFUUIDBytes = [u8; 16];

// --- IOKit types ---
pub type io_object_t = u32;
pub type io_service_t = io_object_t;
pub type io_iterator_t = io_object_t;
pub type mach_port_t = u32;
pub type IOReturn = c_int;
pub type kern_return_t = c_int;
pub type SInt32 = i32;
pub type UInt8 = u8;
pub type UInt16 = u16;
pub type UInt32 = u32;
#[allow(clippy::upper_case_acronyms)] // Matches COM/IOKit FFI convention.
pub type HRESULT = i32;

pub const kIOReturnSuccess: IOReturn = 0;
pub const kIOUSBPipeStalled: IOReturn = 0xe0004079u32 as IOReturn;

// IOKit master port
pub const kIOMasterPortDefault: mach_port_t = 0;

// USB class for PTP/MTP
pub const kUSBDeviceClassName: &str = "IOUSBDevice";
pub const kUSBInterfaceClassName: &str = "IOUSBInterface";

// CoreFoundation UUID constants (we'll look these up at runtime)
pub const kIOUSBDeviceUserClientTypeID_str: &str = "9dc7b780-9ec0-11d4-a54f-000a27052861";
pub const kIOUSBInterfaceUserClientTypeID_str: &str = "2d9786c6-9ef3-11d4-ad51-000a27052861";
pub const kIOCFPlugInInterfaceID_str: &str = "c244e858-109c-11d4-91d4-0050e4c6426f";
// IOUSBDeviceInterfaceID182 — includes USBDeviceSuspend for device wakeup.
pub const kIOUSBDeviceInterfaceID_str: &str = "152fc496-4891-11d5-9d52-000a27801e86";
// IOUSBInterfaceInterfaceID100 — matches our vtable struct layout.
pub const kIOUSBInterfaceInterfaceID_str: &str = "73c97ae8-9ef3-11d4-b1d0-000a27052861";

// --- IOUSBDeviceInterface vtable (IOUSBDeviceStruct182 / ID100) ---
// Layout from IOUSBLib.h: IUNKNOWN_C_GUTS + IOCFPLUGINBASE + device methods.
//
// IUNKNOWN_C_GUTS: _reserved, QueryInterface, AddRef, Release
// IOCFPLUGINBASE:  version(u16), revision(u16), Probe, Start, Stop
//
// On 64-bit, the two u16 fields pack into 4 bytes but the struct is
// pointer-aligned, so they occupy 8 bytes total (with 4 bytes padding).
#[repr(C)]
pub struct IOUSBDeviceInterface {
    // IUNKNOWN_C_GUTS
    pub _reserved: *const c_void,
    pub QueryInterface: unsafe extern "C" fn(
        *mut *mut IOUSBDeviceInterface,
        CFUUIDBytes,
        *mut *mut c_void,
    ) -> HRESULT,
    pub AddRef: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> u32,
    pub Release: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> u32,
    // IOUSBDeviceInterface methods (no IOCFPLUGINBASE — that's on the plugin, not here)
    pub CreateDeviceAsyncEventSource:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut CFRunLoopSourceRef) -> IOReturn,
    pub GetDeviceAsyncEventSource:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> CFRunLoopSourceRef,
    pub CreateDeviceAsyncPort:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut mach_port_t) -> IOReturn,
    pub GetDeviceAsyncPort: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> mach_port_t,
    pub USBDeviceOpen: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> IOReturn,
    pub USBDeviceClose: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> IOReturn,
    pub GetDeviceClass:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt8) -> IOReturn,
    pub GetDeviceSubClass:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt8) -> IOReturn,
    pub GetDeviceProtocol:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt8) -> IOReturn,
    pub GetDeviceVendor:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt16) -> IOReturn,
    pub GetDeviceProduct:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt16) -> IOReturn,
    pub GetDeviceReleaseNumber:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt16) -> IOReturn,
    pub GetDeviceAddress:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt16) -> IOReturn,
    pub GetDeviceBusPowerAvailable:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt32) -> IOReturn,
    pub GetDeviceSpeed:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt8) -> IOReturn,
    pub GetNumberOfConfigurations:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt8) -> IOReturn,
    pub GetLocationID:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt32) -> IOReturn,
    pub GetConfigurationDescriptorPtr:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, UInt8, *mut *const c_void) -> IOReturn,
    pub GetConfiguration:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut UInt8) -> IOReturn,
    pub SetConfiguration: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, UInt8) -> IOReturn,
    pub GetBusFrameNumber:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut u64, *mut u64) -> IOReturn,
    pub ResetDevice: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> IOReturn,
    pub DeviceRequest:
        unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, *mut IOUSBDevRequest) -> IOReturn,
    pub DeviceRequestAsync: *const c_void,
    pub CreateInterfaceIterator: unsafe extern "C" fn(
        *mut *mut IOUSBDeviceInterface,
        *const IOUSBFindInterfaceRequest,
        *mut io_iterator_t,
    ) -> IOReturn,
    // IOUSBDeviceInterface182 extension methods:
    pub USBDeviceOpenSeize: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface) -> IOReturn,
    pub DeviceRequestTO: *const c_void,
    pub DeviceRequestAsyncTO: *const c_void,
    pub USBDeviceSuspend: unsafe extern "C" fn(*mut *mut IOUSBDeviceInterface, u8) -> IOReturn,
}

#[repr(C)]
pub struct IOUSBDevRequest {
    pub bmRequestType: UInt8,
    pub bRequest: UInt8,
    pub wValue: UInt16,
    pub wIndex: UInt16,
    pub wLength: UInt16,
    pub pData: *mut c_void,
    pub wLenDone: UInt32,
}

#[repr(C)]
pub struct IOUSBFindInterfaceRequest {
    pub bInterfaceClass: UInt16,
    pub bInterfaceSubClass: UInt16,
    pub bInterfaceProtocol: UInt16,
    pub bAlternateSetting: UInt16,
}

// --- IOUSBInterfaceInterface vtable ---
#[repr(C)]
pub struct IOUSBInterfaceInterface {
    // IUnknown
    pub _reserved: *const c_void,
    pub QueryInterface: unsafe extern "C" fn(
        *mut *mut IOUSBInterfaceInterface,
        CFUUIDBytes,
        *mut *mut c_void,
    ) -> HRESULT,
    pub AddRef: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface) -> u32,
    pub Release: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface) -> u32,
    // IOUSBInterfaceInterface methods (no IOCFPLUGINBASE)
    pub CreateInterfaceAsyncEventSource: unsafe extern "C" fn(
        *mut *mut IOUSBInterfaceInterface,
        *mut CFRunLoopSourceRef,
    ) -> IOReturn,
    pub GetInterfaceAsyncEventSource:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface) -> CFRunLoopSourceRef,
    pub CreateInterfaceAsyncPort:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut mach_port_t) -> IOReturn,
    pub GetInterfaceAsyncPort:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface) -> mach_port_t,
    pub USBInterfaceOpen: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface) -> IOReturn,
    pub USBInterfaceClose: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface) -> IOReturn,
    pub GetInterfaceClass:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt8) -> IOReturn,
    pub GetInterfaceSubClass:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt8) -> IOReturn,
    pub GetInterfaceProtocol:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt8) -> IOReturn,
    pub GetDeviceVendor:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt16) -> IOReturn,
    pub GetDeviceProduct:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt16) -> IOReturn,
    pub GetDeviceReleaseNumber:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt16) -> IOReturn,
    pub GetConfigurationValue:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt8) -> IOReturn,
    pub GetInterfaceNumber:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt8) -> IOReturn,
    pub GetAlternateSetting:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt8) -> IOReturn,
    pub GetNumEndpoints:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt8) -> IOReturn,
    pub GetLocationID:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut UInt32) -> IOReturn,
    pub GetDevice:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut io_service_t) -> IOReturn,
    pub SetAlternateInterface:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, UInt8) -> IOReturn,
    pub GetBusFrameNumber:
        unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, *mut u64, *mut u64) -> IOReturn,
    pub ControlRequest: unsafe extern "C" fn(
        *mut *mut IOUSBInterfaceInterface,
        UInt8,
        *mut IOUSBDevRequest,
    ) -> IOReturn,
    pub ControlRequestAsync: *const c_void,
    pub GetPipeProperties: unsafe extern "C" fn(
        *mut *mut IOUSBInterfaceInterface,
        UInt8,       // pipe ref (1-based)
        *mut UInt8,  // direction
        *mut UInt8,  // number
        *mut UInt8,  // transfer type
        *mut UInt16, // max packet size
        *mut UInt8,  // interval
    ) -> IOReturn,
    pub GetPipeStatus: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, UInt8) -> IOReturn,
    pub AbortPipe: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, UInt8) -> IOReturn,
    pub ResetPipe: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, UInt8) -> IOReturn,
    pub ClearPipeStall: unsafe extern "C" fn(*mut *mut IOUSBInterfaceInterface, UInt8) -> IOReturn,
    pub ReadPipe: unsafe extern "C" fn(
        *mut *mut IOUSBInterfaceInterface,
        UInt8,       // pipe ref
        *mut c_void, // buffer
        *mut UInt32, // size (in/out)
    ) -> IOReturn,
    pub WritePipe: unsafe extern "C" fn(
        *mut *mut IOUSBInterfaceInterface,
        UInt8,         // pipe ref
        *const c_void, // buffer
        UInt32,        // size
    ) -> IOReturn,
}

// --- IOCFPlugInInterface ---
#[repr(C)]
pub struct IOCFPlugInInterface {
    pub _reserved: *const c_void,
    pub QueryInterface: unsafe extern "C" fn(
        *mut *mut IOCFPlugInInterface,
        CFUUIDBytes,
        *mut *mut c_void,
    ) -> HRESULT,
    pub AddRef: unsafe extern "C" fn(*mut *mut IOCFPlugInInterface) -> u32,
    pub Release: unsafe extern "C" fn(*mut *mut IOCFPlugInInterface) -> u32,
    // Additional fields we don't need
}

// --- IOKit framework functions ---
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    pub fn IOServiceGetMatchingServices(
        masterPort: mach_port_t,
        matching: CFDictionaryRef,
        existing: *mut io_iterator_t,
    ) -> kern_return_t;
    pub fn IOIteratorNext(iterator: io_iterator_t) -> io_object_t;
    pub fn IOObjectRelease(object: io_object_t) -> kern_return_t;
    pub fn IOCreatePlugInInterfaceForService(
        service: io_service_t,
        pluginType: CFUUIDRef,
        interfaceType: CFUUIDRef,
        theInterface: *mut *mut *mut IOCFPlugInInterface,
        theScore: *mut SInt32,
    ) -> kern_return_t;
}

// --- CoreFoundation functions ---
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFUUIDCreateFromString(alloc: CFAllocatorRef, uuidStr: CFStringRef) -> CFUUIDRef;
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
    pub fn CFRelease(cf: CFTypeRef);

    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;

    pub fn CFDictionarySetValue(
        theDict: CFMutableDictionaryRef,
        key: *const c_void,
        value: *const c_void,
    );
    pub fn CFNumberCreate(
        allocator: CFAllocatorRef,
        theType: CFIndex,
        valuePtr: *const c_void,
    ) -> CFNumberRef;
}

pub const kCFStringEncodingUTF8: u32 = 0x08000100;
pub const kCFNumberSInt32Type: CFIndex = 3;

// USB endpoint directions
pub const kUSBIn: u8 = 1;
pub const kUSBOut: u8 = 0;
pub const kUSBBulk: u8 = 2;

/// Helper to create a CFString from a Rust &str.
///
/// # Safety
/// Calls CoreFoundation FFI. The returned CFStringRef must be released with CFRelease.
pub unsafe fn cf_string(s: &str) -> CFStringRef {
    let c_str = std::ffi::CString::new(s).unwrap();
    CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), kCFStringEncodingUTF8)
}

/// Helper to create a CFUUID from a string.
///
/// # Safety
/// Calls CoreFoundation FFI. The returned CFUUIDRef must be released with CFRelease.
pub unsafe fn cf_uuid(s: &str) -> CFUUIDRef {
    let cf_str = cf_string(s);
    let uuid = CFUUIDCreateFromString(std::ptr::null(), cf_str);
    CFRelease(cf_str as CFTypeRef);
    uuid
}

/// Helper to get UUID bytes from a string UUID.
///
/// # Safety
/// Calls CoreFoundation FFI to create and query a UUID.
pub unsafe fn uuid_bytes(s: &str) -> CFUUIDBytes {
    let uuid = cf_uuid(s);
    let bytes = CFUUIDGetUUIDBytes(uuid);
    CFRelease(uuid as CFTypeRef);
    bytes
}
