using FindMyFiles.Engine;

namespace FindMyFiles.Virtualization;

/// <summary>
/// A page of already-fetched rows handed to
/// <see cref="VirtualResultList.Reassign"/>, so the viewport is filled the
/// instant a new result is published — never a placeholder flash.
/// </summary>
/// <param name="Page">Page index (row index ÷ <see cref="VirtualResultList.PageSize"/>)
/// these rows belong to.</param>
/// <param name="Rows">The page's rows in slot order, as fetched from the engine.</param>
public readonly record struct PageSeed(long Page, IReadOnlyList<RowData> Rows);
