use crate::cli_args::{FunctionLayoutArg, LayoutArg};
use hbc_decomp::file::header::{peek_version, MODERN_FUNCTION_HEADER_MIN_VERSION};
use hbc_decomp::{BytecodeFile, BytecodeFormat, FunctionHeaderLayout, HeaderLayout};
use std::fs;
use std::path::PathBuf;

pub fn load_file(
    input: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
) -> Result<BytecodeFile, Box<dyn std::error::Error>> {
    let (file, _) = load_file_with_bytes(input, layout, function_layout)?;
    Ok(file)
}

pub fn load_file_with_bytes(
    input: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
) -> Result<(BytecodeFile, Vec<u8>), Box<dyn std::error::Error>> {
    let bytes = fs::read(input)?;
    warn_layout_mismatch(&bytes, layout, function_layout);
    let file = match layout {
        LayoutArg::Auto => BytecodeFile::parse_auto(&bytes)?,
        LayoutArg::Legacy => {
            let function_layout = resolve_function_layout(layout, function_layout);
            BytecodeFile::parse_with_layout(&bytes, HeaderLayout::Legacy, function_layout)?
        }
        LayoutArg::Modern => {
            let function_layout = resolve_function_layout(layout, function_layout);
            BytecodeFile::parse_with_layout(&bytes, HeaderLayout::Modern, function_layout)?
        }
    };
    Ok((file, bytes))
}

// Forcing a layout that contradicts the file's declared version reads every
// section at the wrong stride, so the failure surfaces far from its cause --
// typically as a nonsense offset ("seek out of range") from a function header
// decoded out of garbage. Say so before parsing, while the version is still in
// hand. This stays a warning, not an error: overriding the layout is the point
// of these flags, and a hand-patched file may legitimately need it.
fn warn_layout_mismatch(bytes: &[u8], layout: LayoutArg, function_layout: FunctionLayoutArg) {
    let Ok(version) = peek_version(bytes) else {
        // Not a recognisable bundle; the parse error will say so on its own.
        return;
    };
    let modern_expected = version >= MODERN_FUNCTION_HEADER_MIN_VERSION;
    let (expected, threshold) = if modern_expected {
        ("Modern", format!(">= {MODERN_FUNCTION_HEADER_MIN_VERSION}"))
    } else {
        ("Legacy", format!("< {MODERN_FUNCTION_HEADER_MIN_VERSION}"))
    };

    let forced_modern = match layout {
        LayoutArg::Auto => None,
        LayoutArg::Legacy => Some(false),
        LayoutArg::Modern => Some(true),
    };
    if let Some(forced_modern) = forced_modern {
        if forced_modern != modern_expected {
            let forced = if forced_modern { "modern" } else { "legacy" };
            eprintln!(
                "warning: --layout {forced} was given, but this file declares bytecode \
                 version {version} ({threshold}), which uses the {expected} layout. \
                 Parsing will likely fail or decode garbage; re-run without --layout to \
                 auto-detect."
            );
        }
    }

    // Only worth a second line when it was set explicitly -- otherwise it just
    // restates the --layout warning above.
    let forced_modern12 = match function_layout {
        FunctionLayoutArg::Auto => None,
        FunctionLayoutArg::Legacy16 => Some(false),
        FunctionLayoutArg::Modern12 => Some(true),
    };
    if let Some(forced_modern12) = forced_modern12 {
        if forced_modern.is_none() {
            // parse_auto picks both layouts together from the version, so an
            // explicit --function-layout never reaches it. Say it was dropped
            // rather than let the caller believe it took effect.
            eprintln!(
                "warning: --function-layout is ignored when --layout is auto (the default); \
                 pass --layout legacy or --layout modern to force it."
            );
        } else if forced_modern12 != modern_expected {
            let forced = if forced_modern12 {
                "modern12"
            } else {
                "legacy16"
            };
            let expected_fn = if modern_expected {
                "Modern12 (12-byte)"
            } else {
                "Legacy16 (16-byte)"
            };
            eprintln!(
                "warning: --function-layout {forced} was given, but bytecode version \
                 {version} ({threshold}) uses {expected_fn} function headers. The header \
                 table will be walked at the wrong stride."
            );
        }
    }
}

pub fn resolve_function_layout(
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
) -> FunctionHeaderLayout {
    match function_layout {
        FunctionLayoutArg::Legacy16 => FunctionHeaderLayout::Legacy16,
        FunctionLayoutArg::Modern12 => FunctionHeaderLayout::Modern12,
        FunctionLayoutArg::Auto => match layout {
            LayoutArg::Legacy => FunctionHeaderLayout::Legacy16,
            LayoutArg::Modern => FunctionHeaderLayout::Modern12,
            LayoutArg::Auto => FunctionHeaderLayout::Legacy16,
        },
    }
}

pub fn load_format(
    file: &BytecodeFile,
    format_version: Option<u32>,
) -> Result<BytecodeFormat, Box<dyn std::error::Error>> {
    let version = format_version.unwrap_or(file.header.version);
    let (format, used_version) = BytecodeFormat::for_version_or_latest(version)?;
    if used_version != version {
        eprintln!(
            "warning: using opcode format version {used_version} for bytecode version {version}"
        );
    }
    Ok(format)
}

pub fn write_output(
    output: Option<PathBuf>,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = output {
        fs::write(&path, content)?;
        let lines = content.lines().count();
        let kib = content.len() as f64 / 1024.0;
        eprintln!(
            "Wrote {} ({} lines, {:.1} KiB)",
            path.display(),
            lines,
            kib
        );
    } else {
        print!("{content}");
    }
    Ok(())
}

// Parse a `--modules` spec like "100-150,200,5-9" into inclusive id ranges.
// A bare id `N` becomes `(N, N)`. Malformed entries are skipped.
pub fn parse_id_ranges(spec: Option<&str>) -> Vec<(u32, u32)> {
    let spec = match spec {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut ranges = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<u32>(), hi.trim().parse::<u32>()) {
                ranges.push((lo.min(hi), lo.max(hi)));
            }
        } else if let Ok(n) = part.parse::<u32>() {
            ranges.push((n, n));
        }
    }
    ranges
}

// Parse a comma-separated glob list (e.g. "react*,lodash*") into trimmed,
// non-empty patterns.
pub fn parse_globs(spec: Option<&str>) -> Vec<String> {
    spec.map(|s| {
        s.split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect()
    })
    .unwrap_or_default()
}
