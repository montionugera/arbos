const std = @import("std");
const builtin = @import("builtin");
const configured_provider = @import("../core/config/configured_provider.zig");
const debug_trace = @import("../core/shared/debug_trace.zig");
const io_mod = @import("../core/shared/io.zig");
const model_capabilities = @import("../core/config/model_capabilities.zig");
const model_provider = @import("../core/config/model_provider.zig");
const profile_paths = @import("../core/shared/profile_paths.zig");
const types = @import("../core/shared/types.zig");

const Allocator = std.mem.Allocator;

pub const connection_id = "engy";
pub const base_url = "https://api.engy.ai/v1";
pub const auth_env = "ENGY_API_KEY";
pub const default_model = "glm-5.3";
pub const models_url = base_url ++ "/models";
pub const metadata_file_name = "engy-models.json";

const max_models_body_bytes: usize = 1024 * 1024;
const output_budget_cap: u32 = 32768;
const provider_limit: usize = 32;

const effort_labels = [_][]const u8{ "none", "low", "medium", "high", "xhigh", "max" };

pub fn providerId() model_provider.ProviderId {
    return model_provider.parse(connection_id).?;
}

pub fn isBuiltin(definition: *const configured_provider.Definition) bool {
    if (!std.mem.eql(u8, definition.id, connection_id)) return false;
    if (!std.mem.eql(u8, definition.base_url, base_url)) return false;
    return switch (definition.auth) {
        .none => false,
        .bearer => |env| std.mem.eql(u8, env, auth_env),
    };
}

/// Output budget stays strictly below the context window. GLM-5.3 cannot
/// disable thinking, so the cap leaves room inside max_tokens.
pub fn outputTokenBudget(context_window: u32) ?u32 {
    if (context_window < 2) return null;
    const budget = @min(context_window / 8, output_budget_cap);
    if (budget == 0) return null;
    if (budget >= context_window) return context_window - 1;
    return budget;
}

fn effortLadder() [effort_labels.len]types.ReasoningEffort {
    return .{
        types.ReasoningEffort.literal("none"),
        types.ReasoningEffort.literal("low"),
        types.ReasoningEffort.literal("medium"),
        types.ReasoningEffort.literal("high"),
        types.ReasoningEffort.literal("xhigh"),
        types.ReasoningEffort.literal("max"),
    };
}

pub fn applyEffortLadder(capabilities: *model_capabilities.Capabilities) void {
    const ladder = effortLadder();
    capabilities.supports_reasoning = true;
    capabilities.reasoning_efforts = .fromSlice(&ladder);
}

pub fn appendReasoningEfforts(alloc: Allocator, list: *std.ArrayList(types.ReasoningEffort)) Allocator.Error!void {
    const ladder = effortLadder();
    try list.appendSlice(alloc, &ladder);
}

/// Inserts the built-in connection when that id is absent. An existing `engy`
/// connection is left unchanged, including a different base URL.
pub fn ensure(alloc: Allocator, registry: *configured_provider.Registry) !void {
    if (registry.get(connection_id) != null) return;
    if (registry.definitions.len >= provider_limit) return;
    const cached = try readCacheObject(alloc);
    defer if (cached) |text| alloc.free(text);
    const metadata_json = if (cached) |text| text else "{}";
    const definition = takeBuiltinDefinition(alloc, metadata_json) catch |err| blk: {
        if (std.mem.eql(u8, metadata_json, "{}")) return err;
        break :blk try takeBuiltinDefinition(alloc, "{}");
    };
    try registry.append_owned(alloc, definition);
}

/// Fetches public `/v1/models` once the built-in connection has no context
/// window for the default model. Failure leaves startup running.
pub fn refreshMetadata(alloc: Allocator, registry: *configured_provider.Registry) void {
    if (builtin.is_test) return;
    const current = registry.get(connection_id) orelse return;
    if (!isBuiltin(current)) return;
    if (current.model(default_model)) |metadata| if (metadata.context_window != null) return;
    const body = fetchModels(alloc) catch |err| {
        debug_trace.logf("engy", "models fetch failed err={s}", .{@errorName(err)});
        return;
    };
    defer alloc.free(body);
    const object = mapModelsJson(alloc, body) catch |err| {
        debug_trace.logf("engy", "models parse failed err={s}", .{@errorName(err)});
        return;
    };
    defer alloc.free(object);
    writeCache(alloc, object) catch |err| {
        debug_trace.logf("engy", "models cache failed err={s}", .{@errorName(err)});
    };
    attachObject(alloc, registry, object) catch |err| {
        debug_trace.logf("engy", "models attach failed err={s}", .{@errorName(err)});
    };
}

pub fn mapModelsJson(alloc: Allocator, body: []const u8) ![]u8 {
    if (body.len > max_models_body_bytes) return error.EngyModelsTooLarge;
    var parsed = std.json.parseFromSlice(std.json.Value, alloc, body, .{
        .duplicate_field_behavior = .@"error",
    }) catch return error.EngyModelsMalformed;
    defer parsed.deinit();
    if (parsed.value != .object) return error.EngyModelsMalformed;
    const data = parsed.value.object.get("data") orelse return error.EngyModelsMalformed;
    if (data != .array) return error.EngyModelsMalformed;

    var seen: std.ArrayList([]const u8) = .empty;
    defer seen.deinit(alloc);
    var out: std.Io.Writer.Allocating = .init(alloc);
    errdefer out.deinit();
    try out.writer.writeByte('{');
    var count: usize = 0;
    for (data.array.items) |item| {
        if (item != .object) continue;
        const id_value = item.object.get("id") orelse continue;
        if (id_value != .string) continue;
        const id = id_value.string;
        configured_provider.validate_model_id(id) catch continue;
        if (alreadySeen(seen.items, id)) continue;
        const context_value = item.object.get("context_length") orelse continue;
        const context_window = jsonU32(context_value) orelse continue;
        const vision = if (item.object.get("input_modalities")) |modalities| hasImage(modalities) else false;
        if (count != 0) try out.writer.writeByte(',');
        try std.json.Stringify.value(id, .{}, &out.writer);
        try out.writer.print(":{{\"context_window\":{d}", .{context_window});
        if (outputTokenBudget(context_window)) |budget| {
            try out.writer.print(",\"max_output_tokens\":{d}", .{budget});
        }
        try out.writer.writeAll(",\"supports_tool_use\":true,\"supports_vision\":");
        try out.writer.writeAll(if (vision) "true" else "false");
        try out.writer.writeByte('}');
        try seen.append(alloc, id);
        count += 1;
        if (count == 256) break;
    }
    try out.writer.writeByte('}');
    return try out.toOwnedSlice();
}

fn alreadySeen(seen: []const []const u8, id: []const u8) bool {
    for (seen) |item| if (std.mem.eql(u8, item, id)) return true;
    return false;
}

fn jsonU32(value: std.json.Value) ?u32 {
    const raw: u64 = switch (value) {
        .integer => |number| if (number <= 0) return null else @intCast(number),
        .number_string => std.fmt.parseInt(u64, value.number_string, 10) catch return null,
        else => return null,
    };
    if (raw == 0 or raw > std.math.maxInt(u32)) return null;
    return @intCast(raw);
}

fn hasImage(value: std.json.Value) bool {
    if (value != .array) return false;
    for (value.array.items) |item| {
        if (item == .string and std.mem.eql(u8, item.string, "image")) return true;
    }
    return false;
}

fn takeBuiltinDefinition(alloc: Allocator, metadata_json: []const u8) !configured_provider.Definition {
    const json = try std.fmt.allocPrint(
        alloc,
        "{{\"engy\":{{\"protocol\":\"openai-chat-completions\",\"base_url\":\"{s}\",\"auth\":{{\"type\":\"bearer\",\"env\":\"{s}\"}},\"model_metadata\":{s}}}}}",
        .{ base_url, auth_env, metadata_json },
    );
    defer alloc.free(json);
    var temp = try configured_provider.Registry.parse_json(alloc, json);
    const slice = temp.definitions;
    const definition = slice[0];
    temp.definitions = &.{};
    temp.deinit(alloc);
    alloc.free(slice);
    return definition;
}

fn readCacheObject(alloc: Allocator) !?[]u8 {
    const home = profile_paths.effectiveHome() orelse return null;
    const path = try cachePath(alloc, home);
    defer alloc.free(path);
    var file = std.Io.Dir.openFileAbsolute(io_mod.getIo(), path, .{}) catch return null;
    defer file.close(io_mod.getIo());
    const bytes = io_mod.readFileToEnd(alloc, &file, max_models_body_bytes) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        else => return null,
    };
    if (!isMetadataObject(alloc, bytes)) {
        alloc.free(bytes);
        return null;
    }
    return bytes;
}

fn isMetadataObject(alloc: Allocator, object: []const u8) bool {
    if (object.len == 0 or object[0] != '{') return false;
    const definition = takeBuiltinDefinition(alloc, object) catch return false;
    var registry: configured_provider.Registry = .{};
    registry.append_owned(alloc, definition) catch return false;
    defer registry.deinit(alloc);
    return true;
}

fn cachePath(alloc: Allocator, home: []const u8) ![]u8 {
    return std.fs.path.join(alloc, &.{ home, profile_paths.root_dir_name, metadata_file_name });
}

fn writeCache(alloc: Allocator, object: []const u8) !void {
    const home = profile_paths.effectiveHome() orelse return;
    const dir = try profile_paths.rootDir(alloc, home);
    defer alloc.free(dir);
    try io_mod.makeDirRecursive(dir);
    const path = try cachePath(alloc, home);
    defer alloc.free(path);
    try io_mod.writeFileAtomic(alloc, path, object);
}

fn fetchModels(alloc: Allocator) ![]u8 {
    var client: std.http.Client = .{ .allocator = alloc, .io = io_mod.getIo() };
    defer client.deinit();
    var out: std.Io.Writer.Allocating = .init(alloc);
    errdefer out.deinit();
    var headers: std.http.Client.Request.Headers = .{};
    headers.user_agent = .{ .override = "fx" };
    const result = try client.fetch(.{
        .location = .{ .url = models_url },
        .method = .GET,
        .headers = headers,
        .response_writer = &out.writer,
        .redirect_behavior = .unhandled,
    });
    if (result.status != .ok) return error.EngyModelsHttp;
    if (out.written().len > max_models_body_bytes) return error.EngyModelsTooLarge;
    return try out.toOwnedSlice();
}

fn attachObject(alloc: Allocator, registry: *configured_provider.Registry, object: []const u8) !void {
    const current = registry.get(connection_id) orelse return;
    if (current.model_metadata.len != 0) return;
    var definition = try takeBuiltinDefinition(alloc, object);
    const stolen = definition.model_metadata;
    definition.model_metadata = &.{};
    var holder: configured_provider.Registry = .{};
    holder.append_owned(alloc, definition) catch |err| {
        freeMetadata(alloc, stolen);
        return err;
    };
    defer holder.deinit(alloc);
    registry.adopt_model_metadata(connection_id, stolen) catch |err| {
        freeMetadata(alloc, stolen);
        return err;
    };
}

fn freeMetadata(alloc: Allocator, metadata: []const configured_provider.ModelMetadata) void {
    for (metadata) |item| alloc.free(item.id);
    alloc.free(metadata);
}

test "engy default model is glm-5.3" {
    try std.testing.expectEqualStrings("glm-5.3", default_model);
}

test "engy output budget stays below the context window" {
    try std.testing.expectEqual(@as(?u32, 32768), outputTokenBudget(327680));
    try std.testing.expect(outputTokenBudget(327680).? < 327680);
    try std.testing.expectEqual(@as(?u32, null), outputTokenBudget(1));
}

test "engy maps context_length onto model metadata" {
    const alloc = std.testing.allocator;
    const object = try mapModelsJson(alloc,
        \\{"object":"list","data":[{"id":"glm-5.3","context_length":327680,"input_modalities":["text"]},{"id":"kimi-k3","context_length":1113088,"input_modalities":["image","text"]},{"id":"bad","context_length":0},{"id":"glm-5.3","context_length":1}]}
    );
    defer alloc.free(object);
    const definition = try takeBuiltinDefinition(alloc, object);
    var registry: configured_provider.Registry = .{};
    try registry.append_owned(alloc, definition);
    defer registry.deinit(alloc);
    const engy = registry.get(connection_id).?;
    const glm = engy.model(default_model).?;
    try std.testing.expectEqual(@as(?u32, 327680), glm.context_window);
    try std.testing.expectEqual(@as(?u32, 32768), glm.max_output_tokens);
    try std.testing.expectEqual(@as(?bool, false), glm.supports_vision);
    try std.testing.expectEqual(@as(?bool, true), glm.supports_tool_use);
    const kimi = engy.model("kimi-k3").?;
    try std.testing.expectEqual(@as(?bool, true), kimi.supports_vision);
    try std.testing.expect(engy.model("bad") == null);
    try std.testing.expectEqual(@as(usize, 2), engy.model_metadata.len);
}

test "engy connection stores the env name and no secret" {
    const alloc = std.testing.allocator;
    var registry: configured_provider.Registry = .{};
    defer registry.deinit(alloc);
    try ensure(alloc, &registry);
    try ensure(alloc, &registry);
    try std.testing.expectEqual(@as(usize, 1), registry.definitions.len);
    const definition = registry.get(connection_id).?;
    try std.testing.expectEqualStrings(base_url, definition.base_url);
    switch (definition.auth) {
        .bearer => |env| try std.testing.expectEqualStrings(auth_env, env),
        .none => return error.TestUnexpectedResult,
    }
}

test "engy effort ladder includes none through max" {
    var capabilities: model_capabilities.Capabilities = .{};
    applyEffortLadder(&capabilities);
    try std.testing.expect(capabilities.supports_reasoning);
    try std.testing.expectEqual(effort_labels.len, capabilities.reasoning_efforts.len);
    for (effort_labels, capabilities.reasoning_efforts.slice()) |want, got| {
        try std.testing.expectEqualStrings(want, got.label());
    }
}
