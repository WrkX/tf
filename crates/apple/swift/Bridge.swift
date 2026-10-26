import Foundation
import FoundationModels
import Speech
import AVFoundation
import CoreMedia

private final class Box<T>: @unchecked Sendable {
    var value: T
    init(_ value: T) { self.value = value }
}

private func blocking<T: Sendable>(_ work: @escaping @Sendable () async -> T) -> T {
    let result = Box<T?>(nil)
    let done = DispatchSemaphore(value: 0)
    Task.detached {
        result.value = await work()
        done.signal()
    }
    done.wait()
    return result.value!
}

@_cdecl("tf_string_free")
public func stringFree(_ pointer: UnsafeMutablePointer<CChar>?) {
    free(pointer)
}

@_cdecl("tf_fm_availability")
public func fmAvailability() -> Int32 {
    switch SystemLanguageModel.default.availability {
    case .available: 0
    case .unavailable(.deviceNotEligible): 1
    case .unavailable(.appleIntelligenceNotEnabled): 2
    case .unavailable(.modelNotReady): 3
    case .unavailable: 4
    }
}

@_cdecl("tf_fm_respond")
public func fmRespond(
    _ instructions: UnsafePointer<CChar>,
    _ prompt: UnsafePointer<CChar>,
    _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>
) -> Bool {
    let instructions = String(cString: instructions)
    let prompt = String(cString: prompt)
    let (ok, text): (Bool, String) = blocking {
        do {
            let session = LanguageModelSession(instructions: instructions)
            let response = try await session.respond(to: prompt)
            return (true, response.content)
        } catch {
            return (false, String(describing: error))
        }
    }
    output.pointee = strdup(text)
    return ok
}

private func fmErrorCode(_ error: Error) -> Int32 {
    let text = String(describing: error).lowercased()
    if text.contains("guardrail") || text.contains("refus") || text.contains("unsafe") {
        return 2
    }
    if text.contains("notready") || text.contains("unavailable") || text.contains("not enabled") {
        return 1
    }
    return 3
}

@_cdecl("tf_fm_respond_options")
public func fmRespondOptions(
    _ instructions: UnsafePointer<CChar>,
    _ prompt: UnsafePointer<CChar>,
    _ temperature: Double,
    _ maxTokens: Int32,
    _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>,
    _ code: UnsafeMutablePointer<Int32>
) -> Bool {
    let instructions = String(cString: instructions)
    let prompt = String(cString: prompt)
    let (ok, text, errorCode): (Bool, String, Int32) = blocking {
        let options = GenerationOptions(
            temperature: temperature,
            maximumResponseTokens: Int(maxTokens)
        )
        do {
            let session = LanguageModelSession(instructions: instructions)
            let response = try await session.respond(to: prompt, options: options)
            return (true, response.content, 0)
        } catch {
            return (false, String(describing: error), fmErrorCode(error))
        }
    }
    if !ok {
        code.pointee = errorCode
    }
    output.pointee = strdup(text)
    return ok
}

@_cdecl("tf_fm_prewarm")
public func fmPrewarm() {
    blocking {
        let session = LanguageModelSession(instructions: "")
        session.prewarm()
    }
}

public typealias TfSpeechProgress = @convention(c) (UnsafeMutableRawPointer?, Double) -> Void

private final class ProgressSink: @unchecked Sendable {
    let callback: TfSpeechProgress?
    let context: UnsafeMutableRawPointer?

    init(_ callback: TfSpeechProgress?, _ context: UnsafeMutableRawPointer?) {
        self.callback = callback
        self.context = context
    }

    func call(_ value: Double) {
        callback?(context, min(max(value, 0.0), 1.0))
    }
}

private func speechLocaleId(_ locale: Locale) -> String {
    locale.identifier.replacingOccurrences(of: "_", with: "-")
}

private func speechError(_ error: Error) -> String {
    String(describing: error)
}

@_cdecl("tf_speech_locales")
public func speechLocales(
    _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>
) -> Bool {
    let (ok, text): (Bool, String) = blocking {
        let supported = await SpeechTranscriber.supportedLocales
        let installed = await SpeechTranscriber.installedLocales
        let installedIds = Set(installed.map(speechLocaleId))
        let items: [[String: Any]] = supported.map { locale in
            let id = speechLocaleId(locale)
            return [
                "id": id,
                "name": Locale.current.localizedString(forIdentifier: id) ?? id,
                "installed": installedIds.contains(id),
            ]
        }.sorted { ($0["id"] as? String ?? "") < ($1["id"] as? String ?? "") }
        do {
            let data = try JSONSerialization.data(withJSONObject: items, options: [])
            return (true, String(decoding: data, as: UTF8.self))
        } catch {
            return (false, speechError(error))
        }
    }
    output.pointee = strdup(text)
    return ok
}

@_cdecl("tf_speech_install")
public func speechInstall(
    _ localeId: UnsafePointer<CChar>,
    _ progress: TfSpeechProgress?,
    _ context: UnsafeMutableRawPointer?,
    _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>
) -> Bool {
    let id = String(cString: localeId)
    let sink = ProgressSink(progress, context)
    let (ok, message): (Bool, String) = blocking {
        do {
            let locale = Locale(identifier: id)
            let transcriber = SpeechTranscriber(
                locale: locale,
                transcriptionOptions: [],
                reportingOptions: [],
                attributeOptions: []
            )
            guard let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) else {
                sink.call(1.0)
                return (true, "")
            }
            let monitor = Task {
                while !Task.isCancelled {
                    sink.call(request.progress.fractionCompleted)
                    try? await Task.sleep(nanoseconds: 200_000_000)
                }
            }
            try await request.downloadAndInstall()
            monitor.cancel()
            sink.call(1.0)
            return (true, "")
        } catch {
            return (false, speechError(error))
        }
    }
    output.pointee = strdup(message)
    return ok
}

private func makeBuffer(samples: ArraySlice<Float>, format: AVAudioFormat) -> AVAudioPCMBuffer? {
    let frames = AVAudioFrameCount(samples.count)
    guard frames > 0,
          let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames),
          let channel = buffer.floatChannelData?[0]
    else { return nil }
    buffer.frameLength = frames
    samples.enumerated().forEach { channel[$0.offset] = $0.element }
    return buffer
}

private func convert(_ input: AVAudioPCMBuffer, to format: AVAudioFormat) -> AVAudioPCMBuffer? {
    if input.format.isEqual(format) {
        return input
    }
    guard let converter = AVAudioConverter(from: input.format, to: format) else { return nil }
    let ratio = format.sampleRate / input.format.sampleRate
    let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 1024
    guard let output = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return nil }
    var conversionError: NSError?
    var supplied = false
    let status = converter.convert(to: output, error: &conversionError) { _, status in
        if supplied {
            status.pointee = .noDataNow
            return nil
        }
        supplied = true
        status.pointee = .haveData
        return input
    }
    guard status != .error else { return nil }
    return output
}

private func runTranscription(
    samples: [Float],
    localeId: String,
    vocabulary: [String]
) async throws -> String {
    let locale = Locale(identifier: localeId)
    let transcriber = SpeechTranscriber(
        locale: locale,
        transcriptionOptions: [],
        reportingOptions: [],
        attributeOptions: []
    )
    let analyzer = SpeechAnalyzer(modules: [transcriber])

    if !vocabulary.isEmpty {
        let context = AnalysisContext()
        context.contextualStrings[.general] = vocabulary
        try await analyzer.setContext(context)
    }

    guard let target = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
        throw NSError(
            domain: "dev.tf.dictation",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "SpeechAnalyzer has no compatible audio format"]
        )
    }
    guard let inputFormat = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: 16_000,
        channels: 1,
        interleaved: false
    ) else {
        throw NSError(
            domain: "dev.tf.dictation",
            code: 2,
            userInfo: [NSLocalizedDescriptionKey: "Could not create the input audio format"]
        )
    }

    let (stream, continuation) = AsyncStream<AnalyzerInput>.makeStream()
    let chunkFrames = 16_000
    var offset = 0
    while offset < samples.count {
        let end = min(offset + chunkFrames, samples.count)
        if let buffer = makeBuffer(samples: samples[offset..<end], format: inputFormat),
           let converted = convert(buffer, to: target) {
            continuation.yield(AnalyzerInput(buffer: converted))
        }
        offset = end
    }
    continuation.finish()

    let analysis = Task {
        try await analyzer.start(inputSequence: stream)
        try await analyzer.finalizeAndFinishThroughEndOfInput()
    }

    var text = ""
    for try await result in transcriber.results {
        text += String(result.text.characters)
    }
    try await analysis.value
    return text.trimmingCharacters(in: .whitespacesAndNewlines)
}

@_cdecl("tf_speech_transcribe")
public func speechTranscribe(
    _ samples: UnsafePointer<Float>,
    _ count: Int,
    _ localeId: UnsafePointer<CChar>,
    _ vocabularyJson: UnsafePointer<CChar>,
    _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>
) -> Bool {
    let audio = Array(UnsafeBufferPointer(start: samples, count: count))
    let locale = String(cString: localeId)
    let vocabulary = (try? JSONSerialization.jsonObject(with: Data(String(cString: vocabularyJson).utf8)))
        as? [String] ?? []
    let (ok, text): (Bool, String) = blocking {
        do {
            let text = try await runTranscription(samples: audio, localeId: locale, vocabulary: vocabulary)
            return (true, text)
        } catch {
            return (false, speechError(error))
        }
    }
    output.pointee = strdup(text)
    return ok
}
