import Foundation
import MLXServerCore
import Testing

@Test func longestCommonPrefixMatchesSharedHead() {
    #expect(longestCommonPrefix([1, 2, 3, 4], [1, 2, 9]) == 2)
    #expect(longestCommonPrefix([1, 2], [1, 2, 3]) == 2)
    #expect(longestCommonPrefix([], [1]) == 0)
    #expect(longestCommonPrefix([4, 5], [1, 2]) == 0)
}

@Test func prefixHashesCoverFullLengthAnd256Blocks() {
    let short = Array(0..<200)
    let shortHashes = prefixHashes(short)
    #expect(shortHashes.map(\.length) == [200])

    let long = Array(0..<300)
    let longHashes = prefixHashes(long)
    #expect(longHashes.map(\.length) == [256, 300])
    #expect(Set(longHashes.map(\.hash)).count == 2)
}

@Test func blockHashIndexHitsLongestPrefixWithoutASession() {
    let saved = Array(0..<300)
    let file = "snap.safetensors"
    let index = indexEntries(for: saved, file: file)

    let nextTurn = Array(0..<300) + Array(900..<940)
    let hit = bestIndexHit(tokens: nextTurn, index: index)
    #expect(hit?.file == file)
    #expect(hit?.length == 300)

    let shortHit = bestIndexHit(tokens: Array(0..<200), index: indexEntries(for: Array(0..<200), file: "short.safetensors"))
    #expect(shortHit?.length == 200)

    let blockHit = bestIndexHit(tokens: Array(0..<400), index: indexEntries(for: Array(0..<256), file: "block.safetensors"))
    #expect(blockHit?.length == 256)

    #expect(bestIndexHit(tokens: Array(50..<80), index: index) == nil)
}

@Test func slotFilenamesRejectPathTraversal() {
    let root = URL(filePath: "/tmp/kv")
    #expect(resolveSlotURL(root: root, filename: "ok.safetensors")?.lastPathComponent == "ok.safetensors")
    #expect(resolveSlotURL(root: root, filename: "../secret") == nil)
    #expect(resolveSlotURL(root: root, filename: "a/../b") == nil)
}

@Test func parsePromptTurnsKeepsChatRoles() {
    let payload: [String: Any] = [
        "messages": [
            ["role": "system", "content": "be brief"],
            ["role": "user", "content": "hello"],
            ["role": "assistant", "content": "hi"],
            ["role": "user", "content": [["type": "text", "text": "again"]]],
        ]
    ]
    let parsed = parsePromptTurns(payload)
    #expect(parsed.turns.map(\.role) == ["system", "user", "assistant", "user"])
    #expect(parsed.turns.map(\.text) == ["be brief", "hello", "hi", "again"])
}
