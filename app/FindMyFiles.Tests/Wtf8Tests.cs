using System.Text;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary> WTF-8 codec: like UTF-8 but lone surrogates round-trip as 3-byte sequences (never U+FFFD), matching the engine's name pools.
/// Each case pins the exact bytes for a code point at a length boundary (1/2/3/4-byte) so a mutated shift, mask, or comparison shifts the bytes and fails — the same structure the Rust side uses against its WTF-8 fixtures.
/// </summary>
public sealed class Wtf8Tests
{
    public static TheoryData<string, byte[]> Vectors() => new()
    {
        { "A", new byte[] { 0x41 } },
        { ((char)0x7F).ToString(), new byte[] { 0x7F } },
        { ((char)0x80).ToString(), new byte[] { 0xC2, 0x80 } },
        { "©", new byte[] { 0xC2, 0xA9 } },
        { "߿", new byte[] { 0xDF, 0xBF } },
        { "ࠀ", new byte[] { 0xE0, 0xA0, 0x80 } },
        { "省", new byte[] { 0xE7, 0x9C, 0x81 } },
        { "￿", new byte[] { 0xEF, 0xBF, 0xBF } },
        { "𐀀", new byte[] { 0xF0, 0x90, 0x80, 0x80 } },
        { "😀", new byte[] { 0xF0, 0x9F, 0x98, 0x80 } },
        { "􏿿", new byte[] { 0xF4, 0x8F, 0xBF, 0xBF } },
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
        byte[] bytes = [0xED, 0xA0, 0x80];
        Assert.Equal(bytes, Wtf8.Encode("\uD800"));
        Assert.Equal("\uD800", Wtf8.Decode(bytes));
    }

    [Fact]
    public void LoneLowSurrogate_EncodesToThreeBytes_AndDecodesBack()
    {
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
    public void Decode_RejectsAnIsolatedContinuationByte()
    {
        Assert.ThrowsAny<Exception>(() => Wtf8.Decode([0x80]));
    }

    [Fact]
    public void LoneHighSurrogate_BeforeAscii_DoesNotConsumeTheNextChar()
    {
        Assert.Equal(new byte[] { 0xED, 0xA0, 0x80, 0x58 }, Wtf8.Encode("\uD800X"));
    }

    [Fact]
    public void HighSurrogate_FollowedByAnotherHigh_BothStayLone()
    {
        Assert.Equal(
            new byte[] { 0xED, 0xA0, 0x80, 0xED, 0xA0, 0x80 },
            Wtf8.Encode("\uD800\uD800"));
    }

    [Fact]
    public void LoneHighSurrogate_AtEndOfString_Encodes()
    {
        Assert.Equal(new byte[] { 0x41, 0xED, 0xA0, 0x80 }, Wtf8.Encode("A\uD800"));
    }

    [Fact]
    public void MixedString_RoundTripsAcrossEveryLengthClass()
    {
        const string s = "a©b省c😀d\uD800e";
        var bytes = Wtf8.Encode(s);
        Assert.Equal(s, Wtf8.Decode(bytes));
    }

    [Fact]
    public void Decode_AgreesWithUtf8_ForWellFormedText()
    {
        const string s = "Files/ファイル/🗂";
        Assert.Equal(s, Wtf8.Decode(Encoding.UTF8.GetBytes(s)));
        Assert.Equal(Encoding.UTF8.GetBytes(s), Wtf8.Encode(s));
    }
}
