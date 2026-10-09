//! NVIDIA discovery uses the driver supplied by Windows, never a toolkit or nvidia-smi.
use std::ffi::{c_char, c_void};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub index: i32,
    pub name: String,
    pub total: u64,
    pub free: u64,
}
impl Gpu {
    pub fn label(&self) -> String {
        format!(
            "{} ({:.1} GiB VRAM)",
            self.name,
            self.total as f64 / 1073741824.0
        )
    }
}
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub device: Option<Gpu>,
    pub reason: String,
}
#[cfg(windows)]
pub fn discover() -> Discovery {
    // System32 restricts loading to the installed NVIDIA driver.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }
    type Init = unsafe extern "system" fn(u32) -> i32;
    type Count = unsafe extern "system" fn(*mut i32) -> i32;
    type Name = unsafe extern "system" fn(*mut c_char, i32, i32) -> i32;
    type Total = unsafe extern "system" fn(*mut usize, i32) -> i32;
    type Create = unsafe extern "system" fn(*mut *mut c_void, u32, i32) -> i32;
    type Destroy = unsafe extern "system" fn(*mut c_void) -> i32;
    type Memory = unsafe extern "system" fn(*mut usize, *mut usize) -> i32;
    unsafe {
        let name: Vec<u16> = "nvcuda.dll\0".encode_utf16().collect();
        let module = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), 0x800);
        if module.is_null() {
            return Discovery {
                device: None,
                reason: "No NVIDIA CUDA driver found".into(),
            };
        }
        let result = (|| -> Option<Gpu> {
            macro_rules! symbol {
                ($name:literal,$ty:ty) => {{
                    let ptr = GetProcAddress(module, concat!($name, "\0").as_ptr().cast());
                    if ptr.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $ty>(ptr)
                }};
            }
            let init = symbol!("cuInit", Init);
            let count = symbol!("cuDeviceGetCount", Count);
            let name = symbol!("cuDeviceGetName", Name);
            let total = symbol!("cuDeviceTotalMem_v2", Total);
            let create = symbol!("cuCtxCreate_v2", Create);
            let destroy = symbol!("cuCtxDestroy_v2", Destroy);
            let memory = symbol!("cuMemGetInfo_v2", Memory);
            if init(0) != 0 {
                return None;
            }
            let mut n = 0;
            if count(&mut n) != 0 {
                return None;
            }
            let mut best: Option<Gpu> = None;
            for index in 0..n {
                let mut context = std::ptr::null_mut();
                if create(&mut context, 0, index) != 0 {
                    continue;
                }
                let mut free = 0;
                let mut available = 0;
                let mut bytes = 0;
                let mut label = [0i8; 256];
                let ok = memory(&mut free, &mut available) == 0
                    && total(&mut bytes, index) == 0
                    && name(label.as_mut_ptr(), 256, index) == 0;
                destroy(context);
                if ok {
                    let gpu = Gpu {
                        index,
                        name: std::ffi::CStr::from_ptr(label.as_ptr())
                            .to_string_lossy()
                            .into_owned(),
                        total: bytes as u64,
                        free: free as u64,
                    };
                    if best.as_ref().is_none_or(|b| gpu.free > b.free) {
                        best = Some(gpu);
                    }
                }
            }
            best
        })();
        FreeLibrary(module);
        Discovery {
            reason: if result.is_some() {
                String::new()
            } else {
                "NVIDIA driver could not initialize a CUDA device".into()
            },
            device: result,
        }
    }
}
#[cfg(not(windows))]
pub fn discover() -> Discovery {
    Discovery {
        device: None,
        reason: "NVIDIA support requires Windows".into(),
    }
}
