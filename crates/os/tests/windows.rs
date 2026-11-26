//! Operations used by the native ERNIE loader on a real Windows host.
#![cfg(windows)]

use os::FileExt;
use std::fs::{self, File};
use std::io::ErrorKind;

#[test]
fn shards_support_positioned_io_and_identity() {
    let dir = std::env::temp_dir().join(format!("bloomery-shard 雪 {}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("model shard");
    let other = dir.join("occupied");
    let file = File::create_new(&path).unwrap();
    file.write_all_at(b"first", 1024).unwrap();
    file.write_all_at(b"last", 8192).unwrap();
    let mut bytes = [0; 5];
    file.read_exact_at(&mut bytes, 1024).unwrap();
    assert_eq!(&bytes, b"first");
    assert_eq!(
        file.read_exact_at(&mut bytes, 8192).unwrap_err().kind(),
        ErrorKind::UnexpectedEof
    );
    let same = File::open(&path).unwrap();
    assert_eq!(
        os::file_identity(&file).unwrap(),
        os::file_identity(&same).unwrap()
    );
    fs::write(&other, b"keep").unwrap();
    assert!(os::rename_noreplace(&path, &other).is_err());
    assert_eq!(fs::read(&other).unwrap(), b"keep");
    drop((file, same));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn workers_pin_only_to_eligible_cores() {
    let initial = os::mask_cpus().unwrap();
    let cores: Vec<_> = os::cache_groups()
        .unwrap()
        .into_iter()
        .flat_map(|(cores, _)| cores)
        .flatten()
        .collect();
    assert!(!initial.is_empty());
    assert!(initial.iter().all(|cpu| cores.contains(cpu)));
    os::pin(initial[0]).unwrap();
    let pinned = os::mask_cpus().unwrap();
    os::set_mask(&initial).unwrap();
    assert_eq!(pinned, [initial[0]]);
    assert!(os::set_mask(&[]).is_err());
}

#[test]
fn mapped_pages_populate_lock_and_report_residency() {
    let memory = os::host_memory().unwrap();
    assert!((1..=memory.total_bytes).contains(&memory.available_bytes));
    let page = usize::try_from(os::page_bytes().unwrap()).unwrap();
    let mut bytes = vec![0_u8; 4 * page];
    let offset = bytes.as_ptr().align_offset(page);
    let span = &mut bytes[offset..offset + 2 * page];
    let addr = span.as_mut_ptr().cast();
    let len = span.len();
    let quota = os::LockQuota::reserve(len as u64).unwrap();
    let mut residency = [0; 2];
    // SAFETY: both pages are aligned and remain in the owned vector through the unlock.
    unsafe {
        os::advise_populate(addr, len).unwrap();
        assert_eq!(
            os::lock_pages(addr, len),
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        let read = os::mincore(addr, len, &mut residency);
        assert_eq!(os::unlock_pages(addr, len), 0);
        read.unwrap();
    }
    drop(quota);
    assert_eq!(residency, [1, 1]);
}
