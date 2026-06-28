namespace FindMyFiles.Engine;

/// <summary>
/// WTF-8 decoding: like UTF-8 but unpaired surrogates round-trip, matching
/// the engine's name pools. C# strings hold unpaired surrogates natively.
/// </summary>
internal static class Wtf8
{
    /// <summary>WTF-8 decoding: the inverse of <see cref="Encode"/>. Lone
    /// surrogate byte sequences round-trip to the matching UTF-16 code units.</summary>
    /// <param name="bytes">The WTF-8 encoded bytes to decode.</param>
    /// <returns>The decoded string, with any lone surrogates preserved.</returns>
    public static string Decode(ReadOnlySpan<byte> bytes)
    {
        // UTF-16 units ≤ WTF-8 bytes, so this is a safe upper bound. Decode into
        // a stack buffer for the common short name/path (≤512 chars = 1 KiB) so
        // only the unavoidable final string allocates; the throwaway scratch
        // array is gone from the per-row page-decode path.
        Span<char> chars = bytes.Length <= 512 ? stackalloc char[bytes.Length] : new char[bytes.Length];
        int n = 0, i = 0;
        while (i < bytes.Length)
        {
            uint b0 = bytes[i];
            uint cp;
            int adv;
            if (b0 < 0x80)
            {
                cp = b0;
                adv = 1;
            }
            else if (b0 < 0xE0)
            {
                cp = ((b0 & 0x1F) << 6) | (uint)(bytes[i + 1] & 0x3F);
                adv = 2;
            }
            else if (b0 < 0xF0)
            {
                cp = ((b0 & 0x0F) << 12) | (uint)((bytes[i + 1] & 0x3F) << 6)
                     | (uint)(bytes[i + 2] & 0x3F);
                adv = 3;
            }
            else
            {
                cp = ((b0 & 0x07) << 18) | (uint)((bytes[i + 1] & 0x3F) << 12)
                     | (uint)((bytes[i + 2] & 0x3F) << 6) | (uint)(bytes[i + 3] & 0x3F);
                adv = 4;
            }

            i += adv;
            if (cp >= 0x10000)
            {
                cp -= 0x10000;
                chars[n++] = (char)(0xD800 + (cp >> 10));
                chars[n++] = (char)(0xDC00 + (cp & 0x3FF));
            }
            else
            {
                chars[n++] = (char)cp; // includes lone surrogates — intentional
            }
        }

        return new string(chars[..n]);
    }

    /// <summary>WTF-8 encoding: the inverse of <see cref="Decode"/>. Lone
    /// surrogates become 3-byte sequences instead of U+FFFD.</summary>
    /// <param name="s">The string to encode, which may contain lone surrogates.</param>
    /// <returns>The WTF-8 encoded bytes.</returns>
    public static byte[] Encode(string s)
    {
        var bytes = new byte[s.Length * 3]; // WTF-8 bytes ≤ 3 × UTF-16 units
        int n = 0, i = 0;
        while (i < s.Length)
        {
            uint cp = s[i];
            if (char.IsHighSurrogate(s[i]) && i + 1 < s.Length && char.IsLowSurrogate(s[i + 1]))
            {
                cp = (uint)char.ConvertToUtf32(s[i], s[i + 1]);
                i += 2;
            }
            else
            {
                i++; // lone surrogates fall through as 3-byte sequences
            }

            if (cp < 0x80)
            {
                bytes[n++] = (byte)cp;
            }
            else if (cp < 0x800)
            {
                bytes[n++] = (byte)(0xC0 | (cp >> 6));
                bytes[n++] = (byte)(0x80 | (cp & 0x3F));
            }
            else if (cp < 0x10000)
            {
                bytes[n++] = (byte)(0xE0 | (cp >> 12));
                bytes[n++] = (byte)(0x80 | ((cp >> 6) & 0x3F));
                bytes[n++] = (byte)(0x80 | (cp & 0x3F));
            }
            else
            {
                bytes[n++] = (byte)(0xF0 | (cp >> 18));
                bytes[n++] = (byte)(0x80 | ((cp >> 12) & 0x3F));
                bytes[n++] = (byte)(0x80 | ((cp >> 6) & 0x3F));
                bytes[n++] = (byte)(0x80 | (cp & 0x3F));
            }
        }

        return bytes[..n];
    }
}
