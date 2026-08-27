//! The read path must degrade, never panic.
//!
//! Every mutant here is fed through the full read surface -- parse, decode,
//! disassemble, every `dump_table` kind, the call graph, the literal buffers --
//! under `catch_unwind`. Any panic is a failure.
//!
//! This is the assertion that the read path's robustness is a property rather
//! than an accident. The sweep that first ran it (260,000 mutants over 13
//! fixtures) found exactly one reachable panic: an unchecked `u32 + u32` on the
//! register counts of a *large* modern function header, where both fields come
//! straight out of the file. See `docs/READ_PATH_GUIDE.md` F7.
//!
//! Run it in **debug** to catch integer overflow -- release builds wrap silently,
//! which is how that one hid. `HBC_FUZZ_FLIPS` raises the per-fixture bit-flip
//! count for a longer soak; the committed default keeps `cargo test` quick.

use std::panic::{catch_unwind, AssertUnwindSafe};

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().map(|x| x == "hbc").unwrap_or(false) {
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// xorshift, so the sweep is reproducible
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn probe(name: &str, bytes: &[u8], report: &mut Vec<String>) {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let file = hbc_decomp::BytecodeFile::parse_auto(bytes)?;
        let fmt = hbc_decomp::opcode::BytecodeFormat::for_version(file.header.version)?;
        let n = file.function_headers.len().min(64);
        for i in 0..n {
            let _ = file.decode_function_instructions(&fmt, i as u32);
            let _ = hbc_decomp::disassemble_function(&file, &fmt, i as u32, &hbc_decomp::DisasmOptions::default());
        }
        for k in [hbc_decomp::inspect::TableKind::CjsModules, hbc_decomp::inspect::TableKind::RegExp, hbc_decomp::inspect::TableKind::ObjShapes, hbc_decomp::inspect::TableKind::FunctionSources, hbc_decomp::inspect::TableKind::StringKinds, hbc_decomp::inspect::TableKind::Sections, hbc_decomp::inspect::TableKind::BigInt, hbc_decomp::inspect::TableKind::ArrayBuffer] { let _ = hbc_decomp::inspect::dump_table(&file, k); let _ = hbc_decomp::inspect::dump_table_json(&file, k); }
        let _ = hbc_decomp::inspect::function_info_banner(&file, 0);
        let _ = hbc_decomp::render_call_graph(&file, &fmt, Some(0), 3, false);
        // debug info / source locations
        if let Some(di) = &file.debug_info {
            let _ = format!("{:?}", di.string_table.len());
        }
        // literal buffers, driven by shape/regexp tables as the IR does
        for sh in file.obj_shape_table.iter().take(64) {
            let _ = file.read_key_buffer_series(sh.key_buffer_offset, sh.num_props);
        }
        for i in 0..file.big_int_table.len().min(64) { let _ = file.bigint_at(i as u32); }
        for off in [0u32, 1, 2, 4, 8] {
            let _ = file.read_array_buffer_series(off, 16);
            let _ = file.read_value_buffer_series(off, 16);
        }
        // analysis + full decompile of the first few functions
        let _ = hbc_decomp::scan_secrets(&file, &[]);
        Ok::<_, hbc_decomp::Error>(())
    }));
    match r {
        Ok(_) => {}
        Err(e) => {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic>".into());
            report.push(format!("{name}: PANIC {msg}"));
        }
    }
}

#[test]
fn corrupt_inputs_never_panic() {
    let mut report = Vec::new();
    for (name, base) in fixtures() {
        // 1. truncations
        for denom in [2usize, 4, 8, 16, 32, 64, 128] {
            let n = base.len() / denom;
            probe(&format!("{name}/trunc-{n}"), &base[..n], &mut report);
        }
        // 2. single-byte flips, seeded
        let mut rng = Rng(0x5eed_1234_9abc_def0);
        let flips: usize = std::env::var("HBC_FUZZ_FLIPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(750);
        for i in 0..flips {
            let mut m = base.clone();
            let pos = (rng.next() as usize) % m.len();
            m[pos] ^= 1u8 << (rng.next() % 8);
            probe(&format!("{name}/flip{i}@{pos}"), &m, &mut report);
        }
        // 3. u32 field smashes in the header region (0..128) -> huge counts
        for pos in (0..128).step_by(4) {
            for val in [u32::MAX, 0x7fff_ffff, 0xffff_0000, 1u32 << 24] {
                let mut m = base.clone();
                m[pos..pos + 4].copy_from_slice(&val.to_le_bytes());
                probe(&format!("{name}/hdr@{pos}={val:#x}"), &m, &mut report);
            }
        }
    }
    // dedupe by panic message tail
    let mut seen = std::collections::BTreeMap::<String, (usize, String)>::new();
    for line in &report {
        let key = line.split(": PANIC ").nth(1).unwrap_or(line).to_string();
        let e = seen.entry(key).or_insert((0, line.clone()));
        e.0 += 1;
    }
    for (k, (n, ex)) in &seen {
        println!("### {n}x  {k}\n    e.g. {ex}");
    }
    println!("TOTAL PANICS: {} distinct: {}", report.len(), seen.len());
    assert!(report.is_empty(), "{} panics", report.len());
}
