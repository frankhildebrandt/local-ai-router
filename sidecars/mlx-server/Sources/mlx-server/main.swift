import Foundation
import Network
import CoreImage
import MLX
import MLXLLM
import MLXVLM
import MLXLMCommon
import MLXHuggingFace
import HuggingFace
import Tokenizers

struct Options {
    let model: String
    let host: NWEndpoint.Host
    let port: NWEndpoint.Port
    let alias: String
    let token: String
    let stub: Bool
    let memoryLimitMiB: Int?

    static func parse() throws -> Options {
        let args = Array(CommandLine.arguments.dropFirst())
        func value(_ flag: String) -> String? { args.firstIndex(of: flag).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
        guard let model = value("--model") else { throw ServerError.badRequest("--model is required") }
        let host = NWEndpoint.Host(value("--host") ?? "127.0.0.1")
        guard let port = NWEndpoint.Port(value("--port") ?? "12100") else { throw ServerError.badRequest("invalid --port") }
        guard let token = ProcessInfo.processInfo.environment["LOCAL_AI_ROUTER_RUNTIME_TOKEN"], !token.isEmpty else { throw ServerError.badRequest("runtime token is required") }
        let stub = ProcessInfo.processInfo.environment["LOCAL_AI_ROUTER_STUB_ENGINE"] == "1" || args.contains("--stub")
        let memoryLimitMiB = value("--memory-limit-mib").flatMap(Int.init)
        if let memoryLimitMiB, memoryLimitMiB < 512 { throw ServerError.badRequest("--memory-limit-mib must be at least 512") }
        return Options(model: model, host: host, port: port, alias: value("--alias") ?? "local-mlx", token: token, stub: stub, memoryLimitMiB: memoryLimitMiB)
    }
}

enum ServerError: Error { case badRequest(String) }

private let vlmTypes: Set<String> = [
    "qwen2_vl", "qwen2_5_vl", "qwen3_vl", "gemma3", "gemma3n", "paligemma", "idefics3", "smolvlm",
    "fastvlm", "llava_qwen2", "pixtral", "mistral3", "lfm2_vl", "lfm2-vl"
]

actor InferenceEngine {
    let container: ModelContainer?
    let alias: String
    let stub: Bool
    let vision: Bool

    init(modelPath: String, alias: String, stub: Bool) async throws {
        self.alias = alias
        self.stub = stub
        if stub {
            self.container = nil
            self.vision = true
            return
        }
        let modelType = (try? InferenceEngine.readModelType(modelPath)) ?? ""
        self.vision = vlmTypes.contains(modelType)
        let configuration = ModelConfiguration(directory: URL(filePath: modelPath))
        // Importing MLXVLM registers its factory trampoline so this macro can
        // resolve either MLXLLM or MLXVLM from the pinned 3.31.4 registry.
        self.container = try await #huggingFaceLoadModelContainer(configuration: configuration)
    }

    func generate(payload: sending [String: Any]) async throws -> AsyncThrowingStream<String, Error> {
        let parsed = ParsedPrompt.parse(payload)
        if !parsed.videos.isEmpty {
            throw ServerError.badRequest("video input is not enabled for this pinned MLX runtime")
        }
        if !parsed.audios.isEmpty {
            throw ServerError.badRequest("audio input is not enabled for this pinned MLX runtime")
        }
        if stub {
            return AsyncThrowingStream { continuation in
                continuation.yield(parsed.images.isEmpty ? "stub-text" : "stub-vision")
                continuation.finish()
            }
        }
        guard let container else { throw ServerError.badRequest("model is not loaded") }
        let maxTokens = payload["max_tokens"] as? Int ?? payload["max_output_tokens"] as? Int ?? 512
        let temperature = payload["temperature"] as? Float ?? Float(payload["temperature"] as? Double ?? 0.7)
        let parameters = GenerateParameters(maxTokens: maxTokens, temperature: temperature)
        if vision || !parsed.images.isEmpty {
            let prompt = parsed.prompt
            let system = parsed.system
            let imageURLs = parsed.images
            return AsyncThrowingStream { continuation in
                Task {
                    do {
                        try await container.perform { context in
                            let images = imageURLs.compactMap { UserInput.Image.fromDataURL($0) }
                            var chat: [Chat.Message] = []
                            if !system.isEmpty { chat.append(.system(system)) }
                            chat.append(.user(prompt, images: images))
                            let prepared = try await context.processor.prepare(input: UserInput(chat: chat))
                            let stream = try MLXLMCommon.generate(input: prepared, parameters: parameters, context: context)
                            for await item in stream {
                                if case .chunk(let text) = item { continuation.yield(text) }
                            }
                        }
                        continuation.finish()
                    } catch {
                        continuation.finish(throwing: error)
                    }
                }
            }
        }
        let session = ChatSession(container, instructions: parsed.system.isEmpty ? nil : parsed.system, generateParameters: parameters)
        return session.streamResponse(to: parsed.prompt)
    }

    static func readModelType(_ path: String) throws -> String {
        let data = try Data(contentsOf: URL(filePath: path).appending(path: "config.json"))
        let json = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        return json?["model_type"] as? String ?? ""
    }
}

struct ParsedPrompt {
    var system: String
    var prompt: String
    var images: [String]
    var audios: [String]
    var videos: [String]

    static func parse(_ payload: [String: Any]) -> ParsedPrompt {
        var system = ""
        var lines: [String] = []
        var images: [String] = []
        var audios: [String] = []
        var videos: [String] = []
        func consume(_ content: Any, role: String) {
            if let text = content as? String {
                if role == "system" || role == "developer" { system += (system.isEmpty ? "" : "\n") + text }
                else { lines.append("\(role): \(text)") }
                return
            }
            guard let blocks = content as? [[String: Any]] else { return }
            var text = ""
            for block in blocks {
                let type = block["type"] as? String
                if let value = block["text"] as? String { text += value }
                if type == "image_url" || type == "input_image" {
                    if let url = (block["image_url"] as? [String: Any])?["url"] as? String ?? block["image_url"] as? String { images.append(url) }
                }
                if type == "input_audio" {
                    if let audio = block["input_audio"] as? [String: Any], let data = audio["data"] as? String {
                        audios.append("data:audio/\(audio["format"] as? String ?? "wav");base64,\(data)")
                    }
                }
                if type == "input_video" {
                    if let video = block["input_video"] as? [String: Any], let url = video["url"] as? String { videos.append(url) }
                }
            }
            if role == "system" || role == "developer" { system += (system.isEmpty ? "" : "\n") + text }
            else if !text.isEmpty { lines.append("\(role): \(text)") }
        }
        if let messages = payload["messages"] as? [[String: Any]] {
            for message in messages {
                consume(message["content"] as Any, role: message["role"] as? String ?? "user")
            }
        }
        let prompt = lines.joined(separator: "\n").isEmpty ? (payload["input"] as? String ?? payload["prompt"] as? String ?? "") : lines.joined(separator: "\n")
        return ParsedPrompt(system: system, prompt: prompt, images: images, audios: audios, videos: videos)
    }
}

extension UserInput.Image {
    static func fromDataURL(_ url: String) -> UserInput.Image? {
        guard let range = url.range(of: "base64,") else { return nil }
        let data = Data(base64Encoded: String(url[range.upperBound...]))
        guard let data else { return nil }
        return .ciImage(CIImage(data: data) ?? CIImage.empty())
    }
}

final class HTTPServer: @unchecked Sendable {
    let listener: NWListener
    let engine: InferenceEngine
    let queue = DispatchQueue(label: "app.local-ai-router.mlx-server")
    let token: String

    init(options: Options, engine: InferenceEngine) throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: options.host, port: options.port)
        listener = try NWListener(using: parameters)
        self.engine = engine
        self.token = options.token
        listener.newConnectionHandler = { [weak self] connection in self?.accept(connection) }
        listener.stateUpdateHandler = { state in
            if case .failed(let error) = state { fputs("listener failed: \(error)\n", stderr); exit(1) }
        }
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
        receiveRequest(connection, buffer: Data())
    }

    private func receiveRequest(_ connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 32 * 1024 * 1024) { [weak self] data, _, complete, error in
            guard let self else { return }
            var buffer = buffer
            if let data { buffer.append(data) }
            if let request = HTTPRequest.parse(buffer) {
                Task { await self.handle(request, connection: connection) }
            } else if error == nil && !complete {
                self.receiveRequest(connection, buffer: buffer)
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
        let alias = engine.alias
        if request.path == "/v1/models" { sendJSON(connection, object: ["object": "list", "data": [["id": alias, "object": "model", "owned_by": "mlx"]]]); return }
        let supported = ["/v1/chat/completions", "/v1/responses", "/v1/completions"]
        guard request.method == "POST", supported.contains(request.path), let payload = try? JSONSerialization.jsonObject(with: request.body) as? [String: Any] else {
            sendJSON(connection, status: "404 Not Found", object: errorObject("route_not_found", "Unsupported endpoint")); return
        }
        if payload["tools"] != nil || payload["response_format"] != nil {
            sendJSON(connection, status: "400 Bad Request", object: errorObject("unsupported_capability", "This MLX runtime currently supports text generation without tools or structured output")); return
        }
        let streaming = payload["stream"] as? Bool == true
        do {
            let stream = try await engine.generate(payload: payload)
            if streaming {
                await sendStream(connection, path: request.path, stream: stream)
            } else {
                var text = ""
                for try await chunk in stream { text += chunk }
                sendJSON(connection, object: completionObject(path: request.path, alias: alias, text: text))
            }
        } catch {
            sendJSON(connection, status: "500 Internal Server Error", object: errorObject("inference_error", String(describing: error)))
        }
    }

    private func sendStream(_ connection: NWConnection, path: String, stream: AsyncThrowingStream<String, Error>) async {
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
        try? await connection.sendAsync(Data(headers.utf8))
        do {
            var completeText = ""
            if path == "/v1/responses" {
                let created: [String: Any] = ["type": "response.created", "response": ["id": "resp_local", "object": "response", "status": "in_progress", "model": engine.alias, "output": []]]
                try await sendEvent(connection, name: "response.created", object: created)
            }
            for try await text in stream {
                completeText += text
                let object: [String: Any]
                if path == "/v1/responses" {
                    object = ["type": "response.output_text.delta", "delta": text]
                } else {
                    object = ["id": "chatcmpl-local", "object": "chat.completion.chunk", "model": engine.alias, "choices": [["index": 0, "delta": ["content": text], "finish_reason": NSNull()]]]
                }
                try await sendEvent(connection, name: path == "/v1/responses" ? "response.output_text.delta" : nil, object: object)
            }
            if path == "/v1/responses" {
                try await sendEvent(connection, name: "response.completed", object: ["type": "response.completed", "response": completionObject(path: path, alias: engine.alias, text: completeText)])
            } else {
                try await connection.sendAsync(Data("data: [DONE]\n\n".utf8))
            }
        } catch { }
        connection.cancel()
    }

    private func sendEvent(_ connection: NWConnection, name: String?, object: [String: Any]) async throws {
        let data = try JSONSerialization.data(withJSONObject: object)
        var event = Data()
        if let name { event.append(Data("event: \(name)\n".utf8)) }
        event.append(Data("data: ".utf8)); event.append(data); event.append(Data("\n\n".utf8))
        try await connection.sendAsync(event)
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

extension NWConnection {
    func sendAsync(_ data: Data) async throws {
        try await withCheckedThrowingContinuation { continuation in
            send(content: data, completion: .contentProcessed { error in error.map { continuation.resume(throwing: $0) } ?? continuation.resume() })
        }
    }
}

func completionObject(path: String, alias: String, text: String) -> [String: Any] {
    if path == "/v1/responses" { return ["id": "resp_local", "object": "response", "status": "completed", "model": alias, "output": [["type": "message", "role": "assistant", "content": [["type": "output_text", "text": text]]]]] }
    if path == "/v1/completions" { return ["id": "cmpl-local", "object": "text_completion", "model": alias, "choices": [["index": 0, "text": text, "finish_reason": "stop"]]] }
    return ["id": "chatcmpl-local", "object": "chat.completion", "model": alias, "choices": [["index": 0, "message": ["role": "assistant", "content": text], "finish_reason": "stop"]]]
}

func errorObject(_ code: String, _ message: String) -> [String: Any] { ["error": ["message": message, "type": "invalid_request_error", "code": code]] }

@main struct MLXServer {
    static func main() async throws {
        let options = try Options.parse()
        if let limit = options.memoryLimitMiB {
            Memory.memoryLimit = limit * 1024 * 1024
            Memory.cacheLimit = min(max(64, limit / 8), 512) * 1024 * 1024
        }
        let engine = try await InferenceEngine(modelPath: options.model, alias: options.alias, stub: options.stub)
        try await HTTPServer(options: options, engine: engine).run()
    }
}
