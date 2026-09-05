import Foundation

/// Port of the `cyrb53` hash used by PairDrop (public/scripts/util.js), so the
/// connection-verification digits shown here match the ones the web client shows.
///
/// The original operates on JavaScript numbers: `Math.imul` is a wrapping 32-bit
/// multiply and `>>>` is a logical shift, both reproduced here with `UInt32`.
public enum Cyrb53 {

    public static func hash(_ string: String, seed: UInt32 = 0) -> UInt64 {
        var h1: UInt32 = 0xdeadbeef ^ seed
        var h2: UInt32 = 0x41c6ce57 ^ seed

        // charCodeAt walks UTF-16 code units.
        for unit in string.utf16 {
            let ch = UInt32(unit)
            h1 = (h1 ^ ch) &* 2654435761
            h2 = (h2 ^ ch) &* 1597334677
        }

        h1 = ((h1 ^ (h1 >> 16)) &* 2246822507) ^ ((h2 ^ (h2 >> 13)) &* 3266489909)
        h2 = ((h2 ^ (h2 >> 16)) &* 2246822507) ^ ((h1 ^ (h1 >> 13)) &* 3266489909)

        return 4294967296 * UInt64(2097151 & h2) + UInt64(h1)
    }

    /// The 16-digit, zero-padded form PairDrop displays for connection verification.
    public static func connectionHash(_ string: String) -> String {
        let digits = String(hash(string))
        guard digits.count < 16 else { return digits }
        return String(repeating: "0", count: 16 - digits.count) + digits
    }
}
