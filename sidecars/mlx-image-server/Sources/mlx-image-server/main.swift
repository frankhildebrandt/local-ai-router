import CoreGraphics
import Foundation
import Flux2Core
import FluxTextEncoders
import Hub
import ImageIO
import MLX
import Network
import StableDiffusion
import UniformTypeIdentifiers

struct Options {
    let model: String
    let host: NWEndpoint.Host
    let port: NWEndpoint.Port
    let alias: String
    let token: String
    let pipeline: String
    let stub: Bool

    static func parse() throws -> Options {
        let args = Array(CommandLine.arguments.dropFirst())
        func value(_ flag: String) -> String? { args.firstIndex(of: flag).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
        guard let model = value("--model") else { throw ServerError.badRequest("--model is required") }
        let host = NWEndpoint.Host(value("--host") ?? "127.0.0.1")
        guard let port = NWEndpoint.Port(value("--port") ?? "12100") else { throw ServerError.badRequest("invalid --port") }
        guard let token = ProcessInfo.processInfo.environment["LOCAL_AI_ROUTER_RUNTIME_TOKEN"], !token.isEmpty else { throw ServerError.badRequest("runtime token is required") }
        let stub = ProcessInfo.processInfo.environment["LOCAL_AI_ROUTER_STUB_ENGINE"] == "1" || args.contains("--stub")
        return Options(model: model, host: host, port: port, alias: value("--alias") ?? "local-image", token: token, pipeline: value("--pipeline") ?? "flux2", stub: stub)
    }
}

enum ServerError: Error { case badRequest(String) }

actor ImageEngine {
    let alias: String
    let pipeline: String
    let stub: Bool
    let modelPath: String
    var flux: Flux2Pipeline?

    init(options: Options) {
        alias = options.alias
        pipeline = options.pipeline
        stub = options.stub
        modelPath = options.model
    }

    func generate(prompt: String, width: Int, height: Int) async throws -> Data {
        if stub { return stubPNG(width: min(max(width, 1), 64), height: min(max(height, 1), 64)) }
        if pipeline == "sdxl" {
            return try generateSDXL(modelDirectory: URL(filePath: modelPath), prompt: prompt, width: width, height: height)
        }
        if flux == nil {
            let modelDirectory = URL(filePath: modelPath)
            configureFluxLocalCache(modelDirectory: modelDirectory)
            let pipeline = Flux2Pipeline(
                model: .klein4B,
                quantization: Flux2QuantizationConfig(textEncoder: .mlx4bit, transformer: .qint8)
            )
            try await pipeline.loadModels()
            flux = pipeline
        }
        guard let flux else { throw ServerError.badRequest("FLUX.2 pipeline is not loaded") }
        let image = try await flux.generateTextToImage(prompt: prompt, height: height, width: width, steps: 4, guidance: 1.0)
        return pngData(from: image)
    }
}

func configureFluxLocalCache(modelDirectory: URL) {
    let cache = modelDirectory.appending(path: ".flux-cache")
    let destinations = [
        cache.appending(path: "black-forest-labs/FLUX.2-klein-4B-klein4b-8bit"),
        cache.appending(path: "black-forest-labs/FLUX.2-klein-4B-klein4b-bf16"),
        cache.appending(path: "black-forest-labs/FLUX.2-klein-4B"),
    ]
    for destination in destinations {
        try? FileManager.default.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        if !FileManager.default.fileExists(atPath: destination.path) {
            try? FileManager.default.createSymbolicLink(at: destination, withDestinationURL: modelDirectory)
        }
    }
    ModelRegistry.customModelsDirectory = cache
    TextEncoderModelDownloader.customModelsDirectory = cache
    TextEncoderModelDownloader.reconfigureHubApi()
}

func generateSDXL(modelDirectory: URL, prompt: String, width: Int, height: Int) throws -> Data {
    let required = [
        "unet/config.json", "vae/config.json",
        "text_encoder/config.json", "text_encoder_2/config.json",
        "tokenizer/vocab.json", "tokenizer/merges.txt",
        "tokenizer_2/vocab.json", "tokenizer_2/merges.txt",
        "scheduler/scheduler_config.json",
    ]
    for relative in required {
        let path = modelDirectory.appending(path: relative)
        if !FileManager.default.fileExists(atPath: path.path) {
            throw ServerError.badRequest("SDXL Turbo is missing required artifact \(relative)")
        }
    }
    let hasUnetWeights = ["unet/diffusion_pytorch_model.safetensors", "unet/model.safetensors"].contains {
        FileManager.default.fileExists(atPath: modelDirectory.appending(path: $0).path)
    }
    if !hasUnetWeights {
        throw ServerError.badRequest("SDXL Turbo is missing unet safetensors weights")
    }
    let hubRoot = modelDirectory.appending(path: ".hub")
    let repoDir = hubRoot.appending(path: "models/stabilityai/sdxl-turbo")
    try FileManager.default.createDirectory(at: repoDir.deletingLastPathComponent(), withIntermediateDirectories: true)
    if !FileManager.default.fileExists(atPath: repoDir.path) {
        try FileManager.default.createSymbolicLink(at: repoDir, withDestinationURL: modelDirectory)
    }
    let hub = HubApi(downloadBase: hubRoot, useOfflineMode: true)
    let configuration = StableDiffusionConfiguration.presetSDXLTurbo
    guard let generator = try configuration.textToImageGenerator(hub: hub, configuration: .init()) else {
        throw ServerError.badRequest("unable to create the SDXL Turbo generator")
    }
    generator.ensureLoaded()
    var parameters = configuration.defaultParameters()
    parameters.prompt = prompt
    parameters.imageCount = 1
    parameters.latentSize = [max(height / 8, 8), max(width / 8, 8)]
    var last: MLXArray?
    for latent in generator.generateLatents(parameters: parameters) {
        last = latent
        eval(latent)
    }
    guard let last else { throw ServerError.badRequest("SDXL Turbo produced no latents") }
    let decoded = generator.decode(xt: last)
    eval(decoded)
    return pngData(from: Image(decoded.squeezed()).asCGImage())
}

final class HTTPServer: @unchecked Sendable {
    let listener: NWListener
    let engine: ImageEngine
    let queue = DispatchQueue(label: "app.local-ai-router.mlx-image-server")
    let token: String

    init(options: Options, engine: ImageEngine) throws {
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
        guard request.method == "POST", request.path == "/v1/images/generations", let payload = try? JSONSerialization.jsonObject(with: request.body) as? [String: Any] else {
            sendJSON(connection, status: "404 Not Found", object: errorObject("route_not_found", "Unsupported endpoint")); return
        }
        let prompt = payload["prompt"] as? String ?? ""
        let size = (payload["size"] as? String ?? "1024x1024").split(separator: "x")
        let width = Int(size.first ?? "1024") ?? 1024
        let height = Int(size.last ?? "1024") ?? 1024
        do {
            let png = try await engine.generate(prompt: prompt, width: width, height: height)
            sendJSON(connection, object: ["created": Int(Date().timeIntervalSince1970), "data": [["b64_json": png.base64EncodedString()]]])
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

func stubPNG(width: Int, height: Int) -> Data {
    var bytes = [UInt8](repeating: 80, count: max(width * height * 4, 16))
    for index in stride(from: 3, to: bytes.count, by: 4) { bytes[index] = 255 }
    let provider = CGDataProvider(data: Data(bytes) as CFData)!
    let image = CGImage(width: max(width, 1), height: max(height, 1), bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: max(width, 1) * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue), provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
    return pngData(from: image)
}

func pngData(from image: CGImage) -> Data {
    let data = NSMutableData()
    let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, nil)
    CGImageDestinationFinalize(destination)
    return data as Data
}

@main struct ImageServer {
    static func main() async throws {
        let options = try Options.parse()
        try await HTTPServer(options: options, engine: ImageEngine(options: options)).run()
    }
}
