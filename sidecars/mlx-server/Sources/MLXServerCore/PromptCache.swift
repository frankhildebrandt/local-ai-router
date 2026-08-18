import CryptoKit
import Foundation

public let promptCacheBlockSize = 256

public struct PromptTurn: Equatable, Sendable {
    public var role: String
    public var text: String
    public var imageURLs: [String]
    public var audioURLs: [String]
    public var videoURLs: [String]
    public var toolCallId: String?

    public init(
        role: String,
        text: String,
        imageURLs: [String] = [],
        audioURLs: [String] = [],
        videoURLs: [String] = [],
        toolCallId: String? = nil
    ) {
        self.role = role
        self.text = text
        self.imageURLs = imageURLs
        self.audioURLs = audioURLs
        self.videoURLs = videoURLs
        self.toolCallId = toolCallId
    }

    public var hasVision: Bool { !imageURLs.isEmpty }
}

public struct PromptCacheIndexEntry: Codable, Equatable, Sendable {
    public var file: String
    public var length: Int

    public init(file: String, length: Int) {
        self.file = file
        self.length = length
    }
}

public func longestCommonPrefix(_ left: [Int], _ right: [Int]) -> Int {
    let limit = min(left.count, right.count)
    var index = 0
    while index < limit, left[index] == right[index] {
        index += 1
    }
    return index
}

public func tokenHash<S: Sequence>(_ tokens: S) -> String where S.Element == Int {
    var data = Data()
    for token in tokens {
        var value = Int32(truncatingIfNeeded: token).littleEndian
        withUnsafeBytes(of: &value) { data.append(contentsOf: $0) }
    }
    return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}

public func prefixHashes(_ tokens: [Int], blockSize: Int = promptCacheBlockSize) -> [(length: Int, hash: String)] {
    guard !tokens.isEmpty else { return [] }
    var lengths: [Int] = []
    var length = blockSize
    while length < tokens.count {
        lengths.append(length)
        length += blockSize
    }
    lengths.append(tokens.count)
    return lengths.map { ($0, tokenHash(tokens.prefix($0))) }
}

public func bestIndexHit(tokens: [Int], index: [String: PromptCacheIndexEntry], blockSize: Int = promptCacheBlockSize)
    -> PromptCacheIndexEntry?
{
    var lengths = Set(prefixHashes(tokens, blockSize: blockSize).map(\.length))
    for entry in index.values where entry.length <= tokens.count {
        lengths.insert(entry.length)
    }
    return lengths
        .sorted(by: >)
        .compactMap { length in index[tokenHash(tokens.prefix(length))] }
        .first
}

public func indexEntries(for tokens: [Int], file: String, blockSize: Int = promptCacheBlockSize) -> [String: PromptCacheIndexEntry] {
    var entries: [String: PromptCacheIndexEntry] = [:]
    for record in prefixHashes(tokens, blockSize: blockSize) {
        entries[record.hash] = PromptCacheIndexEntry(file: file, length: record.length)
    }
    return entries
}

public func sanitizeCacheComponent(_ value: String) -> String {
    let filtered = value.unicodeScalars.map { CharacterSet.alphanumerics.contains($0) || $0 == "." || $0 == "_" || $0 == "-" ? Character($0) : "-" }
    let joined = String(filtered)
    if joined.isEmpty || joined.count > 128 {
        return tokenHash(Array(value.utf8).map(Int.init))
    }
    return joined
}

public func resolveSlotURL(root: URL, filename: String) -> URL? {
    guard !filename.contains("\0"), !filename.contains("..") else { return nil }
    let name = URL(fileURLWithPath: filename).lastPathComponent
    guard !name.isEmpty, name != ".", name != ".." else { return nil }
    return root.appending(path: name)
}

public func parsePromptTurns(_ payload: [String: Any]) -> (turns: [PromptTurn], images: [String], audios: [String], videos: [String]) {
    var turns: [PromptTurn] = []
    var images: [String] = []
    var audios: [String] = []
    var videos: [String] = []

    func consume(_ content: Any, role: String, toolCallId: String?) {
        var text = ""
        var turnImages: [String] = []
        var turnAudios: [String] = []
        var turnVideos: [String] = []
        if let value = content as? String {
            text = value
        } else if let blocks = content as? [[String: Any]] {
            for block in blocks {
                let type = block["type"] as? String
                if let value = block["text"] as? String { text += value }
                if type == "image_url" || type == "input_image" {
                    if let url = (block["image_url"] as? [String: Any])?["url"] as? String ?? block["image_url"] as? String {
                        turnImages.append(url)
                        images.append(url)
                    }
                }
                if type == "input_audio" {
                    if let audio = block["input_audio"] as? [String: Any], let data = audio["data"] as? String {
                        let url = "data:audio/\(audio["format"] as? String ?? "wav");base64,\(data)"
                        turnAudios.append(url)
                        audios.append(url)
                    }
                }
                if type == "input_video" {
                    if let video = block["input_video"] as? [String: Any], let url = video["url"] as? String {
                        turnVideos.append(url)
                        videos.append(url)
                    }
                }
            }
        }
        turns.append(
            PromptTurn(
                role: role,
                text: text,
                imageURLs: turnImages,
                audioURLs: turnAudios,
                videoURLs: turnVideos,
                toolCallId: toolCallId
            )
        )
    }

    if let messages = payload["messages"] as? [[String: Any]] {
        for message in messages {
            consume(
                message["content"] as Any,
                role: message["role"] as? String ?? "user",
                toolCallId: message["tool_call_id"] as? String
            )
        }
    } else if let input = payload["input"] as? String ?? payload["prompt"] as? String {
        consume(input, role: "user", toolCallId: nil)
    }
    return (turns, images, audios, videos)
}
