//! Inter-byte idle budget for streaming model responses.
//!
//! A provider that returns `200 OK` and then stops sending bytes is
//! indistinguishable from a slow one unless something measures the silence.
//! `Budget` tracks the monotonic time of the last byte received from the
//! response body; the gateway cancel watcher aborts the connection when
//! `now - last_byte > timeout_ms`. SSE keep-alive comments count as activity,
//! so a provider that heartbeats is never cut off while it is still thinking.
const std = @import("std");
const io_mod = @import("../core/shared/io.zig");
const atomic_value = @import("../core/mcp/atomic_value.zig");

/// Idle budget applied when `FX_STREAM_IDLE_TIMEOUT_MS` is unset or invalid.
pub const default_timeout_ms: i64 = 90_000;
/// Smallest budget an override may select; anything lower would cut off
/// ordinary providers between tokens.
pub const min_timeout_ms: i64 = 5_000;
/// Overrides the idle budget in milliseconds. `0` disables the watchdog;
/// positive values below `min_timeout_ms` are raised to it.
pub const env_var = "FX_STREAM_IDLE_TIMEOUT_MS";

/// Budget selected by the environment, or null when the watchdog is disabled.
pub fn configuredTimeoutMs() ?i64 {
    return parseTimeoutMs(io_mod.getenv(env_var));
}

/// Pure form of `configuredTimeoutMs` for the raw environment value.
pub fn parseTimeoutMs(raw: ?[]const u8) ?i64 {
    const text = std.mem.trim(u8, raw orelse return default_timeout_ms, " \t");
    if (text.len == 0) return default_timeout_ms;
    const value = std.fmt.parseInt(i64, text, 10) catch return default_timeout_ms;
    if (value == 0) return null;
    if (value < 0) return default_timeout_ms;
    return @max(value, min_timeout_ms);
}

fn nowMs() i64 {
    const now_ns = std.Io.Clock.Timestamp.now(io_mod.getIo(), .awake).raw.toNanoseconds();
    return @intCast(@divFloor(now_ns, std.time.ns_per_ms));
}

/// Sliding deadline shared between the reading thread and the watcher thread.
pub const Budget = struct {
    timeout_ms: i64,
    /// Awake-clock milliseconds of the most recent body byte. Written by the
    /// reader through `touch`, read by the watcher through `expired`.
    last_activity_ms: atomic_value.Value(i64),
    /// Set by the watcher when it aborts the connection on idle so the caller
    /// can report the failure as a timeout rather than a dropped socket.
    tripped: std.atomic.Value(bool) = .init(false),

    /// Starts the budget now: the response head counts as the first activity.
    pub fn init(timeout_ms: i64) Budget {
        return .{ .timeout_ms = timeout_ms, .last_activity_ms = .init(nowMs()) };
    }

    pub fn touch(self: *Budget) void {
        self.last_activity_ms.store(nowMs(), .seq_cst);
    }

    pub fn expiredAt(self: *const Budget, now_ms: i64) bool {
        return now_ms - self.last_activity_ms.load(.seq_cst) > self.timeout_ms;
    }

    pub fn expired(self: *const Budget) bool {
        return self.expiredAt(nowMs());
    }

    pub fn trip(self: *Budget) void {
        self.tripped.store(true, .seq_cst);
    }

    pub fn wasTripped(self: *const Budget) bool {
        return self.tripped.load(.seq_cst);
    }

    /// Maps a transport failure observed after the watcher cut the socket to
    /// `error.Timeout`; every other failure passes through unchanged.
    pub fn failure(self: *const Budget, err: anyerror) anyerror {
        return if (self.wasTripped()) error.Timeout else err;
    }
};

/// Thin `std.Io.Reader` that bumps `budget` on every read that delivers bytes
/// before handing them to the stream consumer.
pub const ActivityReader = struct {
    inner: *std.Io.Reader,
    budget: *Budget,
    interface: std.Io.Reader,

    pub fn init(inner: *std.Io.Reader, budget: *Budget, buffer: []u8) ActivityReader {
        return .{
            .inner = inner,
            .budget = budget,
            .interface = .{
                .vtable = &.{ .stream = stream, .discard = discard },
                .buffer = buffer,
                .seek = 0,
                .end = 0,
            },
        };
    }

    fn stream(r: *std.Io.Reader, w: *std.Io.Writer, limit: std.Io.Limit) std.Io.Reader.StreamError!usize {
        const self: *ActivityReader = @fieldParentPtr("interface", r);
        const n = try self.inner.stream(w, limit);
        if (n > 0) self.budget.touch();
        return n;
    }

    fn discard(r: *std.Io.Reader, limit: std.Io.Limit) std.Io.Reader.Error!usize {
        const self: *ActivityReader = @fieldParentPtr("interface", r);
        const n = try self.inner.discard(limit);
        if (n > 0) self.budget.touch();
        return n;
    }
};

test "stream idle timeout parses the environment override" {
    try std.testing.expectEqual(@as(?i64, default_timeout_ms), parseTimeoutMs(null));
    try std.testing.expectEqual(@as(?i64, default_timeout_ms), parseTimeoutMs(""));
    try std.testing.expectEqual(@as(?i64, default_timeout_ms), parseTimeoutMs("soon"));
    try std.testing.expectEqual(@as(?i64, default_timeout_ms), parseTimeoutMs("-1"));
    try std.testing.expectEqual(@as(?i64, 120_000), parseTimeoutMs(" 120000 "));
    try std.testing.expectEqual(@as(?i64, min_timeout_ms), parseTimeoutMs("1"));
    try std.testing.expectEqual(@as(?i64, null), parseTimeoutMs("0"));
}

test "stream idle budget slides with activity" {
    var budget = Budget.init(100);
    budget.last_activity_ms.store(1_000, .seq_cst);
    try std.testing.expect(!budget.expiredAt(1_100));
    try std.testing.expect(budget.expiredAt(1_101));
    budget.last_activity_ms.store(1_090, .seq_cst);
    try std.testing.expect(!budget.expiredAt(1_150));
    try std.testing.expectEqual(error.ReadFailed, budget.failure(error.ReadFailed));
    budget.trip();
    try std.testing.expectEqual(error.Timeout, budget.failure(error.ReadFailed));
}

test "activity reader records bytes and passes them through unchanged" {
    var budget = Budget.init(1_000);
    budget.last_activity_ms.store(0, .seq_cst);
    var source: std.Io.Reader = .fixed("data: hello\n\n");
    var buffer: [4]u8 = undefined;
    var tracked = ActivityReader.init(&source, &budget, &buffer);
    const alloc = std.testing.allocator;
    const body = try tracked.interface.allocRemaining(alloc, .unlimited);
    defer alloc.free(body);
    try std.testing.expectEqualStrings("data: hello\n\n", body);
    try std.testing.expect(budget.last_activity_ms.load(.seq_cst) > 0);
    try std.testing.expect(!budget.wasTripped());
}
