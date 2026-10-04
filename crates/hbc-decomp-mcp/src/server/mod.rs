// MCP server for the Hermes decompiler. Split into: `params` (tool parameter
// types), `tools_analyze` (read and analysis tools), `tools_write` (write path
// and RE tools). Each tool group builds its own router; `new` merges them.

mod bounds;
mod params;
mod tools_analyze;
mod tools_write;

use rmcp::ErrorData as McpError;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    tool_handler, ServerHandler,
};
use std::sync::Mutex;

use hbc_decomp::opcode::BytecodeFormat;
use hbc_decomp::{BytecodeFile, DecompileOptionsV2, PipelineContext};

pub(crate) struct LoadedFile {
    pub(crate) file: BytecodeFile,
    pub(crate) format: BytecodeFormat,
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) pipeline_ctx: Option<PipelineContext>,
    // Whether the cached `pipeline_ctx` was built in deep naming mode, so a request
    // that toggles deep rebuilds instead of returning the wrong-mode context.
    pub(crate) pipeline_deep: bool,
}

pub struct HermesService {
    loaded: Mutex<Option<LoadedFile>>,
    tool_router: ToolRouter<Self>,
}

impl HermesService {
    pub fn new() -> Self {
        Self {
            loaded: Mutex::new(None),
            tool_router: Self::analyze_router() + Self::write_router(),
        }
    }

    // Every tool body runs through one of these two, so this is the single
    // place where the work is moved onto a large-stack thread. The mutex guard
    // stays on the calling thread (it is not Send); only a borrow of the loaded
    // file crosses into the scoped thread, which ends before the guard drops.
    pub(crate) fn with_file<F, T>(&self, f: F) -> Result<T, McpError>
    where
        F: FnOnce(&LoadedFile) -> Result<T, McpError> + Send,
        T: Send,
    {
        let guard = self.lock();
        let loaded = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No file loaded. Use load_file first.", None)
        })?;
        run_scoped_with_large_stack(move || catch_tool_panic(|| f(loaded)))
    }

    pub(crate) fn with_file_mut<F, T>(&self, f: F) -> Result<T, McpError>
    where
        F: FnOnce(&mut LoadedFile) -> Result<T, McpError> + Send,
        T: Send,
    {
        let mut guard = self.lock();
        let loaded = guard.as_mut().ok_or_else(|| {
            McpError::invalid_params("No file loaded. Use load_file first.", None)
        })?;
        run_scoped_with_large_stack(move || catch_tool_panic(|| f(loaded)))
    }

    /// Take the loaded-file lock, recovering from poisoning rather than failing.
    ///
    /// `std::sync::Mutex` poisons when a thread panics while holding it, and every
    /// tool in this server goes through this lock. Mapping that to an error meant
    /// **one panic anywhere bricked the server for the rest of its life**: the
    /// process stayed up, `lock()` returned `Err` forever, and every subsequent
    /// tool answered `lock: poisoned` with no hint that a restart was the fix.
    ///
    /// Recovering is sound here because the data behind the lock is a *parsed,
    /// immutable-in-practice* `BytecodeFile` plus a memoised analysis context. A
    /// panic mid-read cannot leave it half-updated in a way a later read would
    /// misinterpret; the worst case is a `pipeline_ctx` that was never filled in,
    /// which `ensure_pipeline` rebuilds on demand.
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Option<LoadedFile>> {
        self.loaded.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Ceiling on a single tool response, in bytes.
///
/// **[measured]** on a shipped 16 MB React Native bundle, before any cap existed:
/// `decompile_all` returned **41,447,553 bytes**, `dump kind=strings` 5,839,550,
/// and `dump kind=functions` 3,717,753. Of the 21 read tools, exactly one took a
/// `limit`. A 41 MB response is not a degraded answer, it is a failed call that
/// also costs the caller its context window.
///
/// 256 KiB is comfortably above any single function's decompiled body and well
/// under a reasonable context budget. Callers that need more page through it.
pub(crate) const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Wrap a tool's text in a `CallToolResult`, truncating past `MAX_RESPONSE_BYTES`
/// with an explicit, actionable tail.
///
/// Truncation is stated, never silent: a clipped listing that looks complete is
/// the same class of defect as the rest of the read path's quiet degradations.
pub(crate) fn text_result(body: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(cap_text(
        body.into(),
    ))]))
}

pub(crate) fn cap_text(body: String) -> String {
    if body.len() <= MAX_RESPONSE_BYTES {
        return body;
    }
    // Cut on a char boundary, then back up to the last newline so the tail is not
    // a half-written record.
    let mut end = MAX_RESPONSE_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    let end = body[..end].rfind('\n').map(|i| i + 1).unwrap_or(end);
    let shown = &body[..end];
    let lines = shown.lines().count();
    format!(
        "{shown}\n… TRUNCATED: this response was {} bytes, capped at {}. \
         {lines} lines shown. Narrow the request (a specific function or module, \
         a `limit`/`offset` where the tool takes one) rather than treating this as \
         the complete result.\n",
        body.len(),
        MAX_RESPONSE_BYTES
    )
}

/// Run one tool body, converting a panic into a failed call instead of a dead
/// server.
///
/// A panic that escapes here unwinds through the `Mutex` guard and poisons it (see
/// `HermesService::lock`), and in a `panic = "abort"` profile it takes the process
/// down outright. Neither is an acceptable response to one malformed input, and
/// the read path is pointed at deliberately malformed input by design.
///
/// This does **not** catch stack overflow, which aborts and cannot be caught —
/// that is what the depth bound in `hbc_decomp::ir::depth` and the large worker
/// stack in `main` are for.
pub(crate) fn catch_tool_panic<F, T>(f: F) -> Result<T, McpError>
where
    F: FnOnce() -> Result<T, McpError>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(e) => {
            let detail = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            Err(McpError::internal_error(
                format!(
                    "internal error while reading this file: {detail}. \
                     The loaded file is still available; other tools should still work. \
                     This is a bug -- the read path is expected to degrade, not panic."
                ),
                None,
            ))
        }
    }
}

// Scoped counterpart of `hbc_decomp::run_with_large_stack` for closures that
// borrow the loaded file. Tokio worker threads have a ~2 MB stack; structure
// recovery and codegen on a real bundle overflow it on the calling thread.
fn run_scoped_with_large_stack<T, F>(f: F) -> T
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("hbc-mcp-tool".into())
            .stack_size(hbc_decomp::LARGE_STACK_SIZE)
            .spawn_scoped(scope, f)
            .expect("failed to spawn large-stack tool thread");
        match handle.join() {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

impl LoadedFile {
    fn ensure_pipeline(&mut self, deep: bool) -> Result<(), McpError> {
        if self.pipeline_ctx.is_none() || self.pipeline_deep != deep {
            // Reuse an on-disk analysis cache (`<file>.hdcache`) keyed by the
            // bytecode and options (deep is part of the key), so repeated sessions on
            // the same file and mode don't re-analyze.
            let cache_path = hbc_decomp::default_cache_path(std::path::Path::new(&self.path));
            let options = DecompileOptionsV2 {
                deep,
                ..DecompileOptionsV2::optimized()
            };
            let ctx = PipelineContext::build_cached(
                &self.file,
                &self.format,
                &options,
                &self.bytes,
                &cache_path,
            )
            .map_err(|e| McpError::internal_error(format!("Pipeline build error: {e}"), None))?;
            self.pipeline_ctx = Some(ctx);
            self.pipeline_deep = deep;
        }
        Ok(())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HermesService {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (InitializeResult) is #[non_exhaustive] in rmcp 2, so it
        // cannot be built with a struct literal; set fields on a default value.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Hermes bytecode decompiler for React Native apps (HBC 40 to 99). Load a .hbc file with load_file, then use the decompile, disassemble, xref and module tools to analyze. Use decompile_function for quick single function output, or decompile_function_full and decompile_module for full quality analysis with IPA naming and ESM imports and exports. For structural inspection use dump_table (kinds cjs-modules, regexp, obj-shapes, function-sources, string-kinds, sections, big-int, array-buffer), callgraph (caller to callee edges, optional DOT), and function_info (per function metadata banner).".into()
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_does_not_brick_the_service() {
        let svc = HermesService::new();

        // Poison the lock the way a panicking tool body used to: panic while the
        // guard is held. Before the fix this made every later `lock()` return
        // `Err`, so the server stayed up and answered `lock: poisoned` forever.
        let poisoner = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = svc.loaded.lock().unwrap();
            panic!("simulated tool panic");
        }));
        assert!(poisoner.is_err(), "the test must actually poison the mutex");
        assert!(
            svc.loaded.lock().is_err(),
            "sanity: std's lock really is poisoned now"
        );

        // The recovering accessor still works, and so does a real tool call
        // through it (which reports "no file loaded", not "poisoned").
        drop(svc.lock());
        let err = svc
            .with_file(|_| Ok(()))
            .expect_err("no file is loaded in this service");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("No file loaded"),
            "expected the normal no-file error, got: {msg}"
        );
        assert!(
            !msg.contains("poison"),
            "poisoning must not leak out: {msg}"
        );
    }

    #[test]
    fn catch_tool_panic_converts_a_panic_into_one_failed_call() {
        // Silence the default hook so the test output stays readable.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r: Result<(), McpError> = catch_tool_panic(|| panic!("boom: bad header"));
        std::panic::set_hook(prev);

        let msg = format!("{:?}", r.expect_err("a panic must surface as an error"));
        assert!(msg.contains("boom: bad header"), "detail is kept: {msg}");
        assert!(
            msg.contains("still available"),
            "the caller is told the session survived: {msg}"
        );
    }

    #[test]
    fn cap_text_passes_small_bodies_through_untouched() {
        let small = "line one\nline two\n".to_string();
        assert_eq!(cap_text(small.clone()), small);
    }

    #[test]
    fn cap_text_truncates_on_a_line_boundary_and_says_so() {
        let body: String = (0..200_000).map(|i| format!("line {i}\n")).collect();
        let original_len = body.len();
        assert!(original_len > MAX_RESPONSE_BYTES);

        let out = cap_text(body);
        assert!(out.contains("TRUNCATED"), "truncation must be stated");
        assert!(
            out.contains(&original_len.to_string()),
            "the real size must be reported so the caller can page"
        );
        // The kept prefix ends on a record boundary, not mid-line.
        let kept = out.split("\n… TRUNCATED").next().unwrap();
        assert!(kept.ends_with("\n") || kept.is_empty());
        assert!(kept.lines().all(|l| l.starts_with("line ")));
    }

    #[test]
    fn cap_text_handles_multibyte_boundaries() {
        // A body of 3-byte chars whose cut point lands mid-character.
        let body: String = std::iter::repeat_n('あ', MAX_RESPONSE_BYTES).collect();
        let out = cap_text(body);
        assert!(out.contains("TRUNCATED"));
        // Getting here at all means no char-boundary panic.
    }

    // decompile_all's own bound must not default looser than every other tool's.
    #[test]
    fn decompile_all_defaults_to_the_response_cap() {
        let p: super::params::DecompileAllParams = serde_json::from_str("{}").unwrap();
        assert_eq!(p.max_chars, MAX_RESPONSE_BYTES);
    }
}

#[cfg(test)]
mod stack_tests {
    use super::run_scoped_with_large_stack;

    // Recursion deep enough to blow a 2 MB stack (the frame is padded so the
    // optimizer cannot shrink it) but comfortably inside 64 MB.
    fn deep(n: u32) -> u32 {
        let pad = [n; 32];
        if n == 0 {
            return std::hint::black_box(pad)[0];
        }
        std::hint::black_box(pad);
        1 + deep(n - 1)
    }

    #[test]
    fn scoped_helper_runs_deep_recursion_and_borrows() {
        let data = [1u32, 2, 3];
        let sum = run_scoped_with_large_stack(|| data.iter().sum::<u32>() + deep(50_000));
        assert_eq!(sum, 6 + 50_000);
    }

    #[test]
    fn lib_helper_runs_deep_recursion() {
        assert_eq!(hbc_decomp::run_with_large_stack(|| deep(50_000)), 50_000);
    }

    #[test]
    fn panics_are_resumed_on_the_caller() {
        let r =
            std::panic::catch_unwind(|| run_scoped_with_large_stack(|| -> u32 { panic!("boom") }));
        assert!(r.is_err());
        let r = std::panic::catch_unwind(|| {
            hbc_decomp::run_with_large_stack(|| -> u32 { panic!("boom") })
        });
        assert!(r.is_err());
    }
}
