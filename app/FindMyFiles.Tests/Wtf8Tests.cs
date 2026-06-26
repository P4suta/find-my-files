using System.Text;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// WTF-8 codec: like UTF-8 but lone surrogates round-trip as 3-byte sequences
/// (never U+FFFD), matching the engine's name pools. Each case pins the exact
/// bytes for a code point at a length boundary (1/2/3/4-byte) so a mutated
/// shift, mask, or comparison shifts the bytes and fails — the same structure
/// the Rust side uses against its WTF-8 fixtures.
/// </summary>
public sealed class Wtf8Tests
{
    // (utf16 string, exact WTF-8 bytes). Boundary code points sit on both
    // sides of each length threshold so a flipped `<`/`<=` shifts the bytes.
    public static TheoryData<string, byte[]> Vectors() => new()
    {
        { "A", new byte[] { 0x41 } },                          // ASCII (1-byte)
        { ((char)0x7F).ToString(), new byte[] { 0x7F } },        // max 1-byte (cp < 0x80)
        { ((char)0x80).ToString(), new byte[] { 0xC2, 0x80 } },  // min 2-byte (cp == 0x80 boundary)
        { "©", new byte[] { 0xC2, 0xA9 } },                    // © mid 2-byte
        { "߿", new byte[] { 0xDF, 0xBF } },                    // U+07FF max 2-byte
        { "ࠀ", new byte[] { 0xE0, 0xA0, 0x80 } },              // U+0800 min 3-byte
        { "省", new byte[] { 0xE7, 0x9C, 0x81 } },              // 省 mid 3-byte
        { "￿", new byte[] { 0xEF, 0xBF, 0xBF } },              // U+FFFF max 3-byte BMP
        { "𐀀", new byte[] { 0xF0, 0x90, 0x80, 0x80 } },  // U+10000 min 4-byte
        { "😀", new byte[] { 0xF0, 0x9F, 0x98, 0x80 } },  // 😀 U+1F600 4-byte
        { "􏿿", new byte[] { 0xF4, 0x8F, 0xBF, 0xBF } },  // U+10FFFF max 4-byte

        // NOTE: lone surrogates live in dedicated [Fact]s below, not here —
        // xUnit serializes Theory arguments and a lone surrogate cannot survive
        // the round-trip (it collapses to U+FFFD), which would defeat the point.
    };

    [Theory]
    [MemberData(nameof(Vectors))]
    public void Encode_ProducesTheExactBytes(string text, byte[] expected) =>
        Assert.Equal(expected, Wtf8.Encode(text));

    [Theory]
    [MemberData(nameof(Vectors))]
    public void Decode_ReconstructsTheExactString(string expected, byte[] bytes) =>
        Assert.Equal(expected, Wtf8.Decode(bytes));

    [Fact]
    public void LoneHighSurrogate_EncodesToThreeBytes_AndDecodesBack()
    {
        // U+D800 alone: WTF-8's defining case — 3 bytes, never U+FFFD.
        byte[] bytes = [0xED, 0xA0, 0x80];
        Assert.Equal(bytes, Wtf8.Encode("\uD800"));
        Assert.Equal("\uD800", Wtf8.Decode(bytes));
    }

    [Fact]
    public void LoneLowSurrogate_EncodesToThreeBytes_AndDecodesBack()
    {
        // U+DC00 alone — distinct second byte from the high-surrogate case,
        // so a dropped/inverted bit in the encode shifts it.
        byte[] bytes = [0xED, 0xB0, 0x80];
        Assert.Equal(bytes, Wtf8.Encode("\uDC00"));
        Assert.Equal("\uDC00", Wtf8.Decode(bytes));
    }

    [Fact]
    public void Empty_RoundTripsToEmpty()
    {
        Assert.Empty(Wtf8.Encode(string.Empty));
        Assert.Equal(string.Empty, Wtf8.Decode([]));
    }

    [Fact]
    public void LoneHighSurrogate_BeforeAscii_DoesNotConsumeTheNextChar()
    {
        // A high surrogate not followed by a low one must encode as its own
        // 3-byte sequence and leave the following char intact (the `i += 2`
        // pairing branch must not fire).
        Assert.Equal(new byte[] { 0xED, 0xA0, 0x80, 0x58 }, Wtf8.Encode("\uD800X"));
    }

    [Fact]
    public void HighSurrogate_FollowedByAnotherHigh_BothStayLone()
    {
        // Two high surrogates in a row: neither pairs, so each is its own
        // 3-byte sequence (six bytes total).
        Assert.Equal(
            new byte[] { 0xED, 0xA0, 0x80, 0xED, 0xA0, 0x80 },
            Wtf8.Encode("\uD800\uD800"));
    }

    [Fact]
    public void LoneHighSurrogate_AtEndOfString_Encodes()
    {
        // The pairing test reads i+1; at the last index there is no next char,
        // so the lone surrogate must still flush as a 3-byte sequence.
        Assert.Equal(new byte[] { 0x41, 0xED, 0xA0, 0x80 }, Wtf8.Encode("A\uD800"));
    }

    [Fact]
    public void MixedString_RoundTripsAcrossEveryLengthClass()
    {
        // ASCII + 2-byte + 3-byte + astral + lone surrogate, interleaved, so a
        // wrong advance count would desynchronize the whole stream.
        const string s = "a©b省c😀d\uD800e";
        var bytes = Wtf8.Encode(s);
        Assert.Equal(s, Wtf8.Decode(bytes));
    }

    [Fact]
    public void Decode_AgreesWithUtf8_ForWellFormedText()
    {
        // For text with no lone surrogates, WTF-8 is byte-identical to UTF-8 —
        // pin that against the framework encoder.
        const string s = "Files/ファイル/🗂";
        Assert.Equal(s, Wtf8.Decode(Encoding.UTF8.GetBytes(s)));
        Assert.Equal(Encoding.UTF8.GetBytes(s), Wtf8.Encode(s));
    }
}
