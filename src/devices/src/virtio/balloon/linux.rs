use vm_memory::GuestMemoryMmap;

use super::device::{FreeRange, coalesce};

/// Gives the pages of free page reports back to the host: madvise(MADV_REMOVE) for guest memory in
/// a shared file, as vhost-user needs, and madvise(MADV_DONTNEED) for private memory.
#[derive(Default)]
pub(super) struct FreePageReclaim;

impl FreePageReclaim {
    /// Whether another report can be taken from the queue. Reports are released right away, so
    /// it always can.
    pub(super) fn can_take_report(&self) -> bool {
        true
    }

    /// Gives the pages of a report back to the host. That's done right away, so this returns
    /// false: the report can be used right away.
    pub(super) fn release(
        &mut self,
        mem: &GuestMemoryMmap,
        head: u16,
        ranges: Vec<FreeRange>,
    ) -> bool {
        self.release_with(mem, head, ranges, |range| {
            let addr = range.host_addr.cast();
            // SAFETY: the range is guest memory, whose contents the guest has given up.
            // MADV_DONTNEED would only unmap shared pages from this process, leaving them
            // allocated in the file; MADV_REMOVE frees them, and fails on private memory.
            unsafe {
                if libc::madvise(addr, range.len, libc::MADV_REMOVE) != 0 {
                    libc::madvise(addr, range.len, libc::MADV_DONTNEED);
                }
            }
        })
    }

    /// Like [`FreePageReclaim::release`], with `discard` giving back each run of adjacent ranges.
    /// The tests use it to count the calls.
    pub(super) fn release_with(
        &mut self,
        mem: &GuestMemoryMmap,
        _head: u16,
        ranges: Vec<FreeRange>,
        mut discard: impl FnMut(&FreeRange),
    ) -> bool {
        for range in coalesce(mem, ranges) {
            discard(&range);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::MetadataExt;

    use vm_memory::{Bytes, FileOffset, GuestAddress};

    use super::super::test_util::TestDevice;
    use super::*;

    /// A multiple of the page size of every host.
    const PAGE: usize = 64 << 10;
    const PAGES: usize = 16;

    #[test]
    fn test_released_ranges_read_as_zeroes() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), PAGES * PAGE)]).unwrap();
        mem.write_slice(&vec![0xff; PAGES * PAGE], GuestAddress(0))
            .unwrap();

        // (first page, pages)
        let released = [(1, 1), (2, 2), (8, 1), (15, 1)];
        let ranges = released
            .iter()
            .map(|&(page, pages)| {
                FreeRange::new(&mem, GuestAddress((page * PAGE) as u64), pages * PAGE).unwrap()
            })
            .collect();
        assert!(!FreePageReclaim.release(&mem, 0, ranges));

        let mut contents = vec![0; PAGES * PAGE];
        mem.read_slice(&mut contents, GuestAddress(0)).unwrap();
        for (page, contents) in contents.chunks(PAGE).enumerate() {
            let is_released = released
                .iter()
                .any(|&(first, pages)| (first..first + pages).contains(&page));
            let expected = if is_released { 0 } else { 0xff };
            assert!(contents.iter().all(|&b| b == expected), "page {page}");
        }
    }

    #[test]
    fn test_released_ranges_of_shared_memory_go_back_to_the_host() {
        // vhost-user devices need guest memory in a shared memfd, where MADV_DONTNEED only
        // unmaps pages from the VMM and leaves them allocated in the file.
        let fd = unsafe { libc::memfd_create(c"guest_mem".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0);
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len((PAGES * PAGE) as u64).unwrap();
        let allocated = || file.metadata().unwrap().blocks() * 512;
        let mem = GuestMemoryMmap::from_ranges_with_files(&[(
            GuestAddress(0),
            PAGES * PAGE,
            Some(FileOffset::new(file.try_clone().unwrap(), 0)),
        )])
        .unwrap();
        mem.write_slice(&vec![0xff; PAGES * PAGE], GuestAddress(0))
            .unwrap();
        assert_eq!(allocated(), (PAGES * PAGE) as u64);

        let range = FreeRange::new(&mem, GuestAddress(PAGE as u64), 4 * PAGE).unwrap();
        assert!(!FreePageReclaim.release(&mem, 0, vec![range]));

        assert_eq!(allocated(), ((PAGES - 4) * PAGE) as u64);
        let mut contents = vec![0xaa; 4 * PAGE];
        mem.read_slice(&mut contents, GuestAddress(PAGE as u64))
            .unwrap();
        assert!(contents.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_process_frq_releases_only_reported_ranges_within_guest_memory() {
        const MIB: usize = 1 << 20;
        // A hole of 3 MiB between two regions.
        let mem = GuestMemoryMmap::from_ranges(&[
            (GuestAddress(0), MIB),
            (GuestAddress(4 * MIB as u64), MIB),
        ])
        .unwrap();
        // The free page reporting queue is at the start of the first region.
        let (first, second) = (8 * PAGE, 4 * MIB);
        mem.write_slice(&vec![0xff; MIB - first], GuestAddress(first as u64))
            .unwrap();
        mem.write_slice(&vec![0xff; MIB], GuestAddress(second as u64))
            .unwrap();

        let mut dev = TestDevice::new(&mem);
        let released = [(9 * PAGE, PAGE), (11 * PAGE, 2 * PAGE)];
        let outside = [
            // Past the end of the first region, in the hole, and from the hole into the second
            // region. Releasing what is within a region would give pages back that the guest
            // didn't report.
            (MIB - PAGE, 2 * PAGE),
            (2 * MIB, PAGE),
            (second - PAGE, 2 * PAGE),
        ];
        let report: Vec<_> = released
            .iter()
            .chain(&outside)
            .map(|&(addr, len)| (addr as u64, len as u32))
            .collect();
        dev.report(&report);
        assert!(dev.balloon.process_frq());
        assert_eq!(dev.used(), 1);
        assert_eq!(dev.pending(&mem), 0);

        for (start, len) in [(first, MIB - first), (second, MIB)] {
            let mut contents = vec![0; len];
            mem.read_slice(&mut contents, GuestAddress(start as u64))
                .unwrap();
            for (page, contents) in contents.chunks(PAGE).enumerate() {
                let addr = start + page * PAGE;
                let is_released = released
                    .iter()
                    .any(|&(first, len)| (first..first + len).contains(&addr));
                let expected = if is_released { 0 } else { 0xff };
                assert!(contents.iter().all(|&b| b == expected), "page at {addr:#x}");
            }
        }
    }
}
