//! Win32 services for the native x64 host.
use super::{HostMemory, OsError, counted_page};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{FileExt as WinFileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::{
    Foundation::ERROR_INSUFFICIENT_BUFFER,
    Storage::FileSystem::*,
    System::{Memory::*, ProcessStatus::*, SystemInformation::*, Threading::*},
};

fn check(ok: i32) -> Result<(), OsError> {
    if ok == 0 {
        Err(io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut v: Vec<_> = path.as_os_str().encode_wide().collect();
    if v.contains(&0) {
        return Err(io::Error::new(ErrorKind::InvalidInput, "NUL in path"));
    }
    v.push(0);
    Ok(v)
}

fn bits(mask: usize) -> Vec<u32> {
    (0..usize::BITS)
        .filter(|&cpu| mask & (1usize << cpu) != 0)
        .collect()
}

fn process_mask() -> Result<usize, OsError> {
    let (mut process, mut system) = (0, 0);
    // SAFETY: the pseudo-handle belongs to this process and both outputs are live.
    unsafe {
        if GetActiveProcessorGroupCount() != 1 {
            return Err(OsError::Unsupported {
                call: "CPU affinity across multiple Windows processor groups",
            });
        }
        check(GetProcessAffinityMask(
            GetCurrentProcess(),
            &mut process,
            &mut system,
        ))?;
    }
    Ok(process)
}

pub fn mask_cpus() -> Result<Vec<u32>, OsError> {
    let mut thread = GROUP_AFFINITY::default();
    // SAFETY: the thread pseudo-handle is valid and the output holds one GROUP_AFFINITY.
    unsafe {
        check(GetThreadGroupAffinity(GetCurrentThread(), &mut thread))?;
    }
    Ok(bits(thread.Mask & process_mask()?))
}

pub fn set_mask(cpus: &[u32]) -> Result<(), OsError> {
    let mut mask = 0;
    for &cpu in cpus {
        mask |= 1usize.checked_shl(cpu).ok_or_else(|| {
            io::Error::new(ErrorKind::InvalidInput, "CPU outside processor group")
        })?;
    }
    if mask == 0 || mask & !process_mask()? != 0 {
        return Err(
            io::Error::new(ErrorKind::InvalidInput, "CPU mask outside process affinity").into(),
        );
    }
    // SAFETY: the mask is nonempty and inside the process group.
    if unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

pub fn pin(cpu: u32) -> Result<(), OsError> {
    set_mask(&[cpu])
}
pub fn float_everywhere() -> Result<(), OsError> {
    set_mask(&bits(process_mask()?))
}

/// Physical cores (including SMT siblings) grouped by their shared L3 cache.
pub fn cache_groups() -> Result<Vec<(Vec<Vec<u32>>, u64)>, OsError> {
    process_mask()?;
    let mut len = 0;
    // SAFETY: a null buffer queries the required byte count.
    unsafe {
        GetLogicalProcessorInformation(std::ptr::null_mut(), &mut len);
    }
    if io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(io::Error::last_os_error().into());
    }
    let mut info = vec![
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION::default();
        (len as usize).div_ceil(size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>())
    ];
    // SAFETY: info is aligned and has capacity for len bytes.
    unsafe {
        check(GetLogicalProcessorInformation(info.as_mut_ptr(), &mut len))?;
    }
    info.truncate(len as usize / size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>());
    let mut caches = Vec::new();
    for row in &info {
        if row.Relationship == RelationCache {
            // SAFETY: RelationCache selects this union member.
            let cache = unsafe { row.Anonymous.Cache };
            if cache.Level == 3 {
                caches.push((row.ProcessorMask, u64::from(cache.Size)));
            }
        }
    }
    let mut groups = std::collections::BTreeMap::new();
    for core in info
        .iter()
        .filter(|r| r.Relationship == RelationProcessorCore)
    {
        let (mask, bytes) = caches
            .iter()
            .copied()
            .find(|(m, _)| m & core.ProcessorMask != 0)
            .unwrap_or((usize::MAX, 0));
        groups
            .entry((mask, bytes))
            .or_insert_with(Vec::new)
            .push(bits(core.ProcessorMask));
    }
    Ok(groups
        .into_iter()
        .map(|((_, bytes), mut cores)| {
            cores.sort_unstable();
            (cores, bytes)
        })
        .collect())
}

pub fn host_memory() -> Result<HostMemory, OsError> {
    let mut m = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: initialized size and writable output.
    unsafe {
        check(GlobalMemoryStatusEx(&mut m))?;
    }
    Ok(HostMemory {
        total_bytes: m.ullTotalPhys,
        available_bytes: m.ullAvailPhys,
    })
}

pub fn page_bytes() -> Result<u64, OsError> {
    let mut i = SYSTEM_INFO::default();
    // SAFETY: writable system information output.
    unsafe {
        GetSystemInfo(&mut i);
    }
    counted_page(u64::from(i.dwPageSize))
}

pub fn max_rss_bytes() -> Result<u64, OsError> {
    let n = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let mut m = PROCESS_MEMORY_COUNTERS {
        cb: n,
        ..Default::default()
    };
    // SAFETY: the handle is current and the buffer is exactly n bytes.
    unsafe {
        check(K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, n))?;
    }
    Ok(m.PeakWorkingSetSize as u64)
}

pub fn thread_faults() -> Result<u64, OsError> {
    Err(OsError::Unsupported {
        call: "per-thread page faults",
    })
}
pub fn children_max_rss_bytes() -> Result<u64, OsError> {
    Err(OsError::Unsupported {
        call: "peak resident bytes of exited children",
    })
}

pub const DIRECT_FLAG: Option<u32> = Some(FILE_FLAG_NO_BUFFERING);
pub fn open_direct(path: &Path) -> Result<File, OsError> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(path)?)
}
/// Read a local disk file into its caller's buffer, including an unbuffered file's short final sector.
/// The remaining bytes retain their original contents.
///
/// # Safety
/// The handle must be synchronous (opened without FILE_FLAG_OVERLAPPED).
pub unsafe fn read_sync_at(
    file: &File,
    dst: &mut [std::mem::MaybeUninit<u8>],
    at: u64,
) -> io::Result<usize> {
    use windows_sys::Win32::{
        Foundation::ERROR_HANDLE_EOF,
        System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0},
    };

    let mut got = 0;
    while got < dst.len() {
        let offset = at.checked_add(got as u64).ok_or(ErrorKind::InvalidInput)?;
        // ReadFile takes a DWORD. Keep the offset and buffer of each subsequent call sector-aligned.
        let count = (dst.len() - got).min(0x7fff_f000) as u32;
        let mut position = OVERLAPPED {
            Anonymous: OVERLAPPED_0 {
                Anonymous: OVERLAPPED_0_0 {
                    Offset: offset as u32,
                    OffsetHigh: (offset >> 32) as u32,
                },
            },
            ..Default::default()
        };
        let mut read = 0;
        // SAFETY: this is a synchronous handle. The sub-slice, byte count and explicit
        // file position stay valid through completion. ReadFile initializes the returned prefix.
        let ok = unsafe {
            ReadFile(
                file.as_raw_handle(),
                dst[got..].as_mut_ptr().cast(),
                count,
                &mut read,
                &mut position,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
                break;
            }
            return Err(error);
        }
        got += read as usize;
        // A regular disk file returns its remaining bytes at EOF. Another unbuffered read
        // starting at a partial sector would fail with ERROR_INVALID_PARAMETER.
        if read < count {
            break;
        }
    }
    Ok(got)
}

pub fn rename_noreplace(from: &Path, to: &Path) -> Result<(), OsError> {
    let (from, to) = (wide(from)?, wide(to)?);
    // SAFETY: both paths are nul-terminated; flags exclude replacing an existing file.
    unsafe { check(MoveFileExW(from.as_ptr(), to.as_ptr(), 0)) }
}
pub fn free_bytes(path: &Path) -> Result<u64, OsError> {
    let path = wide(path)?;
    let mut n = 0;
    // SAFETY: the path is terminated and the only requested output is live.
    unsafe {
        check(GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut n,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ))?;
    }
    Ok(n)
}
pub fn sync_file_range(file: &File, _: i64, _: i64) -> Result<(), OsError> {
    Ok(file.sync_data()?)
}
pub fn drop_file_cache(_: &File, _: i64, _: i64) -> Result<(), OsError> {
    Err(OsError::Unsupported {
        call: "posix_fadvise",
    })
}

pub fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    let mut i = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the file owns the handle and i is a writable structure.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut i) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        u64::from(i.dwVolumeSerialNumber),
        (u64::from(i.nFileIndexHigh) << 32) | u64::from(i.nFileIndexLow),
    ))
}

/// # Safety
/// The range must stay mapped until its matching unlock.
pub unsafe fn lock_pages(addr: *const c_void, len: usize) -> i32 {
    // SAFETY: the caller owns this live range.
    if unsafe { VirtualLock(addr as *mut _, len) } != 0 {
        0
    } else {
        -1
    }
}
/// # Safety
/// The range must still be mapped.
pub unsafe fn unlock_pages(addr: *const c_void, len: usize) -> i32 {
    // SAFETY: the caller owns this live range.
    if unsafe { VirtualUnlock(addr as *mut _, len) } != 0 {
        0
    } else {
        -1
    }
}

/// # Safety
/// The range starts on a page boundary and stays mapped during the query.
pub unsafe fn mincore(addr: *mut c_void, len: usize, vec: &mut [u8]) -> Result<(), OsError> {
    let page = page_bytes()? as usize;
    let count = len.div_ceil(page);
    let pages = vec
        .get_mut(..count)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "short residency vector"))?;
    let mut entries = vec![PSAPI_WORKING_SET_EX_INFORMATION::default(); count.min(4096)];
    for (chunk, out) in pages.chunks_mut(4096).enumerate() {
        let rows = &mut entries[..out.len()];
        for (i, row) in rows.iter_mut().enumerate() {
            row.VirtualAddress = addr.wrapping_byte_add((chunk * 4096 + i) * page);
        }
        // SAFETY: each address belongs to the mapping and rows is the exact writable query buffer.
        unsafe {
            check(K32QueryWorkingSetEx(
                GetCurrentProcess(),
                rows.as_mut_ptr().cast(),
                size_of_val(rows) as u32,
            ))?;
        }
        for (v, row) in out.iter_mut().zip(rows) {
            // SAFETY: Flags covers the complete working-set attributes union. Its low bit is Valid.
            *v = (unsafe { row.VirtualAttributes.Flags } & 1) as u8;
        }
    }
    Ok(())
}

/// Positional I/O for large shard files and sidecars.
pub trait FileExt {
    fn read_at(&self, dst: &mut [u8], offset: u64) -> io::Result<usize>;
    fn read_exact_at(&self, dst: &mut [u8], offset: u64) -> io::Result<()>;
    fn write_all_at(&self, src: &[u8], offset: u64) -> io::Result<()>;
}
impl FileExt for File {
    fn read_at(&self, dst: &mut [u8], offset: u64) -> io::Result<usize> {
        self.seek_read(dst, offset)
    }
    fn read_exact_at(&self, mut dst: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !dst.is_empty() {
            let n = self.seek_read(dst, offset)?;
            if n == 0 {
                return Err(ErrorKind::UnexpectedEof.into());
            }
            dst = &mut dst[n..];
            offset += n as u64;
        }
        Ok(())
    }
    fn write_all_at(&self, mut src: &[u8], mut offset: u64) -> io::Result<()> {
        while !src.is_empty() {
            let n = self.seek_write(src, offset)?;
            if n == 0 {
                return Err(ErrorKind::WriteZero.into());
            }
            src = &src[n..];
            offset += n as u64;
        }
        Ok(())
    }
}

pub fn logical_cpu_count() -> u32 {
    // SAFETY: queries a fixed platform counter across the host's processor groups.
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
}

/// # Safety
/// The range belongs to a live mapping.
pub unsafe fn prefetch(addr: *mut c_void, len: usize) -> Result<(), OsError> {
    let mut range = WIN32_MEMORY_RANGE_ENTRY {
        VirtualAddress: addr,
        NumberOfBytes: len,
    };
    // SAFETY: the range is live and its descriptor stays in scope for the call.
    unsafe { check(PrefetchVirtualMemory(GetCurrentProcess(), 1, &mut range, 0)) }
}

/// The owning process and every child it spawns join this job. The kernel closes the
/// sole, non-inheritable handle on process exit, including an abrupt termination.
pub fn guard_children() -> io::Result<()> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::*;
    static JOB: OnceLock<Result<OwnedHandle, i32>> = OnceLock::new();
    let result = JOB.get_or_init(|| {
        // SAFETY: the job is anonymous, its handle is not inheritable and the
        // configured limits structure stays in scope for the Win32 calls.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error().raw_os_error().unwrap_or(1));
            }
            let owned = OwnedHandle::from_raw_handle(job);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(job, GetCurrentProcess()) == 0
            {
                return Err(io::Error::last_os_error().raw_os_error().unwrap_or(1));
            }
            Ok(owned)
        }
    });
    result
        .as_ref()
        .map(|_| ())
        .map_err(|&e| io::Error::from_raw_os_error(e))
}

struct WorkingSet {
    min: usize,
    max: usize,
    reserved: usize,
}
static WORKING_SET: std::sync::Mutex<Option<WorkingSet>> = std::sync::Mutex::new(None);

/// Working-set room requested for VirtualLock; the original limits return after the last load.
pub struct LockQuota(usize);

impl LockQuota {
    pub fn reserve(bytes: u64) -> Result<Self, OsError> {
        let bytes = usize::try_from(bytes).map_err(io::Error::other)?;
        if bytes == 0 {
            return Ok(Self(0));
        }
        let mut state = WORKING_SET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.is_none() {
            let (mut min, mut max) = (0, 0);
            // SAFETY: the current process is valid and the outputs are writable.
            unsafe {
                check(GetProcessWorkingSetSize(
                    GetCurrentProcess(),
                    &mut min,
                    &mut max,
                ))?;
            }
            *state = Some(WorkingSet {
                min,
                max,
                reserved: 0,
            });
        }
        let limits = state.as_mut().expect("working set initialized");
        let reserved = limits
            .reserved
            .checked_add(bytes)
            .ok_or_else(|| io::Error::other("working-set size overflow"))?;
        let min = limits
            .min
            .checked_add(reserved)
            .and_then(|v| v.checked_add(16 << 20))
            .ok_or_else(|| io::Error::other("working-set size overflow"))?;
        // SAFETY: this changes the current process only. VirtualLock checks the actual quota.
        unsafe {
            check(SetProcessWorkingSetSize(
                GetCurrentProcess(),
                min,
                min.max(limits.max),
            ))?;
        }
        limits.reserved = reserved;
        Ok(Self(bytes))
    }
}
impl Drop for LockQuota {
    fn drop(&mut self) {
        if self.0 == 0 {
            return;
        }
        let mut state = WORKING_SET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(limits) = state.as_mut() else { return };
        limits.reserved -= self.0;
        if limits.reserved == 0 {
            // SAFETY: all reservations and their page locks have ended.
            if unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), limits.min, limits.max) } == 0
            {
                eprintln!(
                    "restore Windows working-set limits: {}",
                    io::Error::last_os_error()
                );
            }
            *state = None;
        }
    }
}
