//! Direct userspace Mali Midgard hardware-counter reader.
//!
//! This is the application-side half of `tools/mali-hwcnt-probe.c`: the probe proves the legacy
//! r12p0 UK/vinstr ABI independently, while this module exposes only `init` + `sample` to the draw
//! profiler.  It never changes clocks, power policy, sysfs, or kernel state beyond opening the
//! stock `/dev/mali0` system-monitor context.
//!
//! The target contract is intentionally narrow and fails closed: UK 10.2, reader API 1, hardware
//! layout 5, 1280-byte dumps, and a 16-buffer mapping.  A different driver must be identified and
//! decoded before its words receive T82x names.

use libc::{c_int, c_ulong, c_void};
use std::cell::RefCell;
use std::ffi::c_char;
use std::mem::{size_of, zeroed};
use std::ptr;

pub(crate) const RAW_WORDS: usize = 320;
const DUMP_SIZE: usize = RAW_WORDS * size_of::<u32>();
const BUFFER_COUNT: u32 = 16;
const MAP_SIZE: usize = DUMP_SIZE * BUFFER_COUNT as usize;
const POLL_TIMEOUT_MS: c_int = 2000;

const UK_MAJOR: u16 = 10;
const UK_MINOR: u16 = 2;
const UK_FUNC_ID: u32 = 512;
const KBASE_FUNC_SET_FLAGS: u32 = UK_FUNC_ID + 18;
const KBASE_FUNC_HWCNT_READER_SETUP: u32 = UK_FUNC_ID + 36;
const BASE_CONTEXT_SYSTEM_MONITOR_SUBMIT_DISABLED: u32 = 1 << 1;

const EXPECTED_API: u32 = 1;
const EXPECTED_HWVER: u32 = 5;

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const IOC_TYPESHIFT: u32 = 8;
const IOC_SIZESHIFT: u32 = 16;
const IOC_DIRSHIFT: u32 = 30;
const HWCNT_READER_TYPE: u32 = 0xBE;

#[repr(C)]
union UkHeader {
    id: u32,
    ret: u32,
    _align: u64,
}

#[repr(C)]
struct VersionArgs {
    header: UkHeader,
    major: u16,
    minor: u16,
    padding: u32,
}

#[repr(C)]
struct SetFlagsArgs {
    header: UkHeader,
    create_flags: u32,
    padding: u32,
}

#[repr(C)]
struct ReaderSetupArgs {
    header: UkHeader,
    buffer_count: u32,
    jm_bm: u32,
    shader_bm: u32,
    tiler_bm: u32,
    mmu_l2_bm: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Metadata {
    timestamp: u64,
    event_id: u32,
    buffer_idx: u32,
}

#[derive(Clone)]
pub(crate) struct Sample {
    pub(crate) timestamp_ns: u64,
    pub(crate) event_id: u32,
    pub(crate) words: [u32; RAW_WORDS],
}

pub(crate) struct Info {
    pub(crate) api: u32,
    pub(crate) hwver: u32,
    pub(crate) dump_size: u32,
    pub(crate) buffer_count: u32,
    pub(crate) map_size: usize,
    pub(crate) page_size: usize,
}

struct Fd(c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
        }
    }
}

struct Reader {
    _mali: Fd,
    reader: Fd,
    mapping: *const u8,
}

impl Drop for Reader {
    fn drop(&mut self) {
        if !self.mapping.is_null() {
            unsafe { libc::munmap(self.mapping as *mut c_void, MAP_SIZE) };
        }
    }
}

thread_local! {
    // The GLES renderer and profiler are main-thread-only. A thread-local keeps that invariant
    // safe without turning the reader's mmap pointer into a process-wide `static mut`.
    static READER: RefCell<Option<Reader>> = const { RefCell::new(None) };
}

#[inline]
const fn ioctl_request(dir: u32, ty: u32, nr: u32, size: usize) -> c_ulong {
    ((dir << IOC_DIRSHIFT) | (size as u32) << IOC_SIZESHIFT | ty << IOC_TYPESHIFT | nr) as c_ulong
}

#[inline]
const fn legacy_request<T>() -> c_ulong {
    ioctl_request(IOC_READ | IOC_WRITE, 0, 0, size_of::<T>())
}

const GET_HWVER: c_ulong = ioctl_request(IOC_READ, HWCNT_READER_TYPE, 0x00, size_of::<u32>());
const GET_BUFFER_SIZE: c_ulong = ioctl_request(IOC_READ, HWCNT_READER_TYPE, 0x01, size_of::<u32>());
const DUMP: c_ulong = ioctl_request(IOC_WRITE, HWCNT_READER_TYPE, 0x10, size_of::<u32>());
const GET_BUFFER: c_ulong = ioctl_request(IOC_READ, HWCNT_READER_TYPE, 0x20, size_of::<Metadata>());
const PUT_BUFFER: c_ulong =
    ioctl_request(IOC_WRITE, HWCNT_READER_TYPE, 0x21, size_of::<Metadata>());
const GET_API_VERSION: c_ulong =
    ioctl_request(IOC_WRITE, HWCNT_READER_TYPE, 0xFF, size_of::<u32>());

fn os_error(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

unsafe fn call_ioctl<T>(
    fd: c_int,
    request: c_ulong,
    arg: &mut T,
    what: &str,
) -> Result<(), String> {
    // EINTR is a signal landing mid-call, not a driver refusal. Retrying is the whole difference
    // between a profiling leg that survives a stray signal and one that disables itself halfway
    // through and reports a truncated sample set as if it were the run.
    loop {
        if unsafe { libc::ioctl(fd, request, ptr::from_mut(arg)) } >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("{what}: {error}"));
        }
    }
}

impl Reader {
    fn open() -> Result<(Self, Info), String> {
        let mali_fd = unsafe {
            libc::open(
                c"/dev/mali0".as_ptr() as *const c_char,
                libc::O_RDWR | libc::O_CLOEXEC,
            )
        };
        if mali_fd < 0 {
            return Err(os_error("open /dev/mali0"));
        }
        let mali = Fd(mali_fd);

        let mut version = VersionArgs {
            header: UkHeader { _align: 0 },
            major: UK_MAJOR,
            minor: UK_MINOR,
            padding: 0,
        };
        version.header.id = 0;
        unsafe {
            call_ioctl(
                mali.0,
                legacy_request::<VersionArgs>(),
                &mut version,
                "UK version check",
            )?
        };
        let version_ret = unsafe { version.header.ret };
        if version_ret != 0 || version.major != UK_MAJOR || version.minor != UK_MINOR {
            return Err(format!(
                "unsupported UK ABI: ret={version_ret} version={}.{} (expected {UK_MAJOR}.{UK_MINOR})",
                version.major, version.minor
            ));
        }

        let mut flags = SetFlagsArgs {
            header: UkHeader { _align: 0 },
            create_flags: BASE_CONTEXT_SYSTEM_MONITOR_SUBMIT_DISABLED,
            padding: 0,
        };
        flags.header.id = KBASE_FUNC_SET_FLAGS;
        unsafe {
            call_ioctl(
                mali.0,
                legacy_request::<SetFlagsArgs>(),
                &mut flags,
                "SET_FLAGS",
            )?
        };
        let flags_ret = unsafe { flags.header.ret };
        if flags_ret != 0 {
            return Err(format!("SET_FLAGS returned {flags_ret}"));
        }

        let mut setup = ReaderSetupArgs {
            header: UkHeader { _align: 0 },
            buffer_count: BUFFER_COUNT,
            jm_bm: u32::MAX,
            shader_bm: u32::MAX,
            tiler_bm: u32::MAX,
            mmu_l2_bm: u32::MAX,
            fd: -1,
        };
        setup.header.id = KBASE_FUNC_HWCNT_READER_SETUP;
        unsafe {
            call_ioctl(
                mali.0,
                legacy_request::<ReaderSetupArgs>(),
                &mut setup,
                "HWCNT_READER_SETUP",
            )?
        };
        let setup_ret = unsafe { setup.header.ret };
        if setup_ret != 0 || setup.fd < 0 {
            return Err(format!(
                "HWCNT_READER_SETUP returned ret={setup_ret} fd={}",
                setup.fd
            ));
        }
        let reader = Fd(setup.fd);

        let mut api = 0u32;
        let mut hwver = 0u32;
        let mut dump_size = 0u32;
        unsafe {
            call_ioctl(reader.0, GET_API_VERSION, &mut api, "GET_API_VERSION")?;
            call_ioctl(reader.0, GET_HWVER, &mut hwver, "GET_HWVER")?;
            call_ioctl(reader.0, GET_BUFFER_SIZE, &mut dump_size, "GET_BUFFER_SIZE")?;
        }
        if api != EXPECTED_API || hwver != EXPECTED_HWVER || dump_size as usize != DUMP_SIZE {
            return Err(format!(
                "unsupported reader contract: api={api} hwver={hwver} dump={dump_size}"
            ));
        }

        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return Err(os_error("sysconf(_SC_PAGESIZE)"));
        }
        let page_size = page as usize;
        if MAP_SIZE % page_size != 0 {
            return Err(format!(
                "reader mmap is not page aligned: {MAP_SIZE} bytes, page={page_size}"
            ));
        }

        let mapping = unsafe {
            libc::mmap(
                ptr::null_mut(),
                MAP_SIZE,
                libc::PROT_READ,
                libc::MAP_SHARED,
                reader.0,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(os_error("mmap HWCNT reader"));
        }

        Ok((
            Self {
                _mali: mali,
                reader,
                mapping: mapping.cast(),
            },
            Info {
                api,
                hwver,
                dump_size,
                buffer_count: BUFFER_COUNT,
                map_size: MAP_SIZE,
                page_size,
            },
        ))
    }

    fn sample(&mut self) -> Result<Sample, String> {
        let mut ignored = 0u32;
        unsafe { call_ioctl(self.reader.0, DUMP, &mut ignored, "HWCNT_READER_DUMP")? };

        let mut pfd = libc::pollfd {
            fd: self.reader.0,
            events: libc::POLLIN,
            revents: 0,
        };
        let mut ready = unsafe { libc::poll(&mut pfd, 1, POLL_TIMEOUT_MS) };
        while ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {
            pfd.revents = 0;
            ready = unsafe { libc::poll(&mut pfd, 1, POLL_TIMEOUT_MS) };
        }
        if ready < 0 {
            return Err(os_error("poll HWCNT reader"));
        }
        if ready == 0 {
            return Err("poll HWCNT reader: timed out".to_string());
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(format!("poll HWCNT reader: revents=0x{:x}", pfd.revents));
        }

        let mut meta: Metadata = unsafe { zeroed() };
        unsafe {
            call_ioctl(
                self.reader.0,
                GET_BUFFER,
                &mut meta,
                "HWCNT_READER_GET_BUFFER",
            )?
        };
        if meta.buffer_idx >= BUFFER_COUNT {
            // Do NOT hand this back: PUT_BUFFER validates the index too, so the return would fail
            // as well and leave the reader wedged mid-handshake. An out-of-range index means the
            // mapping contract this module asserts is not what the driver is honouring, which is
            // unrecoverable by construction — the caller tears the reader down.
            return Err(format!("invalid HWCNT buffer index {}", meta.buffer_idx));
        }

        let mut words = [0u32; RAW_WORDS];
        let source = unsafe { self.mapping.add(meta.buffer_idx as usize * DUMP_SIZE) };
        unsafe { ptr::copy_nonoverlapping(source, words.as_mut_ptr().cast::<u8>(), DUMP_SIZE) };
        unsafe {
            call_ioctl(
                self.reader.0,
                PUT_BUFFER,
                &mut meta,
                "HWCNT_READER_PUT_BUFFER",
            )?
        };

        Ok(Sample {
            timestamp_ns: meta.timestamp,
            event_id: meta.event_id,
            words,
        })
    }
}

pub(crate) fn init() -> Result<Info, String> {
    let (reader, info) = Reader::open()?;
    READER.with(|slot| *slot.borrow_mut() = Some(reader));
    Ok(info)
}

/// Detach the vinstr client. Dropping [`Reader`] munmaps and closes the reader fd, which is what
/// makes the kernel release the counter hardware; while a client is attached kbase keeps the
/// shader cores and L2 powered up for counting. A profiler that disabled itself on an error but
/// stayed attached would go on perturbing the very numbers the next leg collects, so every path
/// that gives up on profiling must come through here.
pub(crate) fn shutdown() {
    READER.with(|slot| *slot.borrow_mut() = None);
}

/// The per-block `PRFCNT_EN` word (block header word 2), one entry per dump block.
///
/// This is not decoration. On this Mali-T820 the TILER block comes back with `0x1f` — five groups
/// of four counters, so tiler words 20..63 are not enabled in hardware and `TILER_ACTIVE` (word 22)
/// reads a flat zero no matter what the client bitmap asked for. Logging the mask is what tells a
/// zero counter from a disabled one; without it the two are indistinguishable in the output.
pub(crate) fn block_enables(sample: &Sample) -> [u32; 5] {
    std::array::from_fn(|block| sample.words[block * BLOCK_WORDS + 2])
}

pub(crate) fn sample() -> Result<Sample, String> {
    READER.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.as_mut()
            .ok_or_else(|| "HWCNT reader is not initialized".to_string())?
            .sample()
    })
}

// Hardware layout v5 on this MP2 target is five 64-word blocks: JM, tiler, one MMU/L2 slice,
// then two shader cores. The selected indices below come from Arm's r12p0 T82x name table. Only
// these reviewed names are decoded; the raw 320 words remain the authority for everything else.
const BLOCK_WORDS: usize = 64;

#[derive(Clone, Copy)]
enum Block {
    Jm,
    Tiler,
    L2,
    ShaderSum,
}

#[derive(Clone, Copy)]
pub(crate) struct CounterSpec {
    pub(crate) name: &'static str,
    block: Block,
    word: usize,
}

pub(crate) const COUNTERS: [CounterSpec; 30] = [
    CounterSpec {
        name: "GPU_ACTIVE",
        block: Block::Jm,
        word: 6,
    },
    CounterSpec {
        name: "JS0_ACTIVE",
        block: Block::Jm,
        word: 10,
    },
    CounterSpec {
        name: "JS1_ACTIVE",
        block: Block::Jm,
        word: 18,
    },
    // Verified against Arm's r12p0 T82x table, and STRUCTURALLY ZERO on this television anyway:
    // the tiler block reports `PRFCNT_EN = 0x1f`, so only words 4..19 are enabled in hardware and
    // word 22 can never increment. It is kept, named, and reported so the zero is explicable —
    // `block_enables` is what tells "the tiler was idle" from "this counter is switched off".
    CounterSpec {
        name: "TILER_ACTIVE",
        block: Block::Tiler,
        word: 22,
    },
    CounterSpec {
        name: "FRAG_ACTIVE",
        block: Block::ShaderSum,
        word: 4,
    },
    CounterSpec {
        name: "FRAG_PRIMITIVES",
        block: Block::ShaderSum,
        word: 5,
    },
    CounterSpec {
        name: "FRAG_QUADS_RAST",
        block: Block::ShaderSum,
        word: 14,
    },
    CounterSpec {
        name: "FRAG_NUM_TILES",
        block: Block::ShaderSum,
        word: 20,
    },
    // Midgard's TRANSACTION ELIMINATION: the tile writeback unit CRCs each finished tile against
    // what is already in memory and SKIPS the write when they match. Arm names this counter as the
    // way to detect an application redrawing an unchanged screen — which is precisely the question
    // "would per-region damage tracking pay here" reduces to, and it is answerable without writing
    // any damage code. Word 21 is cross-checked against four independent tables (Arm's r7p0
    // `mali_kbase_gator_hwcnt_names.h`, the Khadas/Amlogic vendor tree's copy of it, gator's
    // `hardware_counter_names` for T820, and the modern libmali `gen.h`); all four also agree on
    // words 4/14/20/26/27, which are already in this table, and that agreement is the check.
    CounterSpec {
        name: "FRAG_TRANS_ELIM",
        block: Block::ShaderSum,
        word: 21,
    },
    CounterSpec {
        name: "TRIPIPE_ACTIVE",
        block: Block::ShaderSum,
        word: 26,
    },
    CounterSpec {
        name: "ARITH_WORDS",
        block: Block::ShaderSum,
        word: 27,
    },
    CounterSpec {
        name: "LS_WORDS",
        block: Block::ShaderSum,
        word: 31,
    },
    CounterSpec {
        name: "LS_ISSUES",
        block: Block::ShaderSum,
        word: 32,
    },
    CounterSpec {
        name: "TEX_WORDS",
        block: Block::ShaderSum,
        word: 38,
    },
    CounterSpec {
        name: "TEX_ISSUES",
        block: Block::ShaderSum,
        word: 42,
    },
    CounterSpec {
        name: "LSC_READ_OP",
        block: Block::ShaderSum,
        word: 49,
    },
    CounterSpec {
        name: "LSC_WRITE_OP",
        block: Block::ShaderSum,
        word: 51,
    },
    CounterSpec {
        name: "SHADER_AXI_BEATS_READ",
        block: Block::ShaderSum,
        word: 62,
    },
    CounterSpec {
        name: "SHADER_AXI_BEATS_WRITTEN",
        block: Block::ShaderSum,
        word: 63,
    },
    CounterSpec {
        name: "MMU_REQUESTS",
        block: Block::L2,
        word: 9,
    },
    CounterSpec {
        name: "L2_EXT_WRITE_BEATS",
        block: Block::L2,
        word: 30,
    },
    CounterSpec {
        name: "L2_EXT_READ_BEATS",
        block: Block::L2,
        word: 31,
    },
    CounterSpec {
        name: "L2_ANY_LOOKUP",
        block: Block::L2,
        word: 32,
    },
    CounterSpec {
        name: "L2_READ_LOOKUP",
        block: Block::L2,
        word: 33,
    },
    CounterSpec {
        name: "L2_READ_HIT",
        block: Block::L2,
        word: 37,
    },
    CounterSpec {
        name: "L2_WRITE_LOOKUP",
        block: Block::L2,
        word: 39,
    },
    CounterSpec {
        name: "L2_WRITE_HIT",
        block: Block::L2,
        word: 43,
    },
    CounterSpec {
        name: "L2_EXT_READ",
        block: Block::L2,
        word: 48,
    },
    CounterSpec {
        name: "L2_EXT_WRITE",
        block: Block::L2,
        word: 50,
    },
    CounterSpec {
        name: "L2_EXT_W_STALL",
        block: Block::L2,
        word: 58,
    },
];

pub(crate) fn decode(sample: &Sample) -> [u64; COUNTERS.len()] {
    std::array::from_fn(|i| {
        let spec = COUNTERS[i];
        let at = |block: usize| sample.words[block * BLOCK_WORDS + spec.word] as u64;
        match spec.block {
            Block::Jm => at(0),
            Block::Tiler => at(1),
            Block::L2 => at(2),
            Block::ShaderSum => at(3) + at(4),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_shapes_match_the_r12p0_linux_uapi() {
        assert_eq!(size_of::<VersionArgs>(), 16);
        assert_eq!(size_of::<SetFlagsArgs>(), 16);
        assert_eq!(size_of::<ReaderSetupArgs>(), 32);
        assert_eq!(size_of::<Metadata>(), 16);
        assert_eq!(legacy_request::<VersionArgs>(), 0xC010_0000);
        assert_eq!(legacy_request::<ReaderSetupArgs>(), 0xC020_0000);
        assert_eq!(GET_HWVER, 0x8004_BE00);
        assert_eq!(DUMP, 0x4004_BE10);
        assert_eq!(GET_BUFFER, 0x8010_BE20);
        assert_eq!(GET_API_VERSION, 0x4004_BEFF);
    }

    #[test]
    fn validated_mapping_is_five_target_pages() {
        assert_eq!(DUMP_SIZE, 1280);
        assert_eq!(MAP_SIZE, 20480);
        assert_eq!(MAP_SIZE % 4096, 0);
    }

    /// `tools/analyze-hwcnt.py` re-declares the counter table so an archived JSONL can be decoded
    /// on the host long after the build that captured it. Two hand-maintained copies of the same
    /// index list is exactly how the two wrong L2 words (45 for `L2_EXT_READ`, 49 for
    /// `L2_EXT_WRITE`) survived being fixed in one place — the raw words outlive the run, so a
    /// stale analyzer keeps mis-decoding captures that are otherwise still good.
    #[test]
    fn the_host_analyzer_decodes_the_same_words_as_the_app() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/analyze-hwcnt.py");
        let source = std::fs::read_to_string(path).expect("analyze-hwcnt.py is readable");
        let table = source
            .split_once("SPECS = (")
            .expect("SPECS table is present")
            .1
            .split_once("\n)")
            .expect("SPECS table is terminated")
            .0;
        let python: Vec<(String, String, usize)> = table
            .lines()
            .filter_map(|line| {
                let inner = line.trim().strip_prefix('(')?.strip_suffix("),")?;
                let mut fields = inner.split(',').map(str::trim);
                let name = fields.next()?.trim_matches('"').to_string();
                let block = fields.next()?.trim_matches('"').to_string();
                Some((name, block, fields.next()?.parse().ok()?))
            })
            .collect();

        let rust: Vec<(String, String, usize)> = COUNTERS
            .iter()
            .map(|spec| {
                let block = match spec.block {
                    Block::Jm => "jm",
                    Block::Tiler => "tiler",
                    Block::L2 => "l2",
                    Block::ShaderSum => "shader",
                };
                (spec.name.to_string(), block.to_string(), spec.word)
            })
            .collect();

        assert_eq!(
            python, rust,
            "tools/analyze-hwcnt.py's SPECS has drifted from hwcnt::COUNTERS; \
             an archived JSONL would decode differently on the host than it logged on the device"
        );
    }

    #[test]
    fn shader_counters_sum_both_mp2_blocks() {
        let mut sample = Sample {
            timestamp_ns: 0,
            event_id: 0,
            words: [0; RAW_WORDS],
        };
        sample.words[3 * BLOCK_WORDS + 4] = 11;
        sample.words[4 * BLOCK_WORDS + 4] = 13;
        let decoded = decode(&sample);
        let at = COUNTERS
            .iter()
            .position(|c| c.name == "FRAG_ACTIVE")
            .unwrap();
        assert_eq!(decoded[at], 24);
    }
}
