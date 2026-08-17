import Foundation
import Network
import MLXLLM
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

    static func parse() throws -> Options {
        let args = Array(CommandLine.arguments.dropFirst())
        func value(_ flag: String) -> String? { args.firstIndex(of: flag).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
        guard let model = value("--model") else { throw ServerError.badRequest("--model is required") }
        let host = NWEndpoint.Host(value("--host") ?? "127.0.0.1")
        guard let port = NWEndpoint.Port(value("--port") ?? "12100") else { throw ServerError.badRequest("invalid --port") }
        guard let token = ProcessInfo.processInfo.environment["LOCAL_AI_ROUTER_RUNTIME_TOKEN"], !token.isEmpty else { throw ServerError.badRequest("runtime token is required") }
        return Options(model: model, host: host, port: port, alias: value("--alias") ?? "local-mlx", token: token)
    }
}

enum ServerError: Error { case badRequest(String) }

actor InferenceEngine {
    let container: ModelContainer
    let alias: String

    init(modelPath: String, alias: String) async throws {
        self.alias = alias
        let configuration = ModelConfiguration(directory: URL(filePath: modelPath))
        self.container = try await #huggingFaceLoadModelContainer(configuration: configuration)
    }

    func generate(payload: sending [String: Any]) async throws -> AsyncThrowingStream<String, Error> {
        let maxTokens = payload["max_tokens"] as? Int ?? payload["max_output_tokens"] as? Int ?? 512
        let temperature = payload["temperature"] as? Float ?? Float(payload["temperature"] as? Double ?? 0.7)
        let parameters = GenerateParameters(maxTokens: maxTokens, temperature: temperature)
        let messages = payload["messages"] as? [[String: Any]]
        let system = messages?.filter { ($0["role"] as? String) == "system" || ($0["role"] as? String) == "developer" }.compactMap { $0["content"] as? String }.joined(separator: "\n")
        let chatPrompt = messages?.filter { ($0["role"] as? String) != "system" && ($0["role"] as? String) != "developer" }.compactMap { message -> String? in
            guard let role = message["role"] as? String, let content = message["content"] as? String else { return nil }
            return "\(role): \(content)"
        }.joined(separator: "\n")
        let input = payload["input"] as? String
        let prompt = chatPrompt ?? input ?? payload["prompt"] as? String ?? ""
        let session = ChatSession(container, instructions: system?.isEmpty == false ? system : nil, generateParameters: parameters)
        return session.streamResponse(to: prompt)
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
        await withCheckedContinuation { (_: CheckedContinuation<Void, Never>) in }
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
        if request.path == "/v1/models" { sendJSON(connection, object: ["object": "list", "data": [["id": engine.alias, "object": "model", "owned_by": "mlx"]]]); return }
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
                sendJSON(connection, object: completionObject(path: request.path, alias: engine.alias, text: text))
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
        let engine = try await InferenceEngine(modelPath: options.model, alias: options.alias)
        try await HTTPServer(options: options, engine: engine).run()
    }
}
