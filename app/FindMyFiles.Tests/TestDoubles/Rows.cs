using FindMyFiles.Engine;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>Compact <see cref="RowData"/> builders for tests.</summary>
internal static class Rows
{
    public static RowData File(ulong entryRef, string name, ulong size = 0, long mtime = 0) =>
        new(entryRef, Frn: entryRef | (1UL << 48), Size: size, Mtime: mtime, Flags: 0,
            Name: name, ParentPath: "F:\\t\\");

    public static RowData Dir(ulong entryRef, string name, ulong size = 0) =>
        new(entryRef, Frn: entryRef | (1UL << 48), Size: size, Mtime: 0, Flags: 1,
            Name: name, ParentPath: "F:\\t\\");

    /// <summary>count rows named "{prefix}_000000.txt" … (deterministic).</summary>
    public static List<RowData> Many(int count, string prefix = "row") =>
        [.. Enumerable.Range(0, count).Select(i =>
            File((ulong)i, $"{prefix}_{i:D6}.txt", (ulong)(i + 1)))];
}
