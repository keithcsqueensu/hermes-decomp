// A function's exception-handler table across a body edit -- plan_guides/
// 06_write/RISKS.md Q3.
//
// The table is `u32 count`, then `count` entries of `{start, end, target}`, each
// a u32 offset *within the function body* (`end` exclusive). It sits in the
// FunctionInfo region after all the code, at the function's info offset aligned
// to 4 -- for a modern function, just past its large header. A body splice moves
// the table along with everything else after the code (`write::reloc`), but the
// offsets inside it are body-relative, so they are rewritten here, in the same
// buffer, before the edit is committed.
//
// The table keeps its size: a body edit may move handlers, never add or remove
// one. Adding a table would resize the function's FunctionInfo region, which no
// op does.

use crate::error::{Error, Result};
use crate::file::{BytecodeFile, ExceptionHandler};
use crate::format::{FunctionHeader, FLAG_HAS_EXCEPTION_HANDLER};

/// What a body edit does to the function's exception-handler table.
#[derive(Debug, Clone)]
pub(crate) enum HandlerUpdate {
    /// Leave it as it is. A size-changing edit to a function that has handlers
    /// is refused: nothing says where its handlers went.
    Keep,
    /// Bytes were inserted at body offset `at`: every handler offset at or past
    /// it moves by the size change. Start, end and target each name an
    /// instruction, and the instruction at `at` is the one pushed forward, so a
    /// try region that spans the insertion grows to cover it.
    Insert { at: u32 },
    /// The new body states its handlers (HASM `.handler` lines), as offsets in
    /// the new body.
    Replace(Vec<ExceptionHandler>),
}

/// Whether function `id` declares a handler table -- the parser's own gate.
pub(crate) fn has_handlers(file: &BytecodeFile, id: u32) -> bool {
    file.function_headers
        .get(id as usize)
        .is_some_and(|h| h.flags() & FLAG_HAS_EXCEPTION_HANDLER != 0)
}

/// Byte position of function `id`'s table (its `count`) in `file`'s image.
pub(crate) fn table_pos(file: &BytecodeFile, id: u32) -> Result<usize> {
    let info = match file.function_headers.get(id as usize) {
        Some(FunctionHeader::Legacy(h)) => h.info_offset,
        Some(FunctionHeader::Modern(h)) => h.info_offset,
        None => return Err(Error::Write(format!("invalid function id {id}"))),
    } as usize;
    if info == 0 {
        return Err(Error::Write(format!(
            "function {id} is flagged with an exception-handler table but has no info offset"
        )));
    }
    Ok((info + 3) & !3)
}

/// Read the table at `pos` in `buf`.
pub(crate) fn read_table(buf: &[u8], pos: usize) -> Result<Vec<ExceptionHandler>> {
    let u32_at = |at: usize| {
        buf.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| Error::Write(format!("handler table at {pos} runs past the image")))
    };
    let count = u32_at(pos)? as usize;
    (0..count)
        .map(|i| {
            let at = pos + 4 + i * 12;
            Ok(ExceptionHandler {
                start: u32_at(at)?,
                end: u32_at(at + 4)?,
                target: u32_at(at + 8)?,
            })
        })
        .collect()
}

/// Overwrite the table at `pos` with `handlers`, which must be as many as it holds.
pub(crate) fn write_table(buf: &mut [u8], pos: usize, handlers: &[ExceptionHandler]) -> Result<()> {
    let count = read_table(buf, pos)?.len();
    if count != handlers.len() {
        return Err(Error::Write(format!(
            "the function has {count} exception handler(s) and the edit gives {}: a body \
             edit can move handlers but not add or remove them",
            handlers.len()
        )));
    }
    for (i, h) in handlers.iter().enumerate() {
        let at = pos + 4 + i * 12;
        buf[at..at + 4].copy_from_slice(&h.start.to_le_bytes());
        buf[at + 4..at + 8].copy_from_slice(&h.end.to_le_bytes());
        buf[at + 8..at + 12].copy_from_slice(&h.target.to_le_bytes());
    }
    Ok(())
}

/// `old` after `delta` bytes were inserted at body offset `at`.
pub(crate) fn shifted(old: &[ExceptionHandler], at: u32, delta: i64) -> Vec<ExceptionHandler> {
    let mv = |x: u32| {
        if x >= at {
            (x as i64 + delta) as u32
        } else {
            x
        }
    };
    old.iter()
        .map(|h| ExceptionHandler {
            start: mv(h.start),
            end: mv(h.end),
            target: mv(h.target),
        })
        .collect()
}

/// Refuse handlers that do not describe a region and a landing pad inside a body
/// of `body_len` bytes.
pub(crate) fn check_within(handlers: &[ExceptionHandler], body_len: u32) -> Result<()> {
    for (i, h) in handlers.iter().enumerate() {
        if h.start > h.end || h.end > body_len || h.target >= body_len {
            return Err(Error::Write(format!(
                "exception handler {i} ({}..{} -> {}) does not fit a {body_len}-byte body",
                h.start, h.end, h.target
            )));
        }
    }
    Ok(())
}
