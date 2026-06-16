namespace FindMyFiles.Engine;

/// <summary>One result row decoded from a page (the C# face of fmf-core's
/// 48-byte <c>FmfRow</c> plus its WTF-8 name/path strings). Immutable; the
/// UI's <c>ResultRow</c> view-model is filled from it.</summary>
/// <param name="EntryRef">Engine-internal stable handle for the entry within
/// its volume index (the identity used for refine/<c>unchanged</c>
/// comparisons) — not the NTFS reference.</param>
/// <param name="Frn">NTFS File Reference Number (record number in the low 48
/// bits, sequence in the high 16) — the identity to correlate with USN
/// records and the filesystem.</param>
/// <param name="Size">File size in bytes (0 for directories).</param>
/// <param name="Mtime">Last-modified time as a Windows <c>FILETIME</c>
/// (100 ns ticks since 1601-01-01 UTC); 0 means unknown/unset and renders as
/// an empty timestamp.</param>
/// <param name="Flags">Bit field of NTFS attributes; bit 0 is the directory
/// flag (see <see cref="IsDirectory"/>).</param>
/// <param name="Name">Leaf file or directory name.</param>
/// <param name="ParentPath">Containing directory path including its trailing
/// separator (e.g. <c>"C:\"</c>), so <see cref="FullPath"/> is a plain
/// concatenation.</param>
public sealed record RowData(
    ulong EntryRef,
    ulong Frn,
    ulong Size,
    long Mtime,
    uint Flags,
    string Name,
    string ParentPath)
{
    /// <summary>True when this row is a directory (bit 0 of
    /// <see cref="Flags"/>).</summary>
    public bool IsDirectory => (Flags & 1) != 0;

    /// <summary>The full path, <see cref="ParentPath"/> (which already ends in
    /// a separator) concatenated with <see cref="Name"/>.</summary>
    public string FullPath => ParentPath + Name;
}
