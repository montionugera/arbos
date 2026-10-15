// Hold-to-record voice memo: AVAudioRecorder to .m4a with live metering for
// the waveform in the composer. The result is sent as an audio attachment.

import AVFoundation
import Foundation

@MainActor
final class MemoRecorder: ObservableObject {
    @Published private(set) var recording = false
    @Published private(set) var seconds: Double = 0
    @Published private(set) var levels: [Float] = Array(repeating: 0.05, count: 28)
    @Published var permissionDenied = false

    private var recorder: AVAudioRecorder?
    private var timer: Timer?
    private var startedAt: Date?
    private var url: URL?

    func start() {
        let session = AVAudioSession.sharedInstance()
        session.requestRecordPermission { [weak self] granted in
            Task { @MainActor in
                guard let self else { return }
                if !granted { self.permissionDenied = true; return }
                self.begin()
            }
        }
    }

    private func begin() {
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker])
        try? session.setActive(true)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("memo-\(Int(Date().timeIntervalSince1970)).m4a")
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
            AVSampleRateKey: 44100,
            AVNumberOfChannelsKey: 1,
            AVEncoderAudioQualityKey: AVAudioQuality.medium.rawValue,
        ]
        guard let rec = try? AVAudioRecorder(url: url, settings: settings) else { return }
        rec.isMeteringEnabled = true
        rec.record()
        recorder = rec
        self.url = url
        startedAt = Date()
        seconds = 0
        recording = true
        timer = Timer.scheduledTimer(withTimeInterval: 0.08, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
    }

    private func tick() {
        guard let rec = recorder, let startedAt else { return }
        rec.updateMeters()
        let db = rec.averagePower(forChannel: 0)
        let level = max(0.05, min(1, (db + 50) / 50))
        levels.removeFirst()
        levels.append(level)
        seconds = Date().timeIntervalSince(startedAt)
    }

    /// Stops and returns the file data when the memo lasted at least a second.
    func stop() -> (name: String, data: Data)? {
        timer?.invalidate()
        timer = nil
        recorder?.stop()
        recording = false
        defer { recorder = nil; url = nil }
        guard let url, seconds >= 1, let data = try? Data(contentsOf: url) else { return nil }
        let f = DateFormatter()
        f.dateFormat = "MMM d HH.mm"
        return ("Voice memo \(f.string(from: Date())).m4a", data)
    }

    func cancel() {
        timer?.invalidate()
        timer = nil
        recorder?.stop()
        recorder = nil
        recording = false
        if let url { try? FileManager.default.removeItem(at: url) }
        url = nil
    }

    var timeLabel: String {
        let s = Int(seconds)
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}
