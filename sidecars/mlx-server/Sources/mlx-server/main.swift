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
import MLXServerCore

struct Options {
    let model: String
    let host: NWEndpoint.Host
    let port: NWEndpoint.Port
    let alias: String
    let token: String
    let stub: Bool
    let memoryLimitMiB: Int?
    let slotSavePath: String?
    let draftModel: String?
    let draftTokens: Int

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
        let draftTokens = value("--draft-tokens").flatMap(Int.init) ?? 5
        if draftTokens < 1 { throw ServerError.badRequest("--draft-tokens must be at least 1") }
        return Options(
            model: model,
            host: host,
            port: port,
            alias: value("--alias") ?? "local-mlx",
            token: token,
            stub: stub,
            memoryLimitMiB: memoryLimitMiB,
            slotSavePath: value("--slot-save-path"),
            draftModel: value("--draft-model"),
            draftTokens: draftTokens
        )
    }
}

enum ServerError: Error { case badRequest(String) }

private let vlmTypes: Set<String> = [
    "qwen2_vl", "qwen2_5_vl", "qwen3_vl", "qwen3_5", "qwen3_5_moe", "gemma3", "gemma3n", "paligemma", "idefics3", "smolvlm",
    "fastvlm", "llava_qwen2", "pixtral", "mistral3", "lfm2_vl", "lfm2-vl"
]

private struct LivePromptCache: @unchecked Sendable {
    var cache: [KVCache]
    var tokens: [Int]
}

actor InferenceEngine {
    let container: ModelContainer?
    let draftContainer: ModelContainer?
    let draftTokens: Int
    let alias: String
    let stub: Bool
    let vision: Bool
    private let slotSaveRoot: URL?
    private var live: LivePromptCache?

    init(modelPath: String, alias: String, stub: Bool, slotSavePath: String?, draftPath: String?, draftTokens: Int) async throws {
        self.alias = alias
        self.stub = stub
        self.draftTokens = max(draftTokens, 1)
        self.slotSaveRoot = slotSavePath.map { URL(filePath: $0, directoryHint: .isDirectory) }
        if stub {
            self.container = nil
            self.draftContainer = nil
            self.vision = true
            return
        }
        let modelType = (try? InferenceEngine.readModelType(modelPath)) ?? ""
        self.vision = vlmTypes.contains(modelType)
        let configuration = ModelConfiguration(directory: URL(filePath: modelPath))
        // Importing MLXVLM registers its factory trampoline so this macro can
        // resolve either MLXLLM or MLXVLM from the pinned 3.31.4 registry.
        self.container = try await #huggingFaceLoadModelContainer(configuration: configuration)
        if let draftPath {
            let draftConfiguration = ModelConfiguration(directory: URL(filePath: draftPath))
            self.draftContainer = try await #huggingFaceLoadModelContainer(configuration: draftConfiguration)
        } else {
            self.draftContainer = nil
        }
    }

    func generate(payload: sending [String: Any], namespace: String, session: String?) async throws -> AsyncThrowingStream<String, Error> {
        let parsed = parsePromptTurns(payload)
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
        let turns = parsed.turns
        let draftTokens = self.draftTokens
        let usePrefixCache = !vision && parsed.images.isEmpty && draftContainer == nil
        let useSpeculative = draftContainer != nil && !vision && parsed.images.isEmpty
        return AsyncThrowingStream { continuation in
            Task {
                do {
                    if useSpeculative, let draftContainer {
                        let session = ChatSession(
                            container,
                            speculativeDecoding: SpeculativeDecodingConfig(
                                draftModel: draftContainer,
                                numDraftTokens: draftTokens
                            ),
                            generateParameters: parameters
                        )
                        for try await text in session.streamResponse(to: chatMessages(from: turns)) {
                            continuation.yield(text)
                        }
                    } else if usePrefixCache {
                        try await self.generateCached(
                            container: container,
                            turns: turns,
                            parameters: parameters,
                            namespace: namespace,
                            session: session,
                            existing: self.live,
                            continuation: continuation
                        )
                    } else {
                        try await container.perform { context in
                            let prepared = try await context.processor.prepare(input: UserInput(chat: chatMessages(from: turns)))
                            let stream = try MLXLMCommon.generate(input: prepared, parameters: parameters, context: context)
                            for await item in stream {
                                if case .chunk(let text) = item { continuation.yield(text) }
                            }
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
        }
    }

    func saveSlot(filename: String) async throws {
        if stub { return }
        guard let live else { throw ServerError.badRequest("no live KV slot") }
        let url = try slotURL(filename)
        guard let container else { throw ServerError.badRequest("model is not loaded") }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let slot = live
        try await container.perform { _ in
            try savePromptCache(url: url, cache: slot.cache)
        }
        try writeTokenFile(url, tokens: slot.tokens)
    }

    func restoreSlot(filename: String) async throws {
        if stub { return }
        guard let container else { throw ServerError.badRequest("model is not loaded") }
        let url = try slotURL(filename)
        let tokens = try readTokenFile(url)
        live = try await container.perform { _ in
            LivePromptCache(cache: try loadPromptCache(url: url).0, tokens: tokens)
        }
    }

    private func generateCached(
        container: ModelContainer,
        turns: [PromptTurn],
        parameters: GenerateParameters,
        namespace: String,
        session: String?,
        existing: LivePromptCache?,
        continuation: AsyncThrowingStream<String, Error>.Continuation
    ) async throws {
        let directory = cacheDirectory(namespace: namespace, session: session)
        let next = try await container.perform { context -> LivePromptCache in
            let prepared = try await context.processor.prepare(input: UserInput(chat: chatMessages(from: turns)))
            let promptTokens = prepared.text.tokens.asArray(Int.self)
            var slot = try resolveCache(
                existing: existing,
                promptTokens: promptTokens,
                model: context.model,
                parameters: parameters,
                directory: directory
            )
            var suffix = Array(promptTokens.dropFirst(slot.tokens.count))
            if suffix.isEmpty, !promptTokens.isEmpty, canTrimPromptCache(slot.cache) {
                _ = trimPromptCache(slot.cache, numTokens: 1)
                slot.tokens.removeLast()
                suffix = [promptTokens[promptTokens.count - 1]]
            }
            let input = suffix.isEmpty ? prepared : LMInput(tokens: MLXArray(suffix))
            var detokenizer = NaiveStreamingDetokenizer(tokenizer: context.tokenizer)
            var generated: [Int] = []
            let (tokenStream, tokenTask) = try generateTokensTask(
                input: input,
                cache: slot.cache,
                parameters: parameters,
                context: context
            )
            for await item in tokenStream {
                if case .token(let token) = item {
                    if token == context.tokenizer.eosTokenId { break }
                    generated.append(token)
                    detokenizer.append(token: token)
                    if let text = detokenizer.next() { continuation.yield(text) }
                }
            }
            await tokenTask.value
            slot.tokens.append(contentsOf: suffix)
            slot.tokens.append(contentsOf: generated)
            if let directory {
                try? persistHashSnapshot(slot, directory: directory)
            }
            return slot
        }
        live = next
    }

    nonisolated private func cacheDirectory(namespace: String, session: String?) -> URL? {
        guard let slotSaveRoot else { return nil }
        return slotSaveRoot
            .appending(path: sanitizeCacheComponent(namespace))
            .appending(path: sanitizeCacheComponent(session ?? "_"))
    }

    private func slotURL(_ filename: String) throws -> URL {
        guard let slotSaveRoot else { throw ServerError.badRequest("slot save path is not configured") }
        guard let url = resolveSlotURL(root: slotSaveRoot, filename: filename) else {
            throw ServerError.badRequest("invalid slot filename")
        }
        return url
    }

    nonisolated private func writeTokenFile(_ cacheURL: URL, tokens: [Int]) throws {
        let data = try JSONSerialization.data(withJSONObject: tokens)
        try data.write(to: cacheURL.deletingPathExtension().appendingPathExtension("tokens.json"), options: .atomic)
    }

    nonisolated private func readTokenFile(_ cacheURL: URL) throws -> [Int] {
        let url = cacheURL.deletingPathExtension().appendingPathExtension("tokens.json")
        let data = try Data(contentsOf: url)
        guard let tokens = try JSONSerialization.jsonObject(with: data) as? [Int] else {
            throw ServerError.badRequest("invalid token sidecar")
        }
        return tokens
    }

    static func readModelType(_ path: String) throws -> String {
        let data = try Data(contentsOf: URL(filePath: path).appending(path: "config.json"))
        let json = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        return json?["model_type"] as? String ?? ""
    }
}

private func resolveCache(
    existing: LivePromptCache?,
    promptTokens: [Int],
    model: any LanguageModel,
    parameters: GenerateParameters,
    directory: URL?
) throws -> LivePromptCache {
    if var current = existing {
        let prefix = longestCommonPrefix(current.tokens, promptTokens)
        if prefix == current.tokens.count {
            return current
        }
        if prefix > 0, canTrimPromptCache(current.cache) {
            let trimmed = current.tokens.count - prefix
            if trimPromptCache(current.cache, numTokens: trimmed) == trimmed {
                current.tokens.removeLast(trimmed)
                return current
            }
        }
    }
    if let restored = try loadHashHit(promptTokens, directory: directory) {
        let prefix = longestCommonPrefix(restored.tokens, promptTokens)
        var current = restored
        if prefix < current.tokens.count {
            guard canTrimPromptCache(current.cache) else {
                return LivePromptCache(cache: model.newCache(parameters: parameters), tokens: [])
            }
            let trimmed = current.tokens.count - prefix
            _ = trimPromptCache(current.cache, numTokens: trimmed)
            current.tokens.removeLast(trimmed)
        }
        return current
    }
    return LivePromptCache(cache: model.newCache(parameters: parameters), tokens: [])
}

private func persistHashSnapshot(_ slot: LivePromptCache, directory: URL) throws {
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let file = "auto-\(tokenHash(slot.tokens).prefix(16)).safetensors"
    let url = directory.appending(path: file)
    try savePromptCache(url: url, cache: slot.cache)
    try JSONSerialization.data(withJSONObject: slot.tokens).write(
        to: url.deletingPathExtension().appendingPathExtension("tokens.json"),
        options: .atomic
    )
    var index = (try? loadPromptIndex(directory)) ?? [:]
    index = index.filter { _, entry in
        FileManager.default.fileExists(atPath: directory.appending(path: entry.file).path)
    }
    for (hash, entry) in indexEntries(for: slot.tokens, file: file) {
        index[hash] = entry
    }
    try JSONEncoder().encode(index).write(to: directory.appending(path: "index.json"), options: .atomic)
}

private func loadHashHit(_ tokens: [Int], directory: URL?) throws -> LivePromptCache? {
    guard let directory else { return nil }
    let index = try loadPromptIndex(directory)
    guard let hit = bestIndexHit(tokens: tokens, index: index) else { return nil }
    let url = directory.appending(path: hit.file)
    guard FileManager.default.fileExists(atPath: url.path) else { return nil }
    let cache = try loadPromptCache(url: url).0
    let tokenURL = url.deletingPathExtension().appendingPathExtension("tokens.json")
    guard let stored = try JSONSerialization.jsonObject(with: Data(contentsOf: tokenURL)) as? [Int] else {
        return nil
    }
    return LivePromptCache(cache: cache, tokens: stored)
}

private func loadPromptIndex(_ directory: URL) throws -> [String: PromptCacheIndexEntry] {
    let url = directory.appending(path: "index.json")
    guard FileManager.default.fileExists(atPath: url.path) else { return [:] }
    return try JSONDecoder().decode([String: PromptCacheIndexEntry].self, from: Data(contentsOf: url))
}

private func chatMessages(from turns: [PromptTurn]) -> [Chat.Message] {
    turns.map { turn in
        let images = turn.imageURLs.compactMap(UserInput.Image.fromDataURL)
        switch turn.role {
        case "system", "developer":
            return .system(turn.text)
        case "assistant":
            return .assistant(turn.text)
        case "tool":
            return .tool(turn.text, id: turn.toolCallId)
        default:
            return .user(turn.text, images: images)
        }
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
        if request.path == "/slots/0" {
            await handleSlot(request, connection: connection)
            return
        }
        let supported = ["/v1/chat/completions", "/v1/responses", "/v1/completions"]
        guard request.method == "POST", supported.contains(request.path), let payload = try? JSONSerialization.jsonObject(with: request.body) as? [String: Any] else {
            sendJSON(connection, status: "404 Not Found", object: errorObject("route_not_found", "Unsupported endpoint")); return
        }
        let streaming = payload["stream"] as? Bool == true
        do {
            let namespace = request.headers["x-local-ai-cache-namespace"] ?? "default"
            let session = request.headers["x-local-ai-session"]
            let stream = try await engine.generate(payload: payload, namespace: namespace, session: session)
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

    private func handleSlot(_ request: HTTPRequest, connection: NWConnection) async {
        guard request.method == "POST" else {
            sendJSON(connection, status: "404 Not Found", object: errorObject("route_not_found", "Unsupported endpoint"))
            return
        }
        let action = request.query["action"] ?? ""
        let filename = ((try? JSONSerialization.jsonObject(with: request.body)) as? [String: Any])?["filename"] as? String
        guard let filename, !filename.isEmpty else {
            sendJSON(connection, status: "400 Bad Request", object: errorObject("invalid_request", "filename is required"))
            return
        }
        do {
            switch action {
            case "save":
                try await engine.saveSlot(filename: filename)
            case "restore":
                try await engine.restoreSlot(filename: filename)
            default:
                sendJSON(connection, status: "400 Bad Request", object: errorObject("invalid_request", "unknown slot action"))
                return
            }
            sendJSON(connection, object: ["ok": true])
        } catch {
            sendJSON(connection, status: "500 Internal Server Error", object: errorObject("slot_error", String(describing: error)))
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
    let query: [String: String]
    let headers: [String: String]
    let body: Data

    static func parse(_ data: Data) -> HTTPRequest? {
        let separator = Data("\r\n\r\n".utf8)
        guard let range = data.range(of: separator), let head = String(data: data[..<range.lowerBound], encoding: .utf8) else { return nil }
        let lines = head.components(separatedBy: "\r\n")
        let requestLine = lines.first?.split(separator: " ") ?? []
        guard requestLine.count >= 2 else { return nil }
        let rawPath = String(requestLine[1])
        let pathParts = rawPath.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        let path = String(pathParts[0])
        var query: [String: String] = [:]
        if pathParts.count == 2 {
            for pair in pathParts[1].split(separator: "&") {
                let sides = pair.split(separator: "=", maxSplits: 1)
                if sides.count == 2 {
                    query[sides[0].removingPercentEncoding ?? String(sides[0])] = sides[1].removingPercentEncoding ?? String(sides[1])
                }
            }
        }
        let headers = Dictionary(uniqueKeysWithValues: lines.dropFirst().compactMap { line -> (String, String)? in
            let parts = line.split(separator: ":", maxSplits: 1)
            guard parts.count == 2 else { return nil }
            return (parts[0].lowercased(), parts[1].trimmingCharacters(in: .whitespaces))
        })
        let length = headers["content-length"].flatMap(Int.init) ?? 0
        let bodyStart = range.upperBound
        guard data.count >= bodyStart + length else { return nil }
        return HTTPRequest(method: String(requestLine[0]), path: path, query: query, headers: headers, body: data.subdata(in: bodyStart..<(bodyStart + length)))
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
    if path == "/v1/completions" { return ["id": "cmpl_local", "object": "text_completion", "model": alias, "choices": [["index": 0, "text": text, "finish_reason": "stop"]]] }
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
        let engine = try await InferenceEngine(
            modelPath: options.model,
            alias: options.alias,
            stub: options.stub,
            slotSavePath: options.slotSavePath,
            draftPath: options.draftModel,
            draftTokens: options.draftTokens
        )
        try await HTTPServer(options: options, engine: engine).run()
    }
}
