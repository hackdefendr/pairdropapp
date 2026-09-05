//! Port of the `cyrb53` hash from PairDrop's `public/scripts/util.js`, so the
//! connection-verification digits shown here match the ones the web client shows.
//!
//! The original operates on JavaScript numbers: `Math.imul` is a wrapping 32-bit
//! multiply and `>>>` is a logical shift, reproduced here with `u32`.

/// Hashes a string exactly as the web client does.
pub fn hash(input: &str, seed: u32) -> u64 {
    let mut h1: u32 = 0xdead_beef ^ seed;
    let mut h2: u32 = 0x41c6_ce57 ^ seed;

    // `charCodeAt` walks UTF-16 code units, not Unicode scalars — an emoji contributes
    // two iterations here just as it does in JavaScript.
    for unit in input.encode_utf16() {
        let ch = unit as u32;
        h1 = (h1 ^ ch).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ ch).wrapping_mul(1_597_334_677);
    }

    h1 = ((h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)) ^ ((h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909));
    h2 = ((h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)) ^ ((h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909));

    4_294_967_296u64 * ((2_097_151 & h2) as u64) + (h1 as u64)
}

/// The 16-digit, zero-padded form PairDrop displays for connection verification.
pub fn connection_hash(input: &str) -> String {
    format!("{:016}", hash(input, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values produced by running the original `cyrb53` in Node on the same inputs.
    /// Identical to the vectors locking down the Swift port, so all three
    /// implementations are pinned to one another.
    #[test]
    fn matches_javascript_reference() {
        let vectors: &[(&str, u64)] = &[
            ("", 3_338_908_027_751_811),
            ("a", 7_929_297_801_672_961),
            ("PairDrop", 3_259_817_742_790_581),
            (
                "sha-256 AB:CD:EF:01:23:45:67:89sha-256 98:76:54:32:10:FE:DC:BA",
                8_763_102_360_577_714,
            ),
            ("d41d8cd98f00b204e9800998ecf8427e", 6_911_763_364_504_760),
        ];

        for (input, expected) in vectors {
            assert_eq!(hash(input, 0), *expected, "cyrb53({input:?})");
        }

        let long = "0123456789".repeat(10);
        assert_eq!(hash(&long, 0), 1_336_842_503_148_492);
    }

    /// Surrogate pairs must be folded in as two code units; iterating Rust `char`s
    /// instead would silently produce a different verification code from the browser,
    /// and only for users whose device name contains an emoji.
    ///
    /// These values also come from Node, so they catch the mistake rather than merely
    /// asserting that two hashes differ.
    #[test]
    fn counts_utf16_code_units() {
        assert_eq!("🌍".encode_utf16().count(), 2);

        assert_eq!(hash("héllo 🌍", 0), 6_778_328_375_342_556);
        assert_eq!(hash("🌍", 0), 5_793_501_205_726_912);
        assert_eq!(hash("🌏", 0), 6_202_052_789_734_818);
        assert_eq!(hash("Jeffrey's MacBook Pro", 0), 8_483_168_045_389_729);
    }

    #[test]
    fn connection_hash_is_padded_to_sixteen_digits() {
        assert_eq!(connection_hash("PairDrop").len(), 16);
        assert!(connection_hash("").chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn seed_changes_the_result() {
        assert_ne!(hash("PairDrop", 0), hash("PairDrop", 1));
    }
}
