using System.Runtime.InteropServices;

namespace FindMyFiles.Highlighting;

/// <summary>
/// A half-open run of UTF-16 code units to emphasize, in the coordinate space
/// of the string handed to <see cref="CompiledHighlighter.Ranges"/>.
/// </summary>
/// <param name="Start">Zero-based UTF-16 index of the first highlighted unit.</param>
/// <param name="Length">Number of UTF-16 units highlighted (always ≥ 1).</param>
[StructLayout(LayoutKind.Auto)]
public readonly record struct HighlightRange(int Start, int Length);
