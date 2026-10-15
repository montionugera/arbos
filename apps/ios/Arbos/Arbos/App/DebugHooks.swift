// Simulator-only hooks so a headless `simctl launch` can land on a given
// screen for screenshots. Compiled out of Release builds.

import Foundation

enum DebugHooks {
    static var openProject: Id? { value("ARBOS_OPEN_PROJECT") }
    static var openChat: Id? { value("ARBOS_OPEN_CHAT") }
    static var autoSend: String? { value("ARBOS_AUTO_SEND") }
    static var pairCode: String? { value("ARBOS_PAIR_CODE") }

    private static func value(_ key: String) -> String? {
        #if DEBUG
        let v = ProcessInfo.processInfo.environment[key] ?? ""
        return v.isEmpty ? nil : v
        #else
        return nil
        #endif
    }
}
