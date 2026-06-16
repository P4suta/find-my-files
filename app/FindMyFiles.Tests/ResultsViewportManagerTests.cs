using FindMyFiles.Controls;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Pins the pure decision helpers of <see cref="ResultsViewportManager"/> —
/// the ListView-touching members themselves need a real UI thread and are
/// covered by the fake-engine UI smoke instead.
/// </summary>
public sealed class ResultsViewportManagerTests
{
    private static ResultRow Row(ulong entryRef, long index = 0, string name = "a.txt")
    {
        var row = ResultRow.CreatePlaceholder(index);
        row.Fill(Rows.File(entryRef, name));
        return row;
    }

    [Theory]
    [InlineData(null, 10, false)] // reset origin: no restore index
    [InlineData(-1, 10, false)]
    [InlineData(0, 0, false)] // new result is empty
    [InlineData(10, 10, false)] // one past the end
    [InlineData(0, 10, true)]
    [InlineData(9, 10, true)]
    public void Restores_only_when_the_index_addresses_an_existing_row(
        int? restoreIndex, int itemCount, bool expected) =>
        Assert.Equal(
            expected, ResultsViewportManager.CanRestoreViewport(restoreIndex, itemCount));

    [Fact]
    public void Selection_is_refound_by_engine_identity_inside_the_seeded_window()
    {
        var items = new object?[] { Row(11, 0), Row(22, 1), Row(33, 2) };
        Assert.Equal(1, ResultsViewportManager.FindSelectionIndex(
            i => items[i], items.Length, 0, 2, entryRef: 22));
    }

    [Fact]
    public void Selection_restore_skips_placeholders_and_reports_missing_rows_as_null()
    {
        var items = new object?[] { ResultRow.CreatePlaceholder(0), Row(7, 1) };
        Assert.Equal(1, ResultsViewportManager.FindSelectionIndex(
            i => items[i], items.Length, 0, 1, entryRef: 7));
        Assert.Null(ResultsViewportManager.FindSelectionIndex(
            i => items[i], items.Length, 0, 1, entryRef: 99));
    }

    [Fact]
    public void Selection_restore_respects_the_window_and_the_item_count()
    {
        var items = new object?[] { Row(1, 0), Row(2, 1), Row(3, 2) };

        // Row 0 exists but sits outside the seeded window.
        Assert.Null(ResultsViewportManager.FindSelectionIndex(
            i => items[i], items.Length, firstSeededIndex: 1, lastSeededIndex: 2, entryRef: 1));

        // Window reaches past the item count: the count clamps, never throws.
        Assert.Null(ResultsViewportManager.FindSelectionIndex(
            i => items[i], itemCount: 2, firstSeededIndex: 0, lastSeededIndex: 5, entryRef: 3));
    }

    [Fact]
    public void Copyable_paths_filter_placeholders_and_non_rows_and_keep_order()
    {
        var a = Row(1, 0, "a.txt");
        var b = Row(2, 1, "b.txt");
        var selected = new object?[] { a, ResultRow.CreatePlaceholder(5), "not a row", b };
        Assert.Equal(
            new[] { a.FullPath, b.FullPath },
            ResultsViewportManager.CopyablePaths(selected));
    }

    [Fact]
    public void Copyable_paths_of_an_all_placeholder_selection_are_empty()
    {
        var selected = new object?[] { ResultRow.CreatePlaceholder(0) };
        Assert.Empty(ResultsViewportManager.CopyablePaths(selected));
    }
}
