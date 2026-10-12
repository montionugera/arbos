//! Exact-value redaction of secret environment values.
//!
//! Arbos hands each worker its credentials through the process environment
//! (`GH_TOKEN`, `OP_SERVICE_ACCOUNT_TOKEN`, provider API keys). Pattern-based
//! masking cannot recognise an opaque value, so this module collects the
//! values of sensitive-looking variables once and replaces every exact
//! occurrence in tool output with `[redacted:<VAR_NAME>]`.
//!
//! Values are never logged; a debug trace records only the name and length.

const std = @import("std");
const io_mod = @import("io.zig");
const debug_trace = @import("debug_trace.zig");

const Allocator = std.mem.Allocator;

pub const Secret = struct {
    name: []const u8,
    value: []const u8,
};

/// Shorter values are too likely to appear in ordinary text to redact.
pub const min_value_len = 12;

const sensitive_name_fragments = [_][]const u8{ "SECRET", "PASSWORD", "TOKEN", "API_KEY" };
const sensitive_name_suffixes = [_][]const u8{
    "_TOKEN",
    "_API_KEY",
    "_SECRET",
    "_SECRET_ACCESS_KEY",
    "_PASSWORD",
    "_PRIVATE_KEY",
};

// Collected once per process; the list is read-only after initialisation.
var state_mutex: std.Io.Mutex = .init;
var collected: ?[]const Secret = null;

/// Returns true when an environment variable name looks like it holds a secret.
pub fn isSensitiveName(name: []const u8) bool {
    for (sensitive_name_suffixes) |suffix| {
        if (std.ascii.endsWithIgnoreCase(name, suffix)) return true;
    }
    for (sensitive_name_fragments) |fragment| {
        if (containsIgnoreCase(name, fragment)) return true;
    }
    return false;
}

fn containsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len > haystack.len) return false;
    var offset: usize = 0;
    while (offset + needle.len <= haystack.len) : (offset += 1) {
        if (std.ascii.startsWithIgnoreCase(haystack[offset..], needle)) return true;
    }
    return false;
}

/// Collects the sensitive entries of `map`, longest value first. The caller
/// owns the returned slice and its strings.
pub fn collectFromMap(alloc: Allocator, map: *const std.process.Environ.Map) Allocator.Error![]Secret {
    var list: std.ArrayList(Secret) = .empty;
    errdefer freeSecrets(alloc, list.items);
    errdefer list.deinit(alloc);

    var iterator = map.iterator();
    while (iterator.next()) |entry| {
        const name = entry.key_ptr.*;
        const value = entry.value_ptr.*;
        if (value.len < min_value_len or !isSensitiveName(name)) continue;
        const owned_name = try alloc.dupe(u8, name);
        errdefer alloc.free(owned_name);
        const owned_value = try alloc.dupe(u8, value);
        errdefer alloc.free(owned_value);
        try list.append(alloc, .{ .name = owned_name, .value = owned_value });
        debug_trace.logf("core", "secret env collected name={s} value_len={d}", .{ name, value.len });
    }
    const secrets = try list.toOwnedSlice(alloc);
    sortLongestFirst(secrets);
    return secrets;
}

fn freeSecrets(alloc: Allocator, secrets: []const Secret) void {
    for (secrets) |secret| {
        alloc.free(secret.name);
        alloc.free(secret.value);
    }
}

fn sortLongestFirst(secrets: []Secret) void {
    std.mem.sort(Secret, secrets, {}, struct {
        fn lessThan(_: void, a: Secret, b: Secret) bool {
            return a.value.len > b.value.len;
        }
    }.lessThan);
}

/// Returns the process-wide secret list, collecting it on first use.
pub fn collectedSecrets() []const Secret {
    const zio = io_mod.getIo();
    state_mutex.lockUncancelable(zio);
    defer state_mutex.unlock(zio);
    if (collected) |list| return list;
    collected = collectFromProcess();
    return collected.?;
}

fn collectFromProcess() []const Secret {
    const alloc = std.heap.c_allocator;
    var map = io_mod.cloneEnvironMap(alloc) catch return &.{};
    defer map.deinit();
    return collectFromMap(alloc, &map) catch &.{};
}

/// Replaces the process list for tests. The caller keeps ownership of
/// `list`, which must outlive the override; pass null to restore collection
/// from the real environment on the next call.
pub fn overrideForTesting(list: ?[]const Secret) void {
    const zio = io_mod.getIo();
    state_mutex.lockUncancelable(zio);
    defer state_mutex.unlock(zio);
    collected = list;
}

const Match = struct {
    start: usize,
    end: usize,
    name: []const u8,
};

fn matchStartsBefore(_: void, a: Match, b: Match) bool {
    return a.start < b.start;
}

/// Replaces every exact occurrence of a collected secret value in `text`
/// with `[redacted:<VAR_NAME>]`. Returns `text` itself (same pointer) when
/// nothing matched, so callers can skip the free. Overlapping occurrences
/// are redacted as one span labelled with the longest value.
pub fn redactSecretValues(alloc: Allocator, text: []const u8) Allocator.Error![]const u8 {
    return redactWith(alloc, text, collectedSecrets());
}

fn redactWith(alloc: Allocator, text: []const u8, list: []const Secret) Allocator.Error![]const u8 {
    var matches: std.ArrayList(Match) = .empty;
    defer matches.deinit(alloc);
    for (list) |secret| {
        if (secret.value.len == 0) continue;
        var from: usize = 0;
        while (std.mem.findPos(u8, text, from, secret.value)) |start| {
            try matches.append(alloc, .{
                .start = start,
                .end = start + secret.value.len,
                .name = secret.name,
            });
            from = start + secret.value.len;
        }
    }
    if (matches.items.len == 0) return text;
    // Stable, so among same-start matches the longest value (listed first)
    // stays ahead and labels the merged span.
    std.mem.sort(Match, matches.items, {}, matchStartsBefore);

    var out: std.Io.Writer.Allocating = .init(alloc);
    defer out.deinit();
    var cursor: usize = 0;
    var index: usize = 0;
    while (index < matches.items.len) {
        var span = matches.items[index];
        var label_len = span.end - span.start;
        index += 1;
        while (index < matches.items.len and matches.items[index].start < span.end) : (index += 1) {
            const next = matches.items[index];
            const next_len = next.end - next.start;
            if (next_len > label_len) {
                span.name = next.name;
                label_len = next_len;
            }
            span.end = @max(span.end, next.end);
        }
        out.writer.writeAll(text[cursor..span.start]) catch return error.OutOfMemory;
        out.writer.print("[redacted:{s}]", .{span.name}) catch return error.OutOfMemory;
        debug_trace.logf("core", "secret env value redacted name={s} bytes={d}", .{ span.name, span.end - span.start });
        cursor = span.end;
    }
    out.writer.writeAll(text[cursor..]) catch return error.OutOfMemory;
    return out.toOwnedSlice() catch return error.OutOfMemory;
}

test "isSensitiveName matches suffixes and fragments case-insensitively" {
    try std.testing.expect(isSensitiveName("GH_TOKEN"));
    try std.testing.expect(isSensitiveName("OP_SERVICE_ACCOUNT_TOKEN"));
    try std.testing.expect(isSensitiveName("ENGY_API_KEY"));
    try std.testing.expect(isSensitiveName("AWS_SECRET_ACCESS_KEY"));
    try std.testing.expect(isSensitiveName("db_password"));
    try std.testing.expect(isSensitiveName("SSH_PRIVATE_KEY"));
    try std.testing.expect(isSensitiveName("MySecretThing"));
    try std.testing.expect(!isSensitiveName("PATH"));
    try std.testing.expect(!isSensitiveName("HOME"));
    try std.testing.expect(!isSensitiveName("PRIVATE_KEY_PATH"));
}

test "collectFromMap keeps sensitive values of at least twelve bytes, longest first" {
    const alloc = std.testing.allocator;
    var map = std.process.Environ.Map.init(alloc);
    defer map.deinit();
    try map.put("GH_TOKEN", "ghp_abcdefghijklmnop");
    try map.put("SHORT_TOKEN", "abcdef");
    try map.put("ENGY_API_KEY", "engy-0123456789abcdef-long");
    try map.put("HOME", "/home/someone-with-a-long-path");

    const list = try collectFromMap(alloc, &map);
    defer {
        freeSecrets(alloc, list);
        alloc.free(list);
    }

    try std.testing.expectEqual(@as(usize, 2), list.len);
    try std.testing.expectEqualStrings("ENGY_API_KEY", list[0].name);
    try std.testing.expectEqualStrings("GH_TOKEN", list[1].name);
}

test "redactSecretValues replaces repeated occurrences and labels the variable" {
    const alloc = std.testing.allocator;
    const token = "ghp_abcdefghijklmnop";
    const list = [_]Secret{.{ .name = "GH_TOKEN", .value = token }};
    overrideForTesting(&list);
    defer overrideForTesting(null);

    const input = "first " ++ token ++ " middle unrelated-string " ++ token;
    const redacted = try redactSecretValues(alloc, input);
    defer if (redacted.ptr != input.ptr) alloc.free(redacted);

    try std.testing.expectEqualStrings(
        "first [redacted:GH_TOKEN] middle unrelated-string [redacted:GH_TOKEN]",
        redacted,
    );
    try std.testing.expect(std.mem.find(u8, redacted, token) == null);
}

test "redactSecretValues returns the same slice when nothing matches" {
    const alloc = std.testing.allocator;
    const list = [_]Secret{.{ .name = "GH_TOKEN", .value = "ghp_abcdefghijklmnop" }};
    overrideForTesting(&list);
    defer overrideForTesting(null);

    const input = "nothing secret here";
    const redacted = try redactSecretValues(alloc, input);
    try std.testing.expect(redacted.ptr == input.ptr);
    try std.testing.expectEqual(input.len, redacted.len);
}

test "redactSecretValues merges overlapping values under the longest name" {
    const alloc = std.testing.allocator;
    const list = [_]Secret{
        .{ .name = "LONG_TOKEN", .value = "CDEFGHIJKLMNOPQRS" },
        .{ .name = "SHORT_TOKEN", .value = "ABCDEFGHIJKLMN" },
    };
    overrideForTesting(&list);
    defer overrideForTesting(null);

    const input = "<ABCDEFGHIJKLMNOPQRS>";
    const redacted = try redactSecretValues(alloc, input);
    defer if (redacted.ptr != input.ptr) alloc.free(redacted);
    try std.testing.expectEqualStrings("<[redacted:LONG_TOKEN]>", redacted);

    const nested = "<CDEFGHIJKLMNOPQRS|ABCDEFGHIJKLMN>";
    const nested_redacted = try redactSecretValues(alloc, nested);
    defer if (nested_redacted.ptr != nested.ptr) alloc.free(nested_redacted);
    try std.testing.expectEqualStrings("<[redacted:LONG_TOKEN]|[redacted:SHORT_TOKEN]>", nested_redacted);
}

test "redactSecretValues handles adjacent occurrences" {
    const alloc = std.testing.allocator;
    const token = "ghp_abcdefghijklmnop";
    const list = [_]Secret{.{ .name = "GH_TOKEN", .value = token }};
    overrideForTesting(&list);
    defer overrideForTesting(null);

    const input = token ++ token;
    const redacted = try redactSecretValues(alloc, input);
    defer if (redacted.ptr != input.ptr) alloc.free(redacted);
    try std.testing.expectEqualStrings("[redacted:GH_TOKEN][redacted:GH_TOKEN]", redacted);
}
