import Foundation
import Kokoro
import Network

struct Options {
    let model: String
    let host: NWEndpoint.Host
    let port: NWEndpoint.Port
    let alias: String
    let token: String
    let stub: Bool

    static func parse() throws -> Options {
        let args = Array(CommandLine.arguments.dropFirst())
        func value(_ flag: String) -> String? { args.firstIndex(of: flag).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
        guard let model = value("--model") else { throw ServerError.badRequest("--model is required") }
        let host = NWEndpoint.Host(value("--host") ?? "127.0.0.1")
        guard let port = NWEndpoint.Port(value("--port") ?? "12100") else { throw ServerError.badRequest("invalid --port") }
        guard let token = ProcessInfo.processInfo.environment["LOCAL_AI_ROUTER_RUNTIME_TOKEN"], !token.isEmpty else { throw ServerError.badRequest("runtime token is required") }
        let stub = ProcessInfo.processInfo.environment["LOCAL_AI_ROUTER_STUB_ENGINE"] == "1" || args.contains("--stub")
        return Options(model: model, host: host, port: port, alias: value("--alias") ?? "local-speech", token: token, stub: stub)
    }
}

enum ServerError: Error { case badRequest(String) }

actor SpeechEngine {
    let alias: String
    let stub: Bool
    let modelPath: String
    var pipeline: KPipeline?

    init(options: Options) throws {
        alias = options.alias
        stub = options.stub
        modelPath = options.model
        if stub { return }
        let root = URL(filePath: modelPath)
        let config = root.appending(path: "MLX_GPU/config.json")
        let weights = root.appending(path: "MLX_GPU/kokoro-v1_0.safetensors")
        let voices = root.appending(path: "MLX_GPU/voices")
        let model = try KModel(configURL: FileManager.default.fileExists(atPath: config.path) ? config : root.appending(path: "config.json"), weightsURL: FileManager.default.fileExists(atPath: weights.path) ? weights : root.appending(path: "model.safetensors"))
        pipeline = KPipeline(model: model, voices: VoiceLoader(baseDirectory: FileManager.default.fileExists(atPath: voices.path) ? voices : root.appending(path: "voices"), enableDownload: false))
    }

    func synthesize(text: String, voice: String, speed: Float, pcm: Bool) throws -> (Data, String) {
        if stub {
            let samples = [Float](repeating: 0, count: 2400)
            return pcm ? (pcmData(samples), "audio/pcm") : (wavData(samples, sampleRate: 24000), "audio/wav")
        }
        guard let pipeline else { throw ServerError.badRequest("speech model is not loaded") }
        let result = try pipeline.synthesize(text: text, voice: voice, speed: speed)
        return pcm ? (pcmData(result.audio), "audio/pcm") : (wavData(result.audio, sampleRate: result.sampleRate), "audio/wav")
    }
}

final class HTTPServer: @unchecked Sendable {
    let listener: NWListener
    let engine: SpeechEngine
    let queue = DispatchQueue(label: "app.local-ai-router.mlx-speech-server")
    let token: String

    init(options: Options, engine: SpeechEngine) throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: options.host, port: options.port)
        listener = try NWListener(using: parameters)
        self.engine = engine
        self.token = options.token
        listener.newConnectionHandler = { [weak self] connection in self?.accept(connection) }
    }

    func run() async {
        listener.start(queue: queue)
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3600))
        }
        listener.cancel()
    }

    private func accept(_ connection: NWConnection) {
        connection.start(queue: queue)
        receive(connection, buffer: Data())
    }

    private func receive(_ connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 8 * 1024 * 1024) { [weak self] data, _, complete, error in
            guard let self else { return }
            var buffer = buffer
            if let data { buffer.append(data) }
            if let request = HTTPRequest.parse(buffer) {
                Task { await self.handle(request, connection: connection) }
            } else if error == nil && !complete {
                self.receive(connection, buffer: buffer)
            } else {
                self.sendJSON(connection, status: "400 Bad Request", object: errorObject("invalid_request", "Incomplete HTTP request"))
            }
        }
    }

    private func handle(_ request: HTTPRequest, connection: NWConnection) async {
        if request.path == "/health" { sendJSON(connection, object: ["status": "ok"]); return }
        guard request.headers["authorization"] == "Bearer \(token)" else {
            sendJSON(connection, status: "401 Unauthorized", object: errorObject("invalid_api_key", "Invalid internal runtime credential")); return
        }
        guard request.method == "POST", request.path == "/v1/audio/speech", let payload = try? JSONSerialization.jsonObject(with: request.body) as? [String: Any] else {
            sendJSON(connection, status: "404 Not Found", object: errorObject("route_not_found", "Unsupported endpoint")); return
        }
        let text = payload["input"] as? String ?? ""
        let voice = payload["voice"] as? String ?? "af_heart"
        let speed = Float(payload["speed"] as? Double ?? 1)
        let pcm = (payload["response_format"] as? String) == "pcm"
        do {
            let (body, mime) = try await engine.synthesize(text: text, voice: voice, speed: speed, pcm: pcm)
            var response = Data("HTTP/1.1 200 OK\r\nContent-Type: \(mime)\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
            response.append(body)
            connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
        } catch {
            sendJSON(connection, status: "500 Internal Server Error", object: errorObject("inference_error", String(describing: error)))
        }
    }

    private func sendJSON(_ connection: NWConnection, status: String = "200 OK", object: [String: Any]) {
        let body = (try? JSONSerialization.data(withJSONObject: object)) ?? Data("{}".utf8)
        var response = Data("HTTP/1.1 \(status)\r\nContent-Type: application/json\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
        response.append(body)
        connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
    }
}

struct HTTPRequest {
    let method: String
    let path: String
    let headers: [String: String]
    let body: Data

    static func parse(_ data: Data) -> HTTPRequest? {
        let separator = Data("\r\n\r\n".utf8)
        guard let range = data.range(of: separator), let head = String(data: data[..<range.lowerBound], encoding: .utf8) else { return nil }
        let lines = head.components(separatedBy: "\r\n")
        let requestLine = lines.first?.split(separator: " ") ?? []
        guard requestLine.count >= 2 else { return nil }
        let headers = Dictionary(uniqueKeysWithValues: lines.dropFirst().compactMap { line -> (String, String)? in
            let parts = line.split(separator: ":", maxSplits: 1)
            guard parts.count == 2 else { return nil }
            return (parts[0].lowercased(), parts[1].trimmingCharacters(in: .whitespaces))
        })
        let length = headers["content-length"].flatMap(Int.init) ?? 0
        let bodyStart = range.upperBound
        guard data.count >= bodyStart + length else { return nil }
        return HTTPRequest(method: String(requestLine[0]), path: String(requestLine[1]), headers: headers, body: data.subdata(in: bodyStart..<(bodyStart + length)))
    }
}

func errorObject(_ code: String, _ message: String) -> [String: Any] { ["error": ["message": message, "type": "invalid_request_error", "code": code]] }

func pcmData(_ samples: [Float]) -> Data {
    var data = Data()
    for sample in samples {
        var value = Int16(max(-1, min(1, sample)) * Float(Int16.max))
        data.append(Data(bytes: &value, count: 2))
    }
    return data
}

func wavData(_ samples: [Float], sampleRate: Int) -> Data {
    let pcm = pcmData(samples)
    var header = Data()
    func append(_ value: String) { header.append(contentsOf: value.utf8) }
    func appendU32(_ value: UInt32) { var value = value.littleEndian; header.append(Data(bytes: &value, count: 4)) }
    func appendU16(_ value: UInt16) { var value = value.littleEndian; header.append(Data(bytes: &value, count: 2)) }
    append("RIFF"); appendU32(UInt32(36 + pcm.count)); append("WAVE"); append("fmt "); appendU32(16); appendU16(1); appendU16(1); appendU32(UInt32(sampleRate)); appendU32(UInt32(sampleRate * 2)); appendU16(2); appendU16(16); append("data"); appendU32(UInt32(pcm.count))
    header.append(pcm)
    return header
}

@main struct SpeechServer {
    static func main() async throws {
        let options = try Options.parse()
        try await HTTPServer(options: options, engine: SpeechEngine(options: options)).run()
    }
}
