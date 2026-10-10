/// Bounded cache that approximates LRU with two generations.
///
/// Lookups hit `current` first, then `previous` (promoting the entry). When
/// `current` fills up it becomes `previous` and the old `previous` is
/// dropped. Everything touched recently survives, memory stays below
/// `2 * capacity` entries, and every operation is O(1) with no bookkeeping
/// list to maintain.
struct TwoGenerationCache<Key: Hashable, Value> {
    private var current: [Key: Value] = [:]
    private var previous: [Key: Value] = [:]
    /// Maximum entries per generation.
    let capacity: Int

    init(capacity: Int) {
        self.capacity = max(1, capacity)
    }

    var count: Int { current.count + previous.count }

    mutating func value(for key: Key) -> Value? {
        if let value = current[key] { return value }
        if let value = previous.removeValue(forKey: key) {
            insert(value, for: key)
            return value
        }
        return nil
    }

    mutating func insert(_ value: Value, for key: Key) {
        if current.count >= capacity, current[key] == nil {
            previous = current
            current = [:]
        }
        current[key] = value
    }

    mutating func removeAll() {
        current.removeAll()
        previous.removeAll()
    }
}
