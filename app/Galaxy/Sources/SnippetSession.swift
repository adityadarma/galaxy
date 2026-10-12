import Foundation

/// Tracks snippet placeholders in document-wide UTF-16 offsets.
///
/// Edits are expected to occur in the selected current stop. The current stop
/// grows by the document delta; later stops shift by the same delta. An edit
/// elsewhere invalidates the session when the next navigation is attempted.
struct SnippetSession {
    struct Stop {
        let index: UInt32
        var location: UInt64
        var length: UInt64
    }

    var stops: [[Stop]]
    var current = 0
    var documentLength: UInt64

    var isFinished: Bool { current >= stops.count }
    var currentStop: [Stop]? { stops.indices.contains(current) ? stops[current] : nil }

    var span: (start: UInt64, end: UInt64)? {
        let all = stops.flatMap { $0 }
        guard let start = all.map(\.location).min(),
              let end = all.map({ $0.location + $0.length }).max() else { return nil }
        return (start, end)
    }

    mutating func noteDocumentChange(length: UInt64) {
        let delta = Int64(length) - Int64(documentLength)
        documentLength = length
        guard delta != 0, stops.indices.contains(current) else { return }
        for index in stops[current].indices {
            stops[current][index].length = UInt64(max(0, Int64(stops[current][index].length) + delta))
        }
        if delta != 0 {
            for index in stops.indices where index > current {
                for stop in stops[index].indices {
                    stops[index][stop].location = UInt64(max(0, Int64(stops[index][stop].location) + delta))
                }
            }
        }
    }

    mutating func move(_ direction: Int) -> [Stop]? {
        let next = current + direction
        guard stops.indices.contains(next) else {
            current = stops.count
            return nil
        }
        current = next
        return stops[current]
    }
}
