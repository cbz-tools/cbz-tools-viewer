use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StorageMedium {
    Hdd,
    Ssd,
    Unknown,
}

pub(super) fn detect_storage_medium_cached(path: &Path) -> StorageMedium {
    // デバイス問い合わせと WMI は高コストなので volume root ごとに結果を共有する。
    // lock が壊れた場合は worker を止めず Unknown として安全側へ倒す。
    static CACHE: OnceLock<Mutex<HashMap<String, StorageMedium>>> = OnceLock::new();
    let key = storage_root_key(path);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = match cache.lock() {
            Ok(g) => g,
            Err(_) => return StorageMedium::Unknown,
        };
        if let Some(medium) = guard.get(&key) {
            return *medium;
        }
    }
    let medium = detect_storage_medium(path);
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return StorageMedium::Unknown,
    };
    guard.insert(key.clone(), medium);
    let medium = match medium {
        StorageMedium::Hdd => "hdd",
        StorageMedium::Ssd => "ssd",
        StorageMedium::Unknown => "unknown",
    };
    log::debug!(
        "[thumb-worker] storage_medium root={} medium={}",
        key,
        medium
    );
    guard.get(&key).copied().unwrap_or(StorageMedium::Unknown)
}

fn storage_root_key(path: &Path) -> String {
    path.components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown-root>".to_owned())
}

#[cfg(windows)]
fn detect_storage_medium(path: &Path) -> StorageMedium {
    let Some(root) = drive_root(path) else {
        return StorageMedium::Unknown;
    };
    if let Some(medium) = detect_storage_medium_by_ioctl(path) {
        return medium;
    }
    detect_storage_medium_by_wmi(root).unwrap_or(StorageMedium::Unknown)
}

#[cfg(windows)]
fn detect_storage_medium_by_ioctl(path: &Path) -> Option<StorageMedium> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, GetDriveTypeW, GetVolumePathNameW, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
        StorageDeviceSeekPenaltyProperty,
    };

    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D1400;
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    const DRIVE_FIXED: u32 = 3;

    let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide_path.push(0);

    let mut volume_root = [0u16; 260];
    // SAFETY:
    // 入出力バッファは固定長配列でこの呼び出し中生存し、失敗時は `None` へ落とす。
    let ok = unsafe {
        GetVolumePathNameW(
            wide_path.as_ptr(),
            volume_root.as_mut_ptr(),
            volume_root.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }

    // SAFETY: `volume_root` は `GetVolumePathNameW` 成功で NUL 終端済み。
    let drive_type = unsafe { GetDriveTypeW(volume_root.as_ptr()) };
    if drive_type != DRIVE_FIXED {
        return None;
    }

    // SAFETY:
    // `volume_root` は NUL 終端済みで、成功時 handle はこの関数末尾で必ず `CloseHandle` する。
    let handle = unsafe {
        CreateFileW(
            volume_root.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut desc = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
    let mut returned = 0u32;
    // SAFETY:
    // query / desc / returned はすべて有効な入出力バッファで、サイズも構造体サイズを渡す。
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&mut query as *mut STORAGE_PROPERTY_QUERY).cast(),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            (&mut desc as *mut DEVICE_SEEK_PENALTY_DESCRIPTOR).cast(),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: `handle` は `CreateFileW` 成功値で、ここで 1 回だけ close する。
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 || returned < std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32 {
        return None;
    }
    if desc.IncursSeekPenalty {
        Some(StorageMedium::Hdd)
    } else {
        Some(StorageMedium::Ssd)
    }
}

#[cfg(not(windows))]
fn detect_storage_medium(_path: &Path) -> StorageMedium {
    StorageMedium::Unknown
}

#[cfg(windows)]
fn detect_storage_medium_by_wmi(root: &str) -> Option<StorageMedium> {
    use serde::Deserialize;
    use wmi::WMIConnection;

    #[derive(Deserialize)]
    struct Win32DiskPartition {
        #[serde(rename = "DeviceID")]
        device_id: String,
    }

    #[derive(Deserialize)]
    struct Win32DiskDrive {
        #[serde(rename = "Model")]
        model: Option<String>,
    }

    #[derive(Deserialize)]
    struct MsftPhysicalDisk {
        #[serde(rename = "MediaType")]
        media_type: Option<u16>,
        #[serde(rename = "Model")]
        model: Option<String>,
        #[serde(rename = "FriendlyName")]
        friendly_name: Option<String>,
    }

    let cimv2 = WMIConnection::new().ok()?;
    let q1 = format!(
        "ASSOCIATORS OF {{Win32_LogicalDisk.DeviceID='{}'}} WHERE AssocClass=Win32_LogicalDiskToPartition",
        root
    );
    let partitions: Vec<Win32DiskPartition> = cimv2.raw_query(q1).ok()?;
    let partition = partitions.first()?;
    let q2 = format!(
        "ASSOCIATORS OF {{Win32_DiskPartition.DeviceID='{}'}} WHERE AssocClass=Win32_DiskDriveToDiskPartition",
        wql_escape_single_quoted(&partition.device_id)
    );
    let drives: Vec<Win32DiskDrive> = cimv2.raw_query(q2).ok()?;
    let drive = drives.first()?;
    let model = drive.model.as_deref().unwrap_or("");

    let storage = WMIConnection::with_namespace_path("ROOT\\Microsoft\\Windows\\Storage").ok()?;
    let physical_disks: Vec<MsftPhysicalDisk> = storage
        .raw_query("SELECT MediaType, Model, FriendlyName FROM MSFT_PhysicalDisk")
        .ok()?;
    for pd in &physical_disks {
        let pd_model = pd.model.as_deref().unwrap_or("");
        let pd_name = pd.friendly_name.as_deref().unwrap_or("");
        if !model.is_empty() && model_match(model, pd_model, pd_name) {
            if let Some(medium) = media_type_to_medium(pd.media_type) {
                return Some(medium);
            }
        }
    }

    if model_implies_ssd(model) {
        return Some(StorageMedium::Ssd);
    }

    None
}

#[cfg(windows)]
fn media_type_to_medium(media_type: Option<u16>) -> Option<StorageMedium> {
    match media_type {
        Some(3) => Some(StorageMedium::Hdd),
        Some(4) | Some(5) => Some(StorageMedium::Ssd),
        _ => None,
    }
}

#[cfg(windows)]
fn model_implies_ssd(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("ssd") || lower.contains("nvme")
}

#[cfg(windows)]
fn model_match(base: &str, lhs: &str, rhs: &str) -> bool {
    let base_l = base.to_ascii_lowercase();
    let lhs_l = lhs.to_ascii_lowercase();
    let rhs_l = rhs.to_ascii_lowercase();
    lhs_l.contains(&base_l)
        || base_l.contains(&lhs_l)
        || rhs_l.contains(&base_l)
        || base_l.contains(&rhs_l)
}

#[cfg(windows)]
fn drive_root(path: &Path) -> Option<&str> {
    use std::path::Component;
    match path.components().next() {
        Some(Component::Prefix(prefix)) => prefix.as_os_str().to_str(),
        _ => None,
    }
}

#[cfg(windows)]
fn wql_escape_single_quoted(s: &str) -> String {
    s.replace('\\', r"\\").replace('\'', r"\'")
}
