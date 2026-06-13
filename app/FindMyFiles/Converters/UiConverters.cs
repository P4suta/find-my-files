using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FindMyFiles.Services;

namespace FindMyFiles.Converters;

/// <summary>
/// Pure x:Bind function-binding converters — no IValueConverter plumbing,
/// x:Bind calls these statically and stays typed end to end.
/// </summary>
public static class UiConverters
{
    /// <summary><c>true</c> を <c>Visible</c>、<c>false</c> を <c>Collapsed</c> に写像する
    /// (反転が要る箇所はバインド側で否定する)。</summary>
    public static Visibility BoolToVis(bool value) =>
        value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>文字列が非 null かつ非空なら <c>Visible</c>、それ以外は <c>Collapsed</c>。
    /// 値がある時だけ要素を見せる(例: エラーテキスト)用途。</summary>
    public static Visibility VisibleIfNotEmpty(string? value) =>
        string.IsNullOrEmpty(value) ? Visibility.Collapsed : Visibility.Visible;

    /// <summary>App severity → InfoBar severity (the InfoBar enum is the
    /// view's vocabulary; the app's is <see cref="NotifySeverity"/>).</summary>
    public static InfoBarSeverity ToInfoSeverity(NotifySeverity s) => s switch
    {
        NotifySeverity.Error => InfoBarSeverity.Error,
        NotifySeverity.Warning => InfoBarSeverity.Warning,
        _ => InfoBarSeverity.Informational,
    };
}
