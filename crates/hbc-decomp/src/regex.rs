// Hermes regex bytecode: the program a RegExp table entry holds --
// plan_guides/01_read/unmodeled_regions/PLAN.md P3.
//
// Each entry is a 6-byte `RegexBytecodeHeader` (u16 markedCount, u16 loopCount,
// u8 syntaxFlags, u8 constraints) followed by a stream of packed instructions,
// one opcode byte then the fields of its struct, from `RegexBytecode.h`; the
// opcodes are numbered in `RegexOpcodes.def` order. Both files are identical at
// v96, v98 and v99, so one decoder serves every supported version. Jump targets
// are offsets into the instruction stream, the header excluded.
//
// Decoding is strict: an unknown opcode, an instruction that runs past the end,
// or bytes left over are errors, so a stream that decodes was read whole.
// `check` goes further and holds the program to its header -- jump targets on
// instruction boundaries, capture groups within markedCount, loop ids within
// loopCount -- which is what makes a decode on real data evidence the format
// model is right, rather than merely that the bytes did not run out.

use crate::error::{Error, Result};

/// `RegexBytecodeHeader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegexHeader {
    /// Capture groups. The bytecode numbers them from 0 (the header comment on
    /// `BeginMarkedSubexpressionInsn` says 1, but compiled streams use 0 for the
    /// first group -- `/^nope-(\d+)$/` has markedCount 1 and group 0).
    pub marked_count: u16,
    pub loop_count: u16,
    /// `SyntaxFlags::toByte`: bit 0 up, `i g m u s y d`.
    pub syntax_flags: u8,
    /// `MatchConstraintFlags`: NonASCII, AnchoredAtStart, NonEmpty.
    pub constraints: u8,
}

pub const REGEX_HEADER_LEN: usize = 6;

/// One regex instruction. Jump targets are stream offsets (header excluded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegexInsn {
    Goal,
    LeftAnchor,
    RightAnchor,
    MatchAny,
    U16MatchAny,
    MatchAnyButNewline,
    U16MatchAnyButNewline,
    MatchChar8(u8),
    MatchChar16(u16),
    U16MatchChar32(u32),
    MatchNChar8(Vec<u8>),
    MatchNCharICase8(Vec<u8>),
    MatchCharICase8(u8),
    MatchCharICase16(u16),
    U16MatchCharICase32(u32),
    Alternation {
        secondary: u32,
        primary_constraints: u8,
        secondary_constraints: u8,
    },
    Jump32(u32),
    Bracket(Bracket),
    U16Bracket(Bracket),
    BeginMarkedSubexpression(u16),
    EndMarkedSubexpression(u16),
    BackRef(u16),
    WordBoundary {
        invert: bool,
    },
    Lookaround {
        invert: bool,
        forwards: bool,
        constraints: u8,
        mexp_begin: u16,
        mexp_end: u16,
        continuation: u32,
    },
    BeginLoop {
        loop_id: u32,
        min: u32,
        max: u32,
        mexp_begin: u16,
        mexp_end: u16,
        greedy: bool,
        constraints: u8,
        not_taken: u32,
    },
    EndLoop(u32),
    BeginSimpleLoop {
        constraints: u8,
        not_taken: u32,
    },
    EndSimpleLoop(u32),
    Width1Loop {
        loop_id: u32,
        min: u32,
        max: u32,
        greedy: bool,
        not_taken: u32,
    },
}

/// A bracket `[...]`: its ranges (inclusive) and character classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bracket {
    pub negate: bool,
    /// `CharacterClass::Type` bits: 1 = \d, 2 = \s, 4 = \w.
    pub positive_classes: u8,
    /// The same bits for \D, \S, \W.
    pub negative_classes: u8,
    pub ranges: Vec<(u32, u32)>,
}

/// A decoded regex: its header and its instructions with their stream offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexProgram {
    pub header: RegexHeader,
    pub insns: Vec<(u32, RegexInsn)>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.at + N;
        let b = self.bytes.get(self.at..end).ok_or_else(|| {
            Error::Parse(format!(
                "regex: instruction runs past the end at {}",
                self.at
            ))
        })?;
        self.at = end;
        Ok(b.try_into().unwrap())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }
    fn chars(&mut self, n: usize) -> Result<Vec<u8>> {
        (0..n).map(|_| self.u8()).collect()
    }
}

/// Decode one RegExp table entry. Every byte must belong to an instruction.
pub fn decode_regex(bytes: &[u8]) -> Result<RegexProgram> {
    if bytes.len() < REGEX_HEADER_LEN {
        return Err(Error::Parse(format!(
            "regex: {} bytes, shorter than the {REGEX_HEADER_LEN}-byte header",
            bytes.len()
        )));
    }
    let header = RegexHeader {
        marked_count: u16::from_le_bytes([bytes[0], bytes[1]]),
        loop_count: u16::from_le_bytes([bytes[2], bytes[3]]),
        syntax_flags: bytes[4],
        constraints: bytes[5],
    };
    let mut r = Reader {
        bytes: &bytes[REGEX_HEADER_LEN..],
        at: 0,
    };
    let mut insns = Vec::new();
    while r.at < r.bytes.len() {
        let offset = r.at as u32;
        let op = r.u8()?;
        let insn = match op {
            0 => RegexInsn::Goal,
            1 => RegexInsn::LeftAnchor,
            2 => RegexInsn::RightAnchor,
            3 => RegexInsn::MatchAny,
            4 => RegexInsn::U16MatchAny,
            5 => RegexInsn::MatchAnyButNewline,
            6 => RegexInsn::U16MatchAnyButNewline,
            7 => RegexInsn::MatchChar8(r.u8()?),
            8 => RegexInsn::MatchChar16(r.u16()?),
            9 => RegexInsn::U16MatchChar32(r.u32()?),
            10 => {
                let n = r.u8()? as usize;
                RegexInsn::MatchNChar8(r.chars(n)?)
            }
            11 => {
                let n = r.u8()? as usize;
                RegexInsn::MatchNCharICase8(r.chars(n)?)
            }
            12 => RegexInsn::MatchCharICase8(r.u8()?),
            13 => RegexInsn::MatchCharICase16(r.u16()?),
            14 => RegexInsn::U16MatchCharICase32(r.u32()?),
            15 => RegexInsn::Alternation {
                secondary: r.u32()?,
                primary_constraints: r.u8()?,
                secondary_constraints: r.u8()?,
            },
            16 => RegexInsn::Jump32(r.u32()?),
            17 | 18 => {
                let count = r.u32()? as usize;
                let bits = r.u8()?;
                let ranges = (0..count)
                    .map(|_| Ok((r.u32()?, r.u32()?)))
                    .collect::<Result<Vec<_>>>()?;
                let b = Bracket {
                    negate: bits & 1 != 0,
                    positive_classes: (bits >> 1) & 7,
                    negative_classes: (bits >> 4) & 7,
                    ranges,
                };
                if op == 17 {
                    RegexInsn::Bracket(b)
                } else {
                    RegexInsn::U16Bracket(b)
                }
            }
            19 => RegexInsn::BeginMarkedSubexpression(r.u16()?),
            20 => RegexInsn::EndMarkedSubexpression(r.u16()?),
            21 => RegexInsn::BackRef(r.u16()?),
            22 => RegexInsn::WordBoundary { invert: r.bool()? },
            23 => RegexInsn::Lookaround {
                invert: r.bool()?,
                forwards: r.bool()?,
                constraints: r.u8()?,
                mexp_begin: r.u16()?,
                mexp_end: r.u16()?,
                continuation: r.u32()?,
            },
            24 => RegexInsn::BeginLoop {
                loop_id: r.u32()?,
                min: r.u32()?,
                max: r.u32()?,
                mexp_begin: r.u16()?,
                mexp_end: r.u16()?,
                greedy: r.bool()?,
                constraints: r.u8()?,
                not_taken: r.u32()?,
            },
            25 => RegexInsn::EndLoop(r.u32()?),
            26 => RegexInsn::BeginSimpleLoop {
                constraints: r.u8()?,
                not_taken: r.u32()?,
            },
            27 => RegexInsn::EndSimpleLoop(r.u32()?),
            28 => RegexInsn::Width1Loop {
                loop_id: r.u32()?,
                min: r.u32()?,
                max: r.u32()?,
                greedy: r.bool()?,
                not_taken: r.u32()?,
            },
            other => {
                return Err(Error::Parse(format!(
                    "regex: unknown opcode {other} at {offset}"
                )))
            }
        };
        insns.push((offset, insn));
    }
    Ok(RegexProgram { header, insns })
}

impl RegexProgram {
    /// Hold the program to its header: every jump lands on an instruction (or
    /// the end), capture groups stay within `marked_count`, loop ids within
    /// `loop_count`, and the stream ends with `Goal`.
    pub fn check(&self) -> Result<()> {
        let end = self.insns.last().map_or(0, |(o, i)| o + width(i));
        let starts: std::collections::BTreeSet<u32> =
            self.insns.iter().map(|(o, _)| *o).chain([end]).collect();
        let bad = |what: String| Err(Error::Parse(format!("regex: {what}")));
        let (marked, loops) = (self.header.marked_count, self.header.loop_count as u32);
        for (at, insn) in &self.insns {
            for t in targets(insn) {
                if !starts.contains(&t) {
                    return bad(format!("jump at {at} to {t}, not an instruction"));
                }
            }
            let mexps: Vec<u16> = match insn {
                RegexInsn::BeginMarkedSubexpression(m)
                | RegexInsn::EndMarkedSubexpression(m)
                | RegexInsn::BackRef(m) => vec![*m],
                _ => vec![],
            };
            if let Some(m) = mexps.into_iter().find(|&m| m >= marked) {
                return bad(format!("capture group {m} at {at}, header says {marked}"));
            }
            match insn {
                RegexInsn::BeginLoop { loop_id, .. } | RegexInsn::Width1Loop { loop_id, .. }
                    if *loop_id >= loops =>
                {
                    return bad(format!("loop {loop_id} at {at}, header says {loops}"));
                }
                _ => {}
            }
        }
        match self.insns.last() {
            Some((_, RegexInsn::Goal)) => Ok(()),
            _ => bad("the stream does not end with Goal".into()),
        }
    }

    /// One instruction per line, `offset  Mnemonic operands`.
    pub fn listing(&self) -> String {
        self.insns
            .iter()
            .map(|(o, i)| format!("{o:04}  {}\n", render(i)))
            .collect()
    }
}

/// The flag letters of a `syntaxFlags` byte, bit 0 up: `i g m u s y d`.
pub fn flags_text(flags: u8) -> String {
    "igmusyd"
        .chars()
        .enumerate()
        .filter(|(bit, _)| flags & (1 << bit) != 0)
        .map(|(_, c)| c)
        .collect()
}

fn width(insn: &RegexInsn) -> u32 {
    use RegexInsn::*;
    match insn {
        Goal
        | LeftAnchor
        | RightAnchor
        | MatchAny
        | U16MatchAny
        | MatchAnyButNewline
        | U16MatchAnyButNewline => 1,
        MatchChar8(_) | MatchCharICase8(_) | WordBoundary { .. } => 2,
        MatchChar16(_)
        | MatchCharICase16(_)
        | BeginMarkedSubexpression(_)
        | EndMarkedSubexpression(_)
        | BackRef(_) => 3,
        U16MatchChar32(_) | U16MatchCharICase32(_) | Jump32(_) | EndLoop(_) | EndSimpleLoop(_) => 5,
        MatchNChar8(c) | MatchNCharICase8(c) => 2 + c.len() as u32,
        Alternation { .. } => 7,
        Bracket(b) | U16Bracket(b) => 6 + 8 * b.ranges.len() as u32,
        Lookaround { .. } => 12,
        BeginLoop { .. } => 23,
        BeginSimpleLoop { .. } => 6,
        Width1Loop { .. } => 18,
    }
}

fn targets(insn: &RegexInsn) -> Vec<u32> {
    use RegexInsn::*;
    match insn {
        Alternation { secondary, .. } => vec![*secondary],
        Jump32(t) | EndLoop(t) | EndSimpleLoop(t) => vec![*t],
        Lookaround { continuation, .. } => vec![*continuation],
        BeginLoop { not_taken, .. }
        | BeginSimpleLoop { not_taken, .. }
        | Width1Loop { not_taken, .. } => vec![*not_taken],
        _ => vec![],
    }
}

fn ch(c: u32) -> String {
    match char::from_u32(c) {
        Some(ch) if !ch.is_control() && c < 0x7f => format!("'{ch}'"),
        _ => format!("U+{c:04X}"),
    }
}

fn text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| ch(b as u32))
        .collect::<Vec<_>>()
        .join(" ")
}

fn bracket(b: &Bracket) -> String {
    let mut parts: Vec<String> = b
        .ranges
        .iter()
        .map(|&(s, e)| {
            if s == e {
                ch(s)
            } else {
                format!("{}-{}", ch(s), ch(e))
            }
        })
        .collect();
    for (bits, names) in [
        (b.positive_classes, ["\\d", "\\s", "\\w"]),
        (b.negative_classes, ["\\D", "\\S", "\\W"]),
    ] {
        for (i, n) in names.iter().enumerate() {
            if bits & (1 << i) != 0 {
                parts.push((*n).to_string());
            }
        }
    }
    format!("[{}{}]", if b.negate { "^" } else { "" }, parts.join(" "))
}

fn count(n: u32) -> String {
    if n == u32::MAX {
        "inf".into()
    } else {
        n.to_string()
    }
}

fn render(insn: &RegexInsn) -> String {
    use RegexInsn::*;
    match insn {
        Goal => "Goal".into(),
        LeftAnchor => "LeftAnchor".into(),
        RightAnchor => "RightAnchor".into(),
        MatchAny => "MatchAny".into(),
        U16MatchAny => "U16MatchAny".into(),
        MatchAnyButNewline => "MatchAnyButNewline".into(),
        U16MatchAnyButNewline => "U16MatchAnyButNewline".into(),
        MatchChar8(c) => format!("MatchChar8 {}", ch(*c as u32)),
        MatchChar16(c) => format!("MatchChar16 {}", ch(*c as u32)),
        U16MatchChar32(c) => format!("U16MatchChar32 {}", ch(*c)),
        MatchNChar8(c) => format!("MatchNChar8 {}", text(c)),
        MatchNCharICase8(c) => format!("MatchNCharICase8 {}", text(c)),
        MatchCharICase8(c) => format!("MatchCharICase8 {}", ch(*c as u32)),
        MatchCharICase16(c) => format!("MatchCharICase16 {}", ch(*c as u32)),
        U16MatchCharICase32(c) => format!("U16MatchCharICase32 {}", ch(*c)),
        Alternation { secondary, .. } => format!("Alternation else {secondary:04}"),
        Jump32(t) => format!("Jump32 {t:04}"),
        Bracket(b) => format!("Bracket {}", bracket(b)),
        U16Bracket(b) => format!("U16Bracket {}", bracket(b)),
        BeginMarkedSubexpression(m) => format!("BeginMarkedSubexpression {m}"),
        EndMarkedSubexpression(m) => format!("EndMarkedSubexpression {m}"),
        BackRef(m) => format!("BackRef {m}"),
        WordBoundary { invert } => if *invert {
            "WordBoundary \\B"
        } else {
            "WordBoundary \\b"
        }
        .into(),
        Lookaround {
            invert,
            forwards,
            continuation,
            ..
        } => format!(
            "Lookaround {}{} then {continuation:04}",
            if *forwards { "ahead" } else { "behind" },
            if *invert { " (negative)" } else { "" }
        ),
        BeginLoop {
            loop_id,
            min,
            max,
            greedy,
            not_taken,
            ..
        } => format!(
            "BeginLoop #{loop_id} {{{min},{}}}{} exit {not_taken:04}",
            count(*max),
            if *greedy { "" } else { " lazy" }
        ),
        EndLoop(t) => format!("EndLoop {t:04}"),
        BeginSimpleLoop { not_taken, .. } => format!("BeginSimpleLoop exit {not_taken:04}"),
        EndSimpleLoop(t) => format!("EndSimpleLoop {t:04}"),
        Width1Loop {
            loop_id,
            min,
            max,
            greedy,
            not_taken,
        } => format!(
            "Width1Loop #{loop_id} {{{min},{}}}{} exit {not_taken:04}",
            count(*max),
            if *greedy { "" } else { " lazy" }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::BytecodeFile;
    use std::collections::BTreeSet;

    fn entries(fixture: &str) -> Vec<Vec<u8>> {
        let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
        let file = BytecodeFile::parse_auto(&std::fs::read(path).unwrap()).unwrap();
        file.reg_exp_table
            .iter()
            .map(|e| {
                file.reg_exp_storage[e.offset as usize..(e.offset + e.length) as usize].to_vec()
            })
            .collect()
    }

    // Production bundles use 22 of the 29 opcodes; `regexp_ops.js` is built to
    // produce the rest. Every entry must decode whole and check.
    #[test]
    fn every_opcode_decodes_and_checks() {
        let mut seen = BTreeSet::new();
        for (i, entry) in entries("regexp_ops.v96.hbc").iter().enumerate() {
            let prog = decode_regex(entry).unwrap_or_else(|e| panic!("entry {i}: {e}"));
            prog.check()
                .unwrap_or_else(|e| panic!("entry {i}: {e}\n{}", prog.listing()));
            for (_, insn) in &prog.insns {
                seen.insert(
                    format!("{insn:?}")
                        .split(['(', ' ', '{'])
                        .next()
                        .unwrap()
                        .to_string(),
                );
            }
        }
        assert_eq!(seen.len(), 29, "opcodes seen: {seen:?}");
    }

    #[test]
    fn a_listing_reads_like_its_pattern() {
        // /^nope-(\d+)$/i, case-folded to upper case as hermesc stores it.
        let prog = decode_regex(&entries("regexp_donor.v96.hbc")[0]).unwrap();
        assert_eq!(
            (
                prog.header.marked_count,
                prog.header.loop_count,
                flags_text(prog.header.syntax_flags)
            ),
            (1, 1, "i".to_string())
        );
        assert_eq!(
            prog.listing(),
            "0000  LeftAnchor\n\
             0001  MatchNCharICase8 'N' 'O' 'P' 'E' '-'\n\
             0008  BeginMarkedSubexpression 0\n\
             0011  Width1Loop #0 {1,inf} exit 0035\n\
             0029  Bracket [\\d]\n\
             0035  EndMarkedSubexpression 0\n\
             0038  RightAnchor\n\
             0039  Goal\n"
        );
    }

    #[test]
    fn a_stream_that_does_not_decode_whole_is_an_error() {
        let header = [0u8, 0, 0, 0, 0, 0];
        let with = |stream: &[u8]| [&header[..], stream].concat();
        assert!(decode_regex(&header[..4]).is_err(), "short header");
        let err = decode_regex(&with(&[16, 1, 2])).unwrap_err().to_string();
        assert!(err.contains("runs past the end"), "{err}");
        let err = decode_regex(&with(&[29])).unwrap_err().to_string();
        assert!(err.contains("unknown opcode 29"), "{err}");
    }

    #[test]
    fn check_holds_a_program_to_its_header() {
        let decoded =
            |header: [u8; 6], stream: &[u8]| decode_regex(&[&header[..], stream].concat()).unwrap();
        // Jump32 into the middle of itself, then Goal.
        let err = decoded([0; 6], &[16, 2, 0, 0, 0, 0])
            .check()
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an instruction"), "{err}");
        // Capture group 1 with markedCount 1.
        let err = decoded([1, 0, 0, 0, 0, 0], &[19, 1, 0, 0])
            .check()
            .unwrap_err()
            .to_string();
        assert!(err.contains("capture group 1"), "{err}");
        // No Goal.
        let err = decoded([0; 6], &[1]).check().unwrap_err().to_string();
        assert!(err.contains("Goal"), "{err}");
        decoded([0; 6], &[1, 0]).check().expect("LeftAnchor, Goal");
    }
}
