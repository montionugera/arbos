// Simulator-only hooks so a headless `simctl launch` can land on a given
// screen for screenshots. Compiled out of Release builds.

import Foundation

enum DebugHooks {
    static var openProject: Id? { value("ARBOS_OPEN_PROJECT") }
    static var openChat: Id? { value("ARBOS_OPEN_CHAT") }
    static var autoSend: String? { value("ARBOS_AUTO_SEND") }
    static var pairCode: String? { value("ARBOS_PAIR_CODE") }
    static var fixture: Bool { value("ARBOS_FIXTURE") == "1" }
    /// keyboard | settings | voice | threads | model — opened on the chat screen.
    static var show: String? { value("ARBOS_SHOW") }
    /// Scripted tour for the screen recording (fixture only).
    static var demo: Bool { value("ARBOS_DEMO") == "1" }
    /// Scripted tour against the live relay: sends ARBOS_AUTO_SEND, waits for the reply, tours the sheets.
    static var liveDemo: Bool { value("ARBOS_DEMO") == "live" }

    private static func value(_ key: String) -> String? {
        #if DEBUG
        let v = ProcessInfo.processInfo.environment[key] ?? ""
        return v.isEmpty ? nil : v
        #else
        return nil
        #endif
    }
}
