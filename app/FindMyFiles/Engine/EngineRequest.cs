using System.Runtime.InteropServices;
using System.Text;

namespace FindMyFiles.Engine;

/// <summary>
/// Checked representations of caller-controlled engine requests. Converting
/// signed managed values to the unsigned FFI/wire contract happens only here,
/// after the shared bounds have been enforced.
/// </summary>
internal static class EngineRequest
{
    [StructLayout(LayoutKind.Auto)]
    public readonly record struct Page(ulong Offset, uint Count);

    public static string QueryText(string text)
    {
        ArgumentNullException.ThrowIfNull(text);
        var byteCount = Encoding.UTF8.GetByteCount(text);
        if (byteCount > EngineContract.MaxQueryBytes)
        {
            throw new ArgumentException(
                $"A query may contain at most {EngineContract.MaxQueryBytes} UTF-8 bytes.",
                nameof(text));
        }

        return text;
    }

    public static Page PageRange(long offset, int count)
    {
        if (offset < 0)
        {
            throw new ArgumentOutOfRangeException(
                nameof(offset), offset, "A page offset cannot be negative.");
        }

        if ((uint)count > (uint)EngineContract.MaxPageRows)
        {
            throw new ArgumentOutOfRangeException(
                nameof(count),
                count,
                $"A page count must be between 0 and {EngineContract.MaxPageRows}.");
        }

        return new Page((ulong)offset, (uint)count);
    }

    public static string[] Volumes(IReadOnlyList<string> volumes)
    {
        ArgumentNullException.ThrowIfNull(volumes);
        if (volumes.Count > EngineContract.MaxVolumes)
        {
            throw new ArgumentOutOfRangeException(
                nameof(volumes),
                volumes.Count,
                $"At most {EngineContract.MaxVolumes} volumes may be indexed at once.");
        }

        var snapshot = new string[volumes.Count];
        var seen = new HashSet<string>(StringComparer.Ordinal);
        for (var i = 0; i < volumes.Count; i++)
        {
            var label = volumes[i];
            if (label is not { Length: 2 }
                || !char.IsAsciiLetter(label[0])
                || label[1] != ':')
            {
                throw new ArgumentException(
                    $"Volume {i} must be an ASCII drive label such as \"C:\".",
                    nameof(volumes));
            }

            var canonical = $"{char.ToUpperInvariant(label[0])}:";
            if (!seen.Add(canonical))
            {
                throw new ArgumentException(
                    $"Volume {canonical} appears more than once.",
                    nameof(volumes));
            }

            snapshot[i] = canonical;
        }

        return snapshot;
    }
}
