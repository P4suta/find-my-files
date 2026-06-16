using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;

namespace FindMyFiles.Highlighting;

/// <summary>
/// Attached behavior that renders <see cref="SourceProperty"/> into a
/// TextBlock's Inlines, emphasizing the spans named by
/// <see cref="RangesProperty"/> with bold + accent foreground. Lets the result
/// list's name/path cells show match highlights without giving up their
/// TextBlock styles, x:Phase, or virtualization: the same cell instance is
/// reused across rows, and <see cref="Rebuild"/> clears and refills it on
/// every change so a reused container never shows the previous row's text.
/// </summary>
public static class HighlightText
{
    /// <summary>The full text to display (set with <see cref="RangesProperty"/>;
    /// either change rebuilds the inlines).</summary>
    public static readonly DependencyProperty SourceProperty =
        DependencyProperty.RegisterAttached(
            "Source",
            typeof(string),
            typeof(HighlightText),
            new PropertyMetadata(null, OnChanged));

    /// <summary>Ranges of <see cref="SourceProperty"/> to emphasize (UTF-16
    /// coordinates, sorted and merged by the highlighter).</summary>
    public static readonly DependencyProperty RangesProperty =
        DependencyProperty.RegisterAttached(
            "Ranges",
            typeof(IReadOnlyList<HighlightRange>),
            typeof(HighlightText),
            new PropertyMetadata(null, OnChanged));

    /// <summary>Set the display text on <paramref name="element"/>.</summary>
    /// <param name="element">The element to set the display text on.</param>
    /// <param name="value">The text to display.</param>
    public static void SetSource(DependencyObject element, string? value)
    {
        ArgumentNullException.ThrowIfNull(element);
        element.SetValue(SourceProperty, value);
    }

    /// <summary>Get the display text from <paramref name="element"/>.</summary>
    /// <param name="element">The element to read the display text from.</param>
    /// <returns>The display text, or null if none is set.</returns>
    public static string? GetSource(DependencyObject element)
    {
        ArgumentNullException.ThrowIfNull(element);
        return (string?)element.GetValue(SourceProperty);
    }

    /// <summary>Set the highlight ranges on <paramref name="element"/>.</summary>
    /// <param name="element">The element to set the highlight ranges on.</param>
    /// <param name="value">The ranges to emphasize.</param>
    public static void SetRanges(DependencyObject element, IReadOnlyList<HighlightRange>? value)
    {
        ArgumentNullException.ThrowIfNull(element);
        element.SetValue(RangesProperty, value);
    }

    /// <summary>Get the highlight ranges from <paramref name="element"/>.</summary>
    /// <param name="element">The element to read the highlight ranges from.</param>
    /// <returns>The highlight ranges, or null if none are set.</returns>
    public static IReadOnlyList<HighlightRange>? GetRanges(DependencyObject element)
    {
        ArgumentNullException.ThrowIfNull(element);
        return (IReadOnlyList<HighlightRange>?)element.GetValue(RangesProperty);
    }

    private static void OnChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is TextBlock tb)
        {
            Rebuild(tb, GetSource(tb) ?? string.Empty, GetRanges(tb));
        }
    }

    /// <summary>Clear and refill <paramref name="tb"/>'s inlines: a single Run
    /// when there is nothing to highlight, otherwise alternating plain and
    /// emphasized (bold + accent) Runs.</summary>
    private static void Rebuild(TextBlock tb, string text, IReadOnlyList<HighlightRange>? ranges)
    {
        tb.Inlines.Clear();
        if (ranges is null || ranges.Count == 0)
        {
            tb.Inlines.Add(new Run { Text = text });
            return;
        }

        var accent = AccentBrush();
        foreach (var seg in HighlightSegmenter.Split(text, ranges))
        {
            var run = new Run { Text = seg.Text };
            if (seg.Highlighted)
            {
                run.FontWeight = FontWeights.SemiBold;
                if (accent is not null)
                {
                    run.Foreground = accent;
                }
            }

            tb.Inlines.Add(run);
        }
    }

    /// <summary>The accent foreground for matched text, resolved from the app
    /// theme resources. Null (bold only) when unavailable — highlighting must
    /// never throw, and bold alone still marks the match.</summary>
    private static Brush? AccentBrush() =>
        Application.Current.Resources.TryGetValue("AccentTextFillColorPrimaryBrush", out var value)
            && value is Brush brush
            ? brush
            : null;
}
